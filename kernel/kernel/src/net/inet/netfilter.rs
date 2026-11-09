//! Legacy iptables / ip6tables tables (x_tables), stored and never
//! enforced.
//!
//! libiptc talks to the kernel with socket options on an AF_INET (SOL_IP)
//! or AF_INET6 (SOL_IPV6) socket: IPT_SO_GET_INFO / GET_ENTRIES read a
//! table's rule blob, IPT_SO_SET_REPLACE swaps in a new one (the old
//! counters are copied back), IPT_SO_SET_ADD_COUNTERS adds counters, and
//! IPT_SO_GET_REVISION_MATCH / TARGET ask whether an extension exists.
//! Every extension is reported present; blobs are kept as given. The
//! tables start with their built-in chains holding one ACCEPT policy rule
//! each, like a freshly loaded ip_tables module.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;

use crate::errno::*;

pub const SOL_IP: i32 = 0;
pub const SOL_IPV6: i32 = 41;
const SO_SET_REPLACE: i32 = 64;
const SO_SET_ADD_COUNTERS: i32 = 65;
const SO_GET_INFO: i32 = 64;
const SO_GET_ENTRIES: i32 = 65;
const SO_GET_REVISION_MATCH: i32 = 66;
const SO_GET_REVISION_TARGET: i32 = 67;

const TABLE_NAME: usize = 32;
const NF_HOOKS: usize = 5;
/// sizeof(struct ipt_entry) / (struct ip6t_entry).
const ENTRY4: usize = 112;
const ENTRY6: usize = 168;
/// sizeof(struct ipt_getinfo), offsetof(ipt_get_entries, entrytable),
/// offsetof(ipt_replace, entries) (the same for IPv6).
const GETINFO_LEN: usize = 84;
const GET_ENTRIES_HDR: usize = 40;
const REPLACE_HDR: usize = 96;
const MAX_BLOB: usize = 16 << 20;

#[derive(Clone)]
struct Table {
    valid_hooks: u32,
    hook_entry: [u32; NF_HOOKS],
    underflow: [u32; NF_HOOKS],
    num_entries: u32,
    blob: Vec<u8>,
}

static TABLES: Mutex<BTreeMap<(bool, String), Table>> = Mutex::new(BTreeMap::new());

/// (name, valid hooks) of the tables an x_tables kernel offers.
const BUILTIN: &[(&str, u32)] = &[("filter", 0b01110), ("nat", 0b11011), ("mangle", 0b11111), ("raw", 0b01001), ("security", 0b01110)];

pub fn is_netfilter_opt(level: i32, name: i32) -> bool {
    (level == SOL_IP || level == SOL_IPV6) && (64..=67).contains(&name)
}

fn entry(v6: bool, target: &[u8]) -> Vec<u8> {
    let esz = if v6 { ENTRY6 } else { ENTRY4 };
    let off = if v6 { 140 } else { 88 };
    let mut e = vec![0u8; esz];
    e[off..off + 2].copy_from_slice(&(esz as u16).to_le_bytes());
    e[off + 2..off + 4].copy_from_slice(&((esz + target.len()) as u16).to_le_bytes());
    e.extend_from_slice(target);
    e
}

/// An xt_entry_target header (+ data), padded to 8 bytes.
fn target(name: &str, data: &[u8]) -> Vec<u8> {
    let len = (32 + data.len()).next_multiple_of(8);
    let mut t = vec![0u8; len];
    t[0..2].copy_from_slice(&(len as u16).to_le_bytes());
    t[2..2 + name.len()].copy_from_slice(name.as_bytes());
    t[32..32 + data.len()].copy_from_slice(data);
    t
}

fn initial(v6: bool, valid_hooks: u32) -> Table {
    let mut t = Table { valid_hooks, hook_entry: [0; NF_HOOKS], underflow: [0; NF_HOOKS], num_entries: 0, blob: Vec::new() };
    // The standard target's verdict: -NF_ACCEPT - 1.
    let accept = target("", &(-2i32).to_le_bytes());
    for h in 0..NF_HOOKS {
        if valid_hooks & 1 << h != 0 {
            t.hook_entry[h] = t.blob.len() as u32;
            t.underflow[h] = t.blob.len() as u32;
            t.blob.extend_from_slice(&entry(v6, &accept));
            t.num_entries += 1;
        }
    }
    let mut err = [0u8; 32];
    err[..5].copy_from_slice(b"ERROR");
    t.blob.extend_from_slice(&entry(v6, &target("ERROR", &err)));
    t.num_entries += 1;
    t
}

fn name_of(b: &[u8]) -> String {
    let n = &b[..TABLE_NAME.min(b.len())];
    let end = n.iter().position(|&c| c == 0).unwrap_or(n.len());
    String::from_utf8_lossy(&n[..end]).into_owned()
}

