//! bpf map storage: what BPF_MAP_LOOKUP/UPDATE/DELETE_ELEM and
//! GET_NEXT_KEY see from user space. No program runs in this kernel, so
//! the contents only change through the syscall.
//!
//! - arrays (ARRAY, PERCPU_ARRAY): a shmem object of `max_entries`
//!   elements of `round_up(value_size, 8)` bytes (per-CPU: that times the
//!   number of CPUs), so a BPF_F_MMAPABLE array can be mmapped and shares
//!   its pages with lookup/update;
//! - hashes (HASH, PERCPU_HASH, LRU_HASH, LRU_PERCPU_HASH and the other
//!   key/value types: map-in-map, devmap, sockmap, local storage, ...): an
//!   ordered map of keys; full LRU maps evict their least recently used
//!   key, full hashes say E2BIG;
//! - LPM_TRIE: like a hash keyed by (prefix length, masked prefix);
//!   lookups find the longest matching prefix;
//! - fd arrays (PROG_ARRAY, PERF_EVENT_ARRAY, CGROUP_ARRAY,
//!   ARRAY_OF_MAPS, DEVMAP, CPUMAP, XSKMAP): sparse arrays (keys below
//!   `max_entries`, deletable);
//! - RINGBUF: no elements; a shmem object laid out like Linux's mapping
//!   (consumer page, producer page, data) that stays empty.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use crate::errno::*;
use crate::mm::PAGE_SIZE;
use crate::mm::shmem::Shmem;

pub const BPF_MAP_TYPE_HASH: u32 = 1;
pub const BPF_MAP_TYPE_ARRAY: u32 = 2;
pub const BPF_MAP_TYPE_PROG_ARRAY: u32 = 3;
pub const BPF_MAP_TYPE_PERF_EVENT_ARRAY: u32 = 4;
pub const BPF_MAP_TYPE_PERCPU_HASH: u32 = 5;
pub const BPF_MAP_TYPE_PERCPU_ARRAY: u32 = 6;
pub const BPF_MAP_TYPE_STACK_TRACE: u32 = 7;
pub const BPF_MAP_TYPE_CGROUP_ARRAY: u32 = 8;
pub const BPF_MAP_TYPE_LRU_HASH: u32 = 9;
pub const BPF_MAP_TYPE_LRU_PERCPU_HASH: u32 = 10;
pub const BPF_MAP_TYPE_LPM_TRIE: u32 = 11;
pub const BPF_MAP_TYPE_ARRAY_OF_MAPS: u32 = 12;
pub const BPF_MAP_TYPE_HASH_OF_MAPS: u32 = 13;
pub const BPF_MAP_TYPE_DEVMAP: u32 = 14;
pub const BPF_MAP_TYPE_SOCKMAP: u32 = 15;
pub const BPF_MAP_TYPE_CPUMAP: u32 = 16;
pub const BPF_MAP_TYPE_XSKMAP: u32 = 17;
pub const BPF_MAP_TYPE_SOCKHASH: u32 = 18;
pub const BPF_MAP_TYPE_CGROUP_STORAGE: u32 = 19;
pub const BPF_MAP_TYPE_PERCPU_CGROUP_STORAGE: u32 = 21;
pub const BPF_MAP_TYPE_SK_STORAGE: u32 = 24;
pub const BPF_MAP_TYPE_DEVMAP_HASH: u32 = 25;
pub const BPF_MAP_TYPE_RINGBUF: u32 = 27;
pub const BPF_MAP_TYPE_INODE_STORAGE: u32 = 28;
pub const BPF_MAP_TYPE_TASK_STORAGE: u32 = 29;
pub const BPF_MAP_TYPE_CGRP_STORAGE: u32 = 32;

pub const BPF_NOEXIST: u64 = 1;
pub const BPF_EXIST: u64 = 2;
pub const BPF_F_LOCK: u64 = 4;

