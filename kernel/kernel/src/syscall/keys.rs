//! Kernel key retention service (keyctl, add_key, request_key), minimal:
//! enough for init's session keyring and fscrypt's "fscrypt" keyring.
//!
//! One global table of keys (serial -> type, description, payload, perm)
//! and keyrings (lists of serials). The special keyrings (thread, process,
//! session, user, user-session, group) are single system-wide keyrings
//! created on first use; JOIN_SESSION_KEYRING returns the (named) session
//! keyring. There are no permission checks, quotas or expiry; payloads of
//! "logon" keys cannot be read back (EOPNOTSUPP), as on Linux.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use crate::errno::*;
use crate::process::current;

pub const KEY_SPEC_THREAD_KEYRING: i32 = -1;
pub const KEY_SPEC_REQKEY_AUTH_KEY: i32 = -7;
pub const KEY_SPEC_REQUESTOR_KEYRING: i32 = -8;

const KEYCTL_GET_KEYRING_ID: u32 = 0;
const KEYCTL_JOIN_SESSION_KEYRING: u32 = 1;
const KEYCTL_UPDATE: u32 = 2;
const KEYCTL_REVOKE: u32 = 3;
const KEYCTL_CHOWN: u32 = 4;
const KEYCTL_SETPERM: u32 = 5;
const KEYCTL_DESCRIBE: u32 = 6;
const KEYCTL_CLEAR: u32 = 7;
const KEYCTL_LINK: u32 = 8;
const KEYCTL_UNLINK: u32 = 9;
const KEYCTL_SEARCH: u32 = 10;
const KEYCTL_READ: u32 = 11;
const KEYCTL_INVALIDATE: u32 = 21;
const KEYCTL_SET_REQKEY_KEYRING: u32 = 14;
const KEYCTL_SET_TIMEOUT: u32 = 15;
const KEYCTL_GET_SECURITY: u32 = 17;
const KEYCTL_SESSION_TO_PARENT: u32 = 18;
const KEYCTL_GET_PERSISTENT: u32 = 22;
const KEYCTL_RESTRICT_KEYRING: u32 = 29;

const DEFAULT_PERM: u32 = 0x3f01_0000;

struct Key {
    ty: String,
    desc: String,
    payload: Vec<u8>,
    perm: u32,
    uid: u32,
    gid: u32,
    /// Keyrings: linked keys.
    links: Vec<i32>,
    revoked: bool,
}

struct Keys {
    next: i32,
    keys: BTreeMap<i32, Key>,
    /// Special keyring id (-1..-6) -> serial.
    special: BTreeMap<i32, i32>,
}

static KEYS: Mutex<Keys> = Mutex::new(Keys { next: 0x1000_0001, keys: BTreeMap::new(), special: BTreeMap::new() });

impl Keys {
    fn new_key(&mut self, ty: &str, desc: &str, payload: Vec<u8>) -> i32 {
        let id = self.next;
        self.next += 1;
        self.keys.insert(
            id,
            Key { ty: ty.into(), desc: desc.into(), payload, perm: DEFAULT_PERM, uid: 0, gid: 0, links: Vec::new(), revoked: false },
        );
        id
    }

    /// A key serial or special keyring id -> serial (`create` makes the
    /// special keyring if needed).
    fn resolve(&mut self, id: i32, create: bool) -> KResult<i32> {
        if id > 0 {
            return match self.keys.get(&id) {
                Some(k) if !k.revoked => Ok(id),
                Some(_) => Err(EKEYREVOKED),
                None => Err(ENOKEY),
            };
        }
        if !(KEY_SPEC_REQUESTOR_KEYRING..=KEY_SPEC_THREAD_KEYRING).contains(&id) || id == KEY_SPEC_REQKEY_AUTH_KEY {
            return Err(EINVAL);
        }
        if let Some(&s) = self.special.get(&id) {
            return Ok(s);
        }
        if !create && id != -3 && id != -4 && id != -5 {
            return Err(ENOKEY);
        }
        let name = match id {
            -1 => "_tid",
            -2 => "_pid",
            -3 => "_ses",
            -4 => "_uid.0",
            -5 => "_uid_ses.0",
            _ => "_gid",
        };
        let s = self.new_key("keyring", name, Vec::new());
        self.special.insert(id, s);
        Ok(s)
    }