fn lookup(v6: bool, name: &str) -> KResult<Table> {
    let mut t = TABLES.lock();
    if let Some(x) = t.get(&(v6, String::from(name))) {
        return Ok(x.clone());
    }
    let &(_, hooks) = BUILTIN.iter().find(|(n, _)| *n == name).ok_or(ENOENT)?;
    let x = initial(v6, hooks);
    t.insert((v6, String::from(name)), x.clone());
    Ok(x)
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

/// setsockopt: IPT_SO_SET_REPLACE / SET_ADD_COUNTERS.
pub fn setsockopt(v6: bool, name: i32, b: &[u8]) -> KResult<()> {
    match name {
        SO_SET_REPLACE => {
            if b.len() < REPLACE_HDR {
                return Err(EINVAL);
            }
            let tname = name_of(b);
            let old = lookup(v6, &tname)?;
            let valid_hooks = u32_at(b, 32);
            let num_entries = u32_at(b, 36);
            let size = u32_at(b, 40) as usize;
            if size > MAX_BLOB || b.len() < REPLACE_HDR + size {
                return Err(EINVAL);
            }
            let mut hook_entry = [0; NF_HOOKS];
            let mut underflow = [0; NF_HOOKS];
            for h in 0..NF_HOOKS {
                hook_entry[h] = u32_at(b, 44 + 4 * h);
                underflow[h] = u32_at(b, 64 + 4 * h);
            }
            if valid_hooks != old.valid_hooks {
                return Err(EINVAL);
            }
            let num_counters = u32_at(b, 84) as u64;
            let counters = u64::from_le_bytes(b[88..96].try_into().unwrap());
            if counters != 0 && num_counters != 0 {
                // The old rules' counters (never counted: all zero).
                let zero = vec![0u8; (num_counters.min(1 << 20) * 16) as usize];
                crate::process::current().mm().copy_out(counters, &zero)?;
            }
            let t = Table { valid_hooks, hook_entry, underflow, num_entries, blob: b[REPLACE_HDR..REPLACE_HDR + size].to_vec() };
            TABLES.lock().insert((v6, tname), t);
            Ok(())
        }
        SO_SET_ADD_COUNTERS => {
            if b.len() < GET_ENTRIES_HDR {
                return Err(EINVAL);
            }
            lookup(v6, &name_of(b)).map(|_| ())
        }
        _ => Err(ENOPROTOOPT),
    }
}

/// getsockopt: `input` is the caller's buffer (these requests carry a
/// table or extension name in it). Returns (data, syscall result).
pub fn getsockopt(v6: bool, name: i32, input: &[u8]) -> KResult<(Vec<u8>, u64)> {
    match name {
        SO_GET_INFO => {
            if input.len() < GETINFO_LEN {
                return Err(EINVAL);
            }
            let t = lookup(v6, &name_of(input))?;
            let mut o = input[..GETINFO_LEN].to_vec();
            o[32..36].copy_from_slice(&t.valid_hooks.to_le_bytes());
            for h in 0..NF_HOOKS {
                o[36 + 4 * h..40 + 4 * h].copy_from_slice(&t.hook_entry[h].to_le_bytes());
                o[56 + 4 * h..60 + 4 * h].copy_from_slice(&t.underflow[h].to_le_bytes());
            }
            o[76..80].copy_from_slice(&t.num_entries.to_le_bytes());
            o[80..84].copy_from_slice(&(t.blob.len() as u32).to_le_bytes());
            Ok((o, 0))
        }
        SO_GET_ENTRIES => {
            if input.len() < GET_ENTRIES_HDR {
                return Err(EINVAL);
            }
            let t = lookup(v6, &name_of(input))?;
            let size = u32_at(input, 32) as usize;
            if size != t.blob.len() || input.len() < GET_ENTRIES_HDR + size {
                return Err(EINVAL);
            }
            let mut o = input[..GET_ENTRIES_HDR].to_vec();
            o.extend_from_slice(&t.blob);
            Ok((o, 0))
        }
        SO_GET_REVISION_MATCH | SO_GET_REVISION_TARGET => {
            if input.len() < 30 {
                return Err(EINVAL);
            }
            // Every extension exists in the requested revision.
            Ok((input[..30].to_vec(), input[29] as u64))
        }
        _ => Err(ENOPROTOOPT),
    }
}

/// /proc/net/ip_tables_names, ip6_tables_names.
pub fn table_names(v6: bool) -> String {
    let t = TABLES.lock();
    let mut s = String::new();
    for (k6, name) in t.keys() {
        if *k6 == v6 {
            s.push_str(name);
            s.push('\n');
        }
    }
    s
}