pub const BPF_F_NO_PREALLOC: u32 = 1 << 0;
pub const BPF_F_NO_COMMON_LRU: u32 = 1 << 1;
pub const BPF_F_NUMA_NODE: u32 = 1 << 2;
pub const BPF_F_RDONLY: u32 = 1 << 3;
pub const BPF_F_WRONLY: u32 = 1 << 4;
pub const BPF_F_ZERO_SEED: u32 = 1 << 6;
pub const BPF_F_RDONLY_PROG: u32 = 1 << 7;
pub const BPF_F_WRONLY_PROG: u32 = 1 << 8;
pub const BPF_F_CLONE: u32 = 1 << 9;
pub const BPF_F_MMAPABLE: u32 = 1 << 10;
pub const BPF_F_INNER_MAP: u32 = 1 << 12;
/// Flags MAP_CREATE accepts (others: EINVAL).
pub const MAP_CREATE_FLAGS: u32 = BPF_F_NO_PREALLOC
    | BPF_F_NO_COMMON_LRU
    | BPF_F_NUMA_NODE
    | BPF_F_RDONLY
    | BPF_F_WRONLY
    | (1 << 5) // BPF_F_STACK_BUILD_ID
    | BPF_F_ZERO_SEED
    | BPF_F_RDONLY_PROG
    | BPF_F_WRONLY_PROG
    | BPF_F_CLONE
    | BPF_F_MMAPABLE
    | (1 << 11) // BPF_F_PRESERVE_ELEMS
    | BPF_F_INNER_MAP;

fn round8(n: u32) -> u64 {
    (n as u64).div_ceil(8) * 8
}

fn is_percpu(ty: u32) -> bool {
    matches!(ty, BPF_MAP_TYPE_PERCPU_HASH | BPF_MAP_TYPE_PERCPU_ARRAY | BPF_MAP_TYPE_LRU_PERCPU_HASH | BPF_MAP_TYPE_PERCPU_CGROUP_STORAGE)
}

fn is_storage(ty: u32) -> bool {
    matches!(ty, BPF_MAP_TYPE_SK_STORAGE | BPF_MAP_TYPE_INODE_STORAGE | BPF_MAP_TYPE_TASK_STORAGE | BPF_MAP_TYPE_CGRP_STORAGE)
}

/// Number of CPUs per-CPU values are given for (/sys/devices/system/cpu/possible).
pub fn possible_cpus() -> u64 {
    crate::arch::x86_64::smp::online_cpus().max(1) as u64
}

enum Store {
    Array(Arc<Shmem>),
    /// key -> (value, last use).
    Hash(BTreeMap<Vec<u8>, (Vec<u8>, u64)>),
    Ringbuf(Arc<Shmem>),
}

pub struct MapData {
    ty: u32,
    key_size: u32,
    max_entries: u32,
    /// Bytes of one element as user space reads and writes it.
    uvalue: usize,
    /// Distance between array elements in the shmem object.
    stride: u64,
    store: Store,
    clock: u64,
}

impl MapData {
    /// Validate the MAP_CREATE parameters and allocate empty storage.
    pub fn new(ty: u32, key_size: u32, value_size: u32, max_entries: u32, flags: u32) -> KResult<MapData> {
        if flags & !MAP_CREATE_FLAGS != 0 || flags & BPF_F_RDONLY != 0 && flags & BPF_F_WRONLY != 0 {
            return Err(EINVAL);
        }
        let percpu = is_percpu(ty);
        let uvalue = if percpu { round8(value_size) * possible_cpus() } else { value_size as u64 };
        let mut stride = round8(value_size);
        let store = match ty {
            BPF_MAP_TYPE_ARRAY | BPF_MAP_TYPE_PERCPU_ARRAY => {
                if key_size != 4 || value_size == 0 || max_entries == 0 || value_size > (1 << 20) {
                    return Err(EINVAL);
                }
                if flags & BPF_F_MMAPABLE != 0 && percpu {
                    return Err(EINVAL);
                }
                if percpu {
                    stride = uvalue;
                }
                let size = (stride * max_entries as u64).div_ceil(PAGE_SIZE) * PAGE_SIZE;
                if size > (1 << 32) {
                    return Err(ENOMEM);
                }
                Store::Array(Shmem::new(size))
            }
            BPF_MAP_TYPE_RINGBUF => {
                // max_entries is the data size: a power of 2, pages.
                if key_size != 0 || value_size != 0 || !max_entries.is_power_of_two() || (max_entries as u64) < PAGE_SIZE {
                    return Err(EINVAL);
                }
                Store::Ringbuf(Shmem::new(2 * PAGE_SIZE + 2 * max_entries as u64))
            }
            BPF_MAP_TYPE_PROG_ARRAY
            | BPF_MAP_TYPE_PERF_EVENT_ARRAY
            | BPF_MAP_TYPE_CGROUP_ARRAY
            | BPF_MAP_TYPE_ARRAY_OF_MAPS
            | BPF_MAP_TYPE_DEVMAP
            | BPF_MAP_TYPE_CPUMAP
            | BPF_MAP_TYPE_XSKMAP => {
                if key_size != 4 || value_size == 0 || max_entries == 0 {
                    return Err(EINVAL);
                }
                Store::Hash(BTreeMap::new())
            }
            BPF_MAP_TYPE_LPM_TRIE => {
                if key_size <= 4 || key_size > 4 + 256 || value_size == 0 || max_entries == 0 || flags & BPF_F_NO_PREALLOC == 0 {
                    return Err(EINVAL);
                }
                Store::Hash(BTreeMap::new())
            }
            t if is_storage(t) => {
                if key_size != 4 || value_size == 0 || max_entries != 0 || flags & BPF_F_NO_PREALLOC == 0 {
                    return Err(EINVAL);
                }
                Store::Hash(BTreeMap::new())
            }
            BPF_MAP_TYPE_HASH
            | BPF_MAP_TYPE_PERCPU_HASH
            | BPF_MAP_TYPE_LRU_HASH
            | BPF_MAP_TYPE_LRU_PERCPU_HASH
            | BPF_MAP_TYPE_STACK_TRACE
            | BPF_MAP_TYPE_HASH_OF_MAPS
            | BPF_MAP_TYPE_SOCKMAP
            | BPF_MAP_TYPE_SOCKHASH
            | BPF_MAP_TYPE_DEVMAP_HASH
            | BPF_MAP_TYPE_CGROUP_STORAGE
            | BPF_MAP_TYPE_PERCPU_CGROUP_STORAGE => {
                if key_size == 0 || value_size == 0 || max_entries == 0 || key_size > 512 || value_size > (1 << 20) {
                    return Err(EINVAL);
                }
                Store::Hash(BTreeMap::new())
            }
            _ => return Err(EINVAL),
        };
        Ok(MapData { ty, key_size, max_entries, uvalue: uvalue as usize, stride, store, clock: 0 })
    }