    fn keyring(&mut self, id: i32, create: bool) -> KResult<i32> {
        let s = self.resolve(id, create)?;
        if self.keys[&s].ty != "keyring" {
            return Err(ENOTDIR);
        }
        Ok(s)
    }

    fn link(&mut self, key: i32, ring: i32) {
        let r = self.keys.get_mut(&ring).unwrap();
        if !r.links.contains(&key) {
            r.links.push(key);
        }
    }

    /// Depth-first search of `ring` for (type, description).
    fn search(&self, ring: i32, ty: &str, desc: &str, depth: u32) -> Option<i32> {
        let r = self.keys.get(&ring)?;
        for &k in &r.links {
            if let Some(key) = self.keys.get(&k)
                && !key.revoked
                && key.ty == ty
                && key.desc == desc
            {
                return Some(k);
            }
        }
        if depth < 6 {
            for &k in &r.links {
                if self.keys.get(&k).is_some_and(|key| key.ty == "keyring")
                    && let Some(f) = self.search(k, ty, desc, depth + 1)
                {
                    return Some(f);
                }
            }
        }
        None
    }
}

fn read_str(ptr: u64) -> KResult<String> {
    let b = current().mm().read_cstr(ptr, 4096)?;
    String::from_utf8(b).map_err(|_| EINVAL)
}

fn read_payload(ptr: u64, len: u64) -> KResult<Vec<u8>> {
    if len > 1 << 20 {
        return Err(E2BIG);
    }
    let mut v = alloc::vec![0u8; len as usize];
    if len > 0 {
        current().mm().copy_in(ptr, &mut v)?;
    }
    Ok(v)
}

/// Copy `data` to a user buffer of `len`; returns the full size.
fn out(buf: u64, len: u64, data: &[u8]) -> KResult<u64> {
    if buf != 0 && len > 0 {
        let n = (len as usize).min(data.len());
        current().mm().copy_out(buf, &data[..n])?;
    }
    Ok(data.len() as u64)
}

pub fn sys_add_key(ty: u64, desc: u64, payload: u64, plen: u64, ring: i32) -> KResult<u64> {
    let ty = read_str(ty)?;
    if ty.is_empty() || ty.starts_with('.') {
        return Err(EPERM);
    }
    let desc = if desc == 0 { return Err(EINVAL) } else { read_str(desc)? };
    let data = read_payload(payload, plen)?;
    if ty != "keyring" && !matches!(ty.as_str(), "user" | "logon" | "big_key" | "fscrypt-provisioning" | "asymmetric") {
        return Err(ENODEV);
    }
    let mut k = KEYS.lock();
    let ring = k.keyring(ring, true)?;
    if let Some(existing) = k.search(ring, &ty, &desc, 6) {
        if ty != "keyring" {
            k.keys.get_mut(&existing).unwrap().payload = data;
        }
        return Ok(existing as u64);
    }
    let id = k.new_key(&ty, &desc, if ty == "keyring" { Vec::new() } else { data });
    k.link(id, ring);
    Ok(id as u64)
}

pub fn sys_request_key(ty: u64, desc: u64, _info: u64, ring: i32) -> KResult<u64> {
    let ty = read_str(ty)?;
    let desc = read_str(desc)?;
    let mut k = KEYS.lock();
    let session = k.resolve(-3, true)?;
    let found = k.search(session, &ty, &desc, 0).or_else(|| {
        let user = k.resolve(-4, true).ok()?;
        k.search(user, &ty, &desc, 0)
    });
    let id = found.ok_or(ENOKEY)?;
    if ring != 0
        && let Ok(r) = k.keyring(ring, true)
    {
        k.link(id, r);
    }
    Ok(id as u64)
}

pub fn sys_keyctl(op: u32, a2: u64, a3: u64, a4: u64, a5: u64) -> KResult<u64> {
    let id = a2 as i32;
    let mut k = KEYS.lock();
    match op {
        KEYCTL_GET_KEYRING_ID => Ok(k.resolve(id, a3 != 0)? as u64),
        KEYCTL_JOIN_SESSION_KEYRING => {
            if a2 == 0 {
                return Ok(k.resolve(-3, true)? as u64);
            }
            drop(k);
            let name = read_str(a2)?;
            let mut k = KEYS.lock();
            let s = k.new_key("keyring", &name, Vec::new());
            k.special.insert(-3, s);
            Ok(s as u64)
        }
        KEYCTL_UPDATE => {
            let s = k.resolve(id, false)?;
            drop(k);
            let data = read_payload(a3, a4)?;
            let mut k = KEYS.lock();
            let key = k.keys.get_mut(&s).ok_or(ENOKEY)?;
            if key.ty == "keyring" {
                return Err(EOPNOTSUPP);
            }
            key.payload = data;
            Ok(0)
        }
        KEYCTL_REVOKE | KEYCTL_INVALIDATE => {
            let s = k.resolve(id, false)?;
            k.keys.get_mut(&s).unwrap().revoked = true;
            Ok(0)
        }
        KEYCTL_CHOWN => {
            let s = k.resolve(id, false)?;
            let key = k.keys.get_mut(&s).unwrap();
            if a3 as u32 != u32::MAX {
                key.uid = a3 as u32;
            }
            if a4 as u32 != u32::MAX {
                key.gid = a4 as u32;
            }
            Ok(0)
        }
        KEYCTL_SETPERM => {
            let s = k.resolve(id, false)?;
            k.keys.get_mut(&s).unwrap().perm = a3 as u32;
            Ok(0)
        }
        KEYCTL_DESCRIBE => {
            let s = k.resolve(id, false)?;
            let key = &k.keys[&s];
            let mut d = format!("{};{};{};{:08x};{}", key.ty, key.uid, key.gid, key.perm, key.desc).into_bytes();
            d.push(0);
            drop(k);
            out(a3, a4, &d)
        }
        KEYCTL_CLEAR => {
            let s = k.keyring(id, false)?;
            k.keys.get_mut(&s).unwrap().links.clear();
            Ok(0)
        }
        KEYCTL_LINK => {
            let key = k.resolve(id, false)?;
            let ring = k.keyring(a3 as i32, true)?;
            k.link(key, ring);
            Ok(0)
        }
        KEYCTL_UNLINK => {
            let key = k.resolve(id, false)?;
            let ring = k.keyring(a3 as i32, false)?;
            let r = k.keys.get_mut(&ring).unwrap();
            let before = r.links.len();
            r.links.retain(|&x| x != key);
            if r.links.len() == before { Err(ENOENT) } else { Ok(0) }
        }
        KEYCTL_SEARCH => {
            let ring = k.keyring(id, false)?;
            drop(k);
            let (ty, desc) = (read_str(a3)?, read_str(a4)?);
            let mut k = KEYS.lock();
            let found = k.search(ring, &ty, &desc, 0).ok_or(ENOKEY)?;
            if a5 as i32 != 0
                && let Ok(dest) = k.keyring(a5 as i32, true)
            {
                k.link(found, dest);
            }
            Ok(found as u64)
        }
        KEYCTL_READ => {
            let s = k.resolve(id, false)?;
            let key = &k.keys[&s];
            let data = match key.ty.as_str() {
                "keyring" => key.links.iter().flat_map(|l| l.to_le_bytes()).collect(),
                "logon" => return Err(EOPNOTSUPP),
                _ => key.payload.clone(),
            };
            drop(k);
            out(a3, a4, &data)
        }
        KEYCTL_GET_SECURITY => {
            k.resolve(id, false)?;
            drop(k);
            let mut c = current().process.sec.lock().current.clone();
            c.push(0);
            out(a3, a4, &c)
        }
        KEYCTL_GET_PERSISTENT => Ok(k.resolve(-4, true)? as u64),
        KEYCTL_SET_REQKEY_KEYRING => Ok(0),
        KEYCTL_SET_TIMEOUT => {
            k.resolve(id, false)?;
            Ok(0)
        }
        KEYCTL_SESSION_TO_PARENT => Ok(0),
        KEYCTL_RESTRICT_KEYRING => {
            k.keyring(id, false)?;
            Ok(0)
        }
        _ => Err(EOPNOTSUPP),
    }
}