    pub fn key_size(&self) -> usize {
        self.key_size as usize
    }

    /// Size of a value buffer as user space passes it.
    pub fn value_len(&self) -> usize {
        self.uvalue
    }

    /// The shmem object behind an mmappable map.
    pub fn shmem(&self) -> Option<&Arc<Shmem>> {
        match &self.store {
            Store::Array(o) | Store::Ringbuf(o) => Some(o),
            Store::Hash(_) => None,
        }
    }

    fn is_fd_array(&self) -> bool {
        matches!(
            self.ty,
            BPF_MAP_TYPE_PROG_ARRAY
                | BPF_MAP_TYPE_PERF_EVENT_ARRAY
                | BPF_MAP_TYPE_CGROUP_ARRAY
                | BPF_MAP_TYPE_ARRAY_OF_MAPS
                | BPF_MAP_TYPE_DEVMAP
                | BPF_MAP_TYPE_CPUMAP
                | BPF_MAP_TYPE_XSKMAP
        )
    }

    fn index(&self, key: &[u8]) -> u32 {
        u32::from_ne_bytes(key[..4].try_into().unwrap())
    }

    /// (prefix length, key with the bits past the prefix cleared).
    fn lpm_key(&self, key: &[u8]) -> KResult<(u32, Vec<u8>)> {
        let plen = self.index(key);
        let data = &key[4..];
        if plen as usize > data.len() * 8 {
            return Err(EINVAL);
        }
        let mut k = key.to_vec();
        for (i, b) in k[4..].iter_mut().enumerate() {
            let bits = (plen as usize).saturating_sub(i * 8).min(8);
            *b &= (0xff00u16 >> bits) as u8;
        }
        Ok((plen, k))
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    pub fn lookup(&mut self, key: &[u8]) -> KResult<Vec<u8>> {
        let stamp = self.tick();
        let (ty, stride, uvalue) = (self.ty, self.stride, self.uvalue);
        let k = match ty {
            BPF_MAP_TYPE_LPM_TRIE => {
                // Longest stored prefix that matches the key.
                let plen = self.index(key);
                if plen as usize > (key.len() - 4) * 8 {
                    return Err(EINVAL);
                }
                let Store::Hash(h) = &self.store else { unreachable!() };
                let best = h
                    .keys()
                    .filter(|k| {
                        let p = u32::from_ne_bytes(k[..4].try_into().unwrap());
                        p <= plen && prefix_eq(&k[4..], &key[4..], p as usize)
                    })
                    .max_by_key(|k| u32::from_ne_bytes(k[..4].try_into().unwrap()));
                return best.map(|k| h[k].0.clone()).ok_or(ENOENT);
            }
            _ => key.to_vec(),
        };
        match &mut self.store {
            Store::Array(obj) => {
                let i = u32::from_ne_bytes(k[..4].try_into().unwrap());
                if i >= self.max_entries {
                    return Err(ENOENT);
                }
                let mut v = vec![0u8; uvalue];
                obj.read(i as u64 * stride, &mut v);
                Ok(v)
            }
            Store::Hash(h) => {
                let e = h.get_mut(&k).ok_or(ENOENT)?;
                e.1 = stamp;
                Ok(e.0.clone())
            }
            Store::Ringbuf(_) => Err(EINVAL),
        }
    }

    pub fn update(&mut self, key: &[u8], value: &[u8], flags: u64) -> KResult<()> {
        let mode = flags & !BPF_F_LOCK;
        if mode > BPF_EXIST {
            return Err(EINVAL);
        }
        let stamp = self.tick();
        let (max, stride, lru) =
            (self.max_entries, self.stride, matches!(self.ty, BPF_MAP_TYPE_LRU_HASH | BPF_MAP_TYPE_LRU_PERCPU_HASH));
        let k = if self.ty == BPF_MAP_TYPE_LPM_TRIE { self.lpm_key(key)?.1 } else { key.to_vec() };
        if self.is_fd_array() && self.index(key) >= max {
            return Err(E2BIG);
        }
        let limited = !is_storage(self.ty);
        match &mut self.store {
            Store::Array(obj) => {
                let i = u32::from_ne_bytes(k[..4].try_into().unwrap());
                if i >= max {
                    return Err(E2BIG);
                }
                if mode == BPF_NOEXIST {
                    return Err(EEXIST);
                }
                obj.write(i as u64 * stride, value)?;
                Ok(())
            }
            Store::Hash(h) => {
                let exists = h.contains_key(&k);
                match mode {
                    BPF_NOEXIST if exists => return Err(EEXIST),
                    BPF_EXIST if !exists => return Err(ENOENT),
                    _ => {}
                }
                if !exists && limited && h.len() >= max as usize {
                    if !lru {
                        return Err(E2BIG);
                    }
                    let oldest = h.iter().min_by_key(|(_, (_, t))| *t).map(|(k, _)| k.clone());
                    if let Some(o) = oldest {
                        h.remove(&o);
                    }
                }
                h.insert(k, (value.to_vec(), stamp));
                Ok(())
            }
            Store::Ringbuf(_) => Err(EINVAL),
        }
    }

    pub fn delete(&mut self, key: &[u8]) -> KResult<()> {
        let k = if self.ty == BPF_MAP_TYPE_LPM_TRIE { self.lpm_key(key)?.1 } else { key.to_vec() };
        match &mut self.store {
            Store::Array(_) | Store::Ringbuf(_) => Err(EINVAL),
            Store::Hash(h) => h.remove(&k).map(|_| ()).ok_or(ENOENT),
        }
    }

    /// The key after `key` (the first key when `key` is None or not in
    /// the map); ENOENT after the last one.
    pub fn next_key(&self, key: Option<&[u8]>) -> KResult<Vec<u8>> {
        match &self.store {
            Store::Array(_) => {
                let next = match key.map(|k| self.index(k)) {
                    Some(i) if i < self.max_entries => i.checked_add(1).ok_or(ENOENT)?,
                    _ => 0,
                };
                if next >= self.max_entries {
                    return Err(ENOENT);
                }
                Ok(next.to_ne_bytes().to_vec())
            }
            Store::Hash(h) => {
                use core::ops::Bound::{Excluded, Unbounded};
                let start = match key {
                    Some(k) if h.contains_key(k) => Excluded(k.to_vec()),
                    _ => Unbounded,
                };
                h.range((start, Unbounded)).next().map(|(k, _)| k.clone()).ok_or(ENOENT)
            }
            Store::Ringbuf(_) => Err(EINVAL),
        }
    }

    /// Bytes of storage in use (fdinfo memlock).
    pub fn memlock(&self) -> u64 {
        match &self.store {
            Store::Array(o) | Store::Ringbuf(o) => o.size(),
            Store::Hash(h) => (h.len() * (self.key_size as usize + self.uvalue)) as u64,
        }
    }
}

/// Whether the first `bits` bits of `a` and `b` are equal.
fn prefix_eq(a: &[u8], b: &[u8], bits: usize) -> bool {
    let full = bits / 8;
    if a[..full] != b[..full] {
        return false;
    }
    let rest = bits % 8;
    rest == 0 || (a[full] ^ b[full]) & (0xff00u16 >> rest) as u8 == 0
}
