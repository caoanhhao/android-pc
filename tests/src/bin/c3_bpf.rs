//! C3: eBPF as AOSP's bpfloader, netd and libbpf use it. bpf(2) maps:
//! array, hash (NOEXIST/EXIST, full -> E2BIG, iteration with
//! GET_NEXT_KEY), LRU hash eviction, per-CPU array values, LPM trie
//! longest-prefix lookups, a BPF_F_MMAPABLE array shared with mmap, a
//! ring buffer's mappings, freeze, read-only fds, OBJ_GET_INFO_BY_FD,
//! ids (GET_NEXT_ID, GET_FD_BY_ID). bpffs: pin, OBJ_GET, mode, rename,
//! unlink, EEXIST, EPERM outside bpffs. Programs: PROG_LOAD (with a map
//! reference), prog info, pinning, EBADF / EINVAL, cgroup attach / query /
//! detach. BTF_LOAD. OFD locks on map fds, as netd's mapLockTest takes
//! them.
//!
//! Needs CAP_BPF / root on Linux (unprivileged bpf is disabled there):
//! skipped when bpf() says EPERM.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("c3_bpf", main);

const SYS_BPF: usize = 321;
const SYS_FCHMODAT: usize = 268;
const E2BIG: i32 = 7;

const BPF_MAP_CREATE: usize = 0;
const BPF_MAP_LOOKUP_ELEM: usize = 1;
const BPF_MAP_UPDATE_ELEM: usize = 2;
const BPF_MAP_DELETE_ELEM: usize = 3;
const BPF_MAP_GET_NEXT_KEY: usize = 4;
const BPF_PROG_LOAD: usize = 5;
const BPF_OBJ_PIN: usize = 6;
const BPF_OBJ_GET: usize = 7;
const BPF_PROG_ATTACH: usize = 8;
const BPF_PROG_DETACH: usize = 9;
const BPF_PROG_GET_NEXT_ID: usize = 11;
const BPF_MAP_GET_NEXT_ID: usize = 12;
const BPF_MAP_GET_FD_BY_ID: usize = 14;
const BPF_OBJ_GET_INFO_BY_FD: usize = 15;
const BPF_PROG_QUERY: usize = 16;
const BPF_BTF_LOAD: usize = 18;
const BPF_MAP_FREEZE: usize = 22;

const HASH: u32 = 1;
const ARRAY: u32 = 2;
const PERCPU_ARRAY: u32 = 6;
const LRU_HASH: u32 = 9;
const LPM_TRIE: u32 = 11;
const RINGBUF: u32 = 27;

const BPF_NOEXIST: u64 = 1;
const BPF_EXIST: u64 = 2;
const BPF_F_NO_PREALLOC: u32 = 1;
const BPF_F_RDONLY: u32 = 1 << 3;
const BPF_F_MMAPABLE: u32 = 1 << 10;

const PROG_SOCKET_FILTER: u32 = 1;
const PROG_CGROUP_SKB: u32 = 8;
const BPF_CGROUP_INET_EGRESS: u32 = 1;
const BPF_F_ALLOW_MULTI: u32 = 2;

const F_OFD_SETLK: i32 = 37;
const F_RDLCK: i16 = 0;
const F_WRLCK: i16 = 1;

/// `union bpf_attr` as raw words.
#[repr(C, align(8))]
struct Attr([u8; 128]);

impl Attr {
    fn new() -> Attr {
        Attr([0; 128])
    }
    fn u32(mut self, off: usize, v: u32) -> Attr {
        self.0[off..off + 4].copy_from_slice(&v.to_ne_bytes());
        self
    }
    fn u64(mut self, off: usize, v: u64) -> Attr {
        self.0[off..off + 8].copy_from_slice(&v.to_ne_bytes());
        self
    }
    fn bytes(mut self, off: usize, b: &[u8]) -> Attr {
        self.0[off..off + b.len()].copy_from_slice(b);
        self
    }
    fn get32(&self, off: usize) -> u32 {
        u32::from_ne_bytes(self.0[off..off + 4].try_into().unwrap())
    }
}

fn bpf(cmd: usize, a: &mut Attr) -> isize {
    unsafe { syscall!(SYS_BPF, cmd, a as *mut Attr, 128usize) }
}

fn map_create(ty: u32, key: u32, value: u32, max: u32, flags: u32, name: &[u8]) -> isize {
    let mut a = Attr::new().u32(0, ty).u32(4, key).u32(8, value).u32(12, max).u32(16, flags).bytes(28, name);
    bpf(BPF_MAP_CREATE, &mut a)
}

fn ptr<T>(p: &T) -> u64 {
    p as *const T as u64
}

fn update<K, V>(fd: isize, k: &K, v: &V, flags: u64) -> isize {
    let mut a = Attr::new().u32(0, fd as u32).u64(8, ptr(k)).u64(16, ptr(v)).u64(24, flags);
    bpf(BPF_MAP_UPDATE_ELEM, &mut a)
}

fn lookup<K, V>(fd: isize, k: &K, v: &mut V) -> isize {
    let mut a = Attr::new().u32(0, fd as u32).u64(8, ptr(k)).u64(16, v as *mut V as u64);
    bpf(BPF_MAP_LOOKUP_ELEM, &mut a)
}

fn delete<K>(fd: isize, k: &K) -> isize {
    let mut a = Attr::new().u32(0, fd as u32).u64(8, ptr(k));
    bpf(BPF_MAP_DELETE_ELEM, &mut a)
}

/// GET_NEXT_KEY; `k` None starts from the beginning.
fn next_key<K>(fd: isize, k: Option<&K>, next: &mut K) -> isize {
    let mut a = Attr::new().u32(0, fd as u32).u64(8, k.map_or(0, ptr)).u64(16, next as *mut K as u64);
    bpf(BPF_MAP_GET_NEXT_KEY, &mut a)
}

fn pin(fd: isize, path: &Path) -> isize {
    let mut a = Attr::new().u64(0, path.as_ptr() as u64).u32(8, fd as u32);
    bpf(BPF_OBJ_PIN, &mut a)
}

fn obj_get(path: &Path, flags: u32) -> isize {
    let mut a = Attr::new().u64(0, path.as_ptr() as u64).u32(12, flags);
    bpf(BPF_OBJ_GET, &mut a)
}

/// OBJ_GET_INFO_BY_FD into `info`; returns (result, info_len written).
fn info(fd: isize, info: &mut [u8]) -> (isize, u32) {
    let mut a = Attr::new().u32(0, fd as u32).u32(4, info.len() as u32).u64(8, info.as_mut_ptr() as u64);
    let r = bpf(BPF_OBJ_GET_INFO_BY_FD, &mut a);
    (r, a.get32(4))
}

fn le32(b: &[u8], off: usize) -> u32 {
    u32::from_ne_bytes(b[off..off + 4].try_into().unwrap())
}

fn map_id(fd: isize) -> u32 {
    let mut i = [0u8; 88];
    info(fd, &mut i);
    le32(&i, 4)
}

/// Load a program of `ty` from 8-byte instructions.
fn prog_load(ty: u32, insns: &[[u8; 8]], attach: u32, name: &[u8]) -> isize {
    let mut a = Attr::new()
        .u32(0, ty)
        .u32(4, insns.len() as u32)
        .u64(8, insns.as_ptr() as u64)
        .u64(16, c"GPL".as_ptr() as u64)
        .bytes(48, name)
        .u32(68, attach);
    bpf(BPF_PROG_LOAD, &mut a)
}

/// Instruction encoding helpers.
fn ins(code: u8, dst: u8, src: u8, off: i16, imm: i32) -> [u8; 8] {
    let mut b = [0u8; 8];
    b[0] = code;
    b[1] = dst | (src << 4);
    b[2..4].copy_from_slice(&off.to_ne_bytes());
    b[4..8].copy_from_slice(&imm.to_ne_bytes());
    b
}
const MOV_R0_0: u8 = 0xb7; // BPF_ALU64 | BPF_MOV | BPF_K
const EXIT: u8 = 0x95; // BPF_JMP | BPF_EXIT
const LD_IMM64: u8 = 0x18; // BPF_LD | BPF_DW | BPF_IMM

fn ofd_lock(fd: isize, ty: i16, start: i64) -> isize {
    let mut fl = [0u8; 32];
    fl[0..2].copy_from_slice(&ty.to_ne_bytes());
    fl[8..16].copy_from_slice(&start.to_ne_bytes());
    fl[16..24].copy_from_slice(&1i64.to_ne_bytes());
    fcntl(fd as i32, F_OFD_SETLK, fl.as_ptr() as usize)
}

fn ncpus() -> usize {
    // "0-3" or "0".
    let mut b = [0u8; 64];
    let n = read_file(c"/sys/devices/system/cpu/possible".as_ptr() as *const u8, &mut b);
    let s = trim(&b[..n.max(0) as usize]);
    let last = s.rsplit(|&c| c == b'-').next().unwrap_or(b"0");
    parse_int(last).unwrap_or(0) as usize + 1
}

fn main() {
    let probe = map_create(ARRAY, 4, 4, 1, 0, b"probe");
    if probe == -(EPERM as isize) || probe == -(ENOSYS as isize) {
        skip("all", "bpf() not permitted here (needs CAP_BPF)");
        return;
    }
    check_ok!(probe, "map_create_array");
    close(probe as i32);

    arrays();
    hashes();
    special_maps();
    ids_and_info();
    let dir = scratch_dir();
    bpffs(&dir);
    programs(&dir);
    btf();
    locks(&dir);
}

fn arrays() {
    let m = map_create(ARRAY, 4, 8, 4, 0, b"c3_array");
    check_ok!(m, "array_create");
    let (k1, v1) = (1u32, 0x1122_3344_5566_7788u64);
    check_eq!(update(m, &k1, &v1, 0), 0, "array_update");
    let mut v = 0u64;
    check!(lookup(m, &k1, &mut v) == 0 && v == v1, "array_lookup", "v={:#x}", v);
    v = 1;
    check!(lookup(m, &0u32, &mut v) == 0 && v == 0, "array_lookup_zero");
    check_err!(lookup(m, &4u32, &mut v), ENOENT, "array_lookup_out_of_range");
    check_err!(update(m, &4u32, &v1, 0), E2BIG, "array_update_out_of_range");
    check_err!(update(m, &1u32, &v1, BPF_NOEXIST), EEXIST, "array_noexist");
    check_err!(delete(m, &1u32), EINVAL, "array_delete");
    let mut n = 99u32;
    check!(next_key::<u32>(m, None, &mut n) == 0 && n == 0, "array_next_first", "n={}", n);
    check!(next_key(m, Some(&2u32), &mut n) == 0 && n == 3, "array_next");
    check_err!(next_key(m, Some(&3u32), &mut n), ENOENT, "array_next_end");
    check_err!(map_create(ARRAY, 8, 8, 4, 0, b"bad"), EINVAL, "array_key_size_8");
    check_err!(map_create(ARRAY, 4, 8, 0, 0, b"bad"), EINVAL, "array_max_0");

    // Freeze: no more writes from user space, reads still work.
    let mut a = Attr::new().u32(0, m as u32);
    check_eq!(bpf(BPF_MAP_FREEZE, &mut a), 0, "freeze");
    check_err!(update(m, &1u32, &v1, 0), EPERM, "frozen_update");
    check!(lookup(m, &k1, &mut v) == 0 && v == v1, "frozen_lookup");
    close(m as i32);

    // Per-CPU: user space sees one 8-byte-aligned value per possible CPU.
    let cpus = ncpus();
    check!(cpus >= 1, "ncpus", "{}", cpus);
    let pc = map_create(PERCPU_ARRAY, 4, 4, 2, 0, b"c3_percpu");
    check_ok!(pc, "percpu_create");
    let mut vals = [0u64; 64];
    for (i, x) in vals.iter_mut().enumerate().take(cpus) {
        *x = 100 + i as u64;
    }
    check_eq!(update(pc, &1u32, &vals, 0), 0, "percpu_update");
    let mut back = [0u64; 64];
    check!(lookup(pc, &1u32, &mut back) == 0 && back[..cpus] == vals[..cpus], "percpu_lookup");
    close(pc as i32);

    // A BPF_F_MMAPABLE array shares its pages with lookup/update.
    let mm = map_create(ARRAY, 4, 8, 512, BPF_F_MMAPABLE, b"c3_mmap");
    check_ok!(mm, "mmapable_create");
    check_eq!(update(mm, &2u32, &0xabcdu64, 0), 0, "mmapable_update");
    let p = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, mm as i32, 0);
    check!(p >= 0, "mmapable_mmap", "{}", errname(p));
    if p >= 0 {
        let words = p as *mut u64;
        check_eq!(unsafe { words.add(2).read_volatile() }, 0xabcd, "mmap_sees_update");
        unsafe { words.add(3).write_volatile(0x5150) };
        let mut v = 0u64;
        check!(lookup(mm, &3u32, &mut v) == 0 && v == 0x5150, "lookup_sees_mmap_write");
        munmap(p as *mut u8, 4096);
    }
    let plain = map_create(ARRAY, 4, 8, 4, 0, b"c3_nommap");
    check!(mmap(0, 4096, PROT_READ, MAP_SHARED, plain as i32, 0) < 0, "plain_array_no_mmap");
    close(plain as i32);
    close(mm as i32);
}

fn hashes() {
    let h = map_create(HASH, 4, 8, 3, 0, b"c3_hash");
    check_ok!(h, "hash_create");
    for k in [10u32, 20, 30] {
        update(h, &k, &(k as u64 * 2), BPF_NOEXIST);
    }
    let mut v = 0u64;
    check!(lookup(h, &20u32, &mut v) == 0 && v == 40, "hash_lookup");
    check_err!(lookup(h, &21u32, &mut v), ENOENT, "hash_lookup_missing");
    check_err!(update(h, &40u32, &1u64, 0), E2BIG, "hash_full");
    check_err!(update(h, &10u32, &1u64, BPF_NOEXIST), EEXIST, "hash_noexist");
    check_err!(update(h, &11u32, &1u64, BPF_EXIST), ENOENT, "hash_exist_missing");
    check_eq!(update(h, &10u32, &7u64, BPF_EXIST), 0, "hash_exist_replace");
    check!(lookup(h, &10u32, &mut v) == 0 && v == 7, "hash_replaced");
    // Iterate: every key exactly once.
    let mut seen = [false; 3];
    let mut k = 0u32;
    let mut r = next_key::<u32>(h, None, &mut k);
    let mut steps = 0;
    while r == 0 && steps < 10 {
        if let Some(i) = [10u32, 20, 30].iter().position(|&x| x == k) {
            seen[i] = true;
        }
        let cur = k;
        r = next_key(h, Some(&cur), &mut k);
        steps += 1;
    }
    check!(r == -(ENOENT as isize) && steps == 3 && seen.iter().all(|&s| s), "hash_iterate", "steps={} r={}", steps, r);
    // A key that is not there restarts from the first key.
    check!(next_key(h, Some(&12345u32), &mut k) == 0 && [10, 20, 30].contains(&k), "hash_next_missing_key");
    check_eq!(delete(h, &20u32), 0, "hash_delete");
    check_err!(delete(h, &20u32), ENOENT, "hash_delete_again");
    check_eq!(update(h, &40u32, &1u64, 0), 0, "hash_room_after_delete");
    close(h as i32);

    // LRU: a full map evicts its least recently used key.
    let l = map_create(LRU_HASH, 4, 4, 2, 0, b"c3_lru");
    check_ok!(l, "lru_create");
    update(l, &1u32, &1u32, 0);
    update(l, &2u32, &2u32, 0);
    let mut x = 0u32;
    lookup(l, &1u32, &mut x); // 2 is now the oldest
    check_eq!(update(l, &3u32, &3u32, 0), 0, "lru_insert_when_full");
    check_err!(lookup(l, &2u32, &mut x), ENOENT, "lru_evicted_oldest");
    check_eq!(lookup(l, &1u32, &mut x), 0, "lru_kept_recent");
    close(l as i32);

    // LPM trie: IPv4 prefixes, longest match wins.
    let t = map_create(LPM_TRIE, 8, 4, 16, BPF_F_NO_PREALLOC, b"c3_lpm");
    check_ok!(t, "lpm_create");
    check_err!(map_create(LPM_TRIE, 8, 4, 16, 0, b"bad"), EINVAL, "lpm_needs_no_prealloc");
    let key = |plen: u32, a: [u8; 4]| -> [u8; 8] {
        let mut k = [0u8; 8];
        k[..4].copy_from_slice(&plen.to_ne_bytes());
        k[4..].copy_from_slice(&a);
        k
    };
    check_eq!(update(t, &key(8, [10, 0, 0, 0]), &1u32, 0), 0, "lpm_add_8");
    check_eq!(update(t, &key(16, [10, 1, 0, 0]), &2u32, 0), 0, "lpm_add_16");
    let mut v = 0u32;
    check!(lookup(t, &key(32, [10, 1, 2, 3]), &mut v) == 0 && v == 2, "lpm_longest", "v={}", v);
    check!(lookup(t, &key(32, [10, 2, 0, 1]), &mut v) == 0 && v == 1, "lpm_shorter", "v={}", v);
    check_err!(lookup(t, &key(32, [11, 0, 0, 1]), &mut v), ENOENT, "lpm_no_match");
    check_eq!(delete(t, &key(16, [10, 1, 0, 0])), 0, "lpm_delete");
    check!(lookup(t, &key(32, [10, 1, 2, 3]), &mut v) == 0 && v == 1, "lpm_after_delete");
    close(t as i32);
}

fn special_maps() {
    // Ring buffer: power-of-2 pages; consumer page RW, the rest RO.
    check_err!(map_create(RINGBUF, 0, 0, 4096 + 1, 0, b"bad"), EINVAL, "ringbuf_size_pow2");
    let rb = map_create(RINGBUF, 0, 0, 4096, 0, b"c3_ringbuf");
    check_ok!(rb, "ringbuf_create");
    let c = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, rb as i32, 0);
    check!(c >= 0, "ringbuf_mmap_consumer", "{}", errname(c));
    let p = mmap(0, 3 * 4096, PROT_READ, MAP_SHARED, rb as i32, 4096);
    check!(p >= 0, "ringbuf_mmap_producer", "{}", errname(p));
    if p >= 0 {
        check_eq!(unsafe { (p as *const u64).read_volatile() }, 0, "ringbuf_empty");
    }
    let mut fds = [Pollfd { fd: rb as i32, events: 1, revents: 0 }];
    check_eq!(poll(&mut fds, 0), 0, "ringbuf_poll_empty");
    close(rb as i32);
}

fn ids_and_info() {
    let m = map_create(HASH, 4, 16, 8, 0, b"c3_info");
    let mut i = [0u8; 88];
    let (r, len) = info(m, &mut i);
    check!(r == 0 && len == 88, "map_info", "r={} len={}", r, len);
    check!(
        le32(&i, 0) == HASH && le32(&i, 8) == 4 && le32(&i, 12) == 16 && le32(&i, 16) == 8 && &i[24..32] == b"c3_info\0",
        "map_info_fields"
    );
    let id = le32(&i, 4);
    check!(id > 0, "map_id");
    // A short buffer gets a short answer.
    let mut small = [0u8; 8];
    let (r, len) = info(m, &mut small);
    check!(r == 0 && len == 8 && le32(&small, 4) == id, "map_info_short");
    // A longer one must be zero past the kernel's struct.
    let mut big = [0u8; 200];
    big[150] = 1;
    check_err!(info(m, &mut big).0, E2BIG, "map_info_e2big");

    // Ids: iteration finds it, GET_FD_BY_ID gives the same map.
    let mut found = false;
    let mut cur = 0u32;
    for _ in 0..10000 {
        let mut a = Attr::new().u32(0, cur);
        if bpf(BPF_MAP_GET_NEXT_ID, &mut a) != 0 {
            break;
        }
        let next = a.get32(4);
        if next <= cur {
            break;
        }
        cur = next;
        found |= cur == id;
    }
    check!(found, "map_get_next_id");
    let mut a = Attr::new().u32(0, id);
    let fd = bpf(BPF_MAP_GET_FD_BY_ID, &mut a);
    check_ok!(fd, "map_get_fd_by_id");
    update(m, &5u32, &[9u8; 16], 0);
    let mut v = [0u8; 16];
    check!(lookup(fd, &5u32, &mut v) == 0 && v == [9u8; 16], "same_map_by_id");
    close(fd as i32);
    close(m as i32);
    // Freed with its last fd: the id is gone.
    let mut a = Attr::new().u32(0, id);
    check_err!(bpf(BPF_MAP_GET_FD_BY_ID, &mut a), ENOENT, "map_id_freed");
}

fn bpffs(dir: &Path) {
    let fs = cpath!("{}/bpf", Bytes(dir.as_bytes()));
    mkdirat(AT_FDCWD, fs.as_ptr(), 0o755);
    let r = mount(c"bpf".as_ptr() as *const u8, fs.as_ptr(), c"bpf".as_ptr() as *const u8, 0, core::ptr::null());
    if !check_eq!(r, 0, "mount_bpffs") {
        return;
    }
    let m = map_create(HASH, 4, 4, 4, 0, b"c3_pinned");
    update(m, &1u32, &111u32, 0);
    let id = map_id(m);
    let p = cpath!("{}/map_c3", Bytes(fs.as_bytes()));
    check_eq!(pin(m, &p), 0, "pin_map");
    check_err!(pin(m, &p), EEXIST, "pin_exists");
    let mut st = Stat::default();
    check!(fstatat(AT_FDCWD, p.as_ptr(), &mut st, 0) == 0 && st.mode & 0o170000 == S_IFREG, "pinned_is_file", "mode={:o}", st.mode);
    close(m as i32);
    // The pin keeps the map alive.
    let g = obj_get(&p, 0);
    check_ok!(g, "obj_get");
    check_eq!(map_id(g), id, "obj_get_same_id");
    let mut v = 0u32;
    check!(lookup(g, &1u32, &mut v) == 0 && v == 111, "obj_get_same_data");
    // A read-only fd can look up but not update.
    let ro = obj_get(&p, BPF_F_RDONLY);
    check_ok!(ro, "obj_get_rdonly");
    check_eq!(lookup(ro, &1u32, &mut v), 0, "rdonly_lookup");
    check_err!(update(ro, &2u32, &2u32, 0), EPERM, "rdonly_update");
    close(ro as i32);
    // Pinned objects are not opened with open(2).
    check!(open(p.as_ptr(), O_RDONLY, 0) < 0, "open_pinned_fails");
    // Subdirectories, rename (as netbpfload's create_location), chmod.
    let sub = cpath!("{}/sub", Bytes(fs.as_bytes()));
    check_eq!(mkdirat(AT_FDCWD, sub.as_ptr(), 0o1777), 0, "bpffs_mkdir");
    let tmp = cpath!("{}/sub/tmp_map", Bytes(fs.as_bytes()));
    let fin = cpath!("{}/sub/map_final", Bytes(fs.as_bytes()));
    check_eq!(pin(g, &tmp), 0, "pin_in_subdir");
    let r = unsafe { syscall!(nr::RENAMEAT, AT_FDCWD, tmp.as_ptr(), AT_FDCWD, fin.as_ptr()) };
    check_eq!(r, 0, "rename_pinned");
    let g2 = obj_get(&fin, 0);
    check!(g2 >= 0 && map_id(g2) == id, "obj_get_renamed");
    close(g2 as i32);
    check_eq!(unsafe { syscall!(SYS_FCHMODAT, AT_FDCWD, fin.as_ptr(), 0o440) }, 0, "chmod_pinned");
    // Unlinking the last pin and closing the last fd frees the map.
    unlinkat(AT_FDCWD, fin.as_ptr(), 0);
    check_eq!(unlinkat(AT_FDCWD, p.as_ptr(), 0), 0, "unlink_pin");
    check_err!(obj_get(&p, 0), ENOENT, "obj_get_unlinked");
    close(g as i32);
    let mut a = Attr::new().u32(0, id);
    check_err!(bpf(BPF_MAP_GET_FD_BY_ID, &mut a), ENOENT, "unpinned_map_freed");
    // Only inside a bpffs.
    let m2 = map_create(ARRAY, 4, 4, 1, 0, b"x");
    let outside = cpath!("{}/not_bpffs", Bytes(dir.as_bytes()));
    check_err!(pin(m2, &outside), EPERM, "pin_outside_bpffs");
    close(m2 as i32);
    umount2(fs.as_ptr(), 0);
}

fn programs(dir: &Path) {
    let prog = [ins(MOV_R0_0, 0, 0, 0, 0), ins(EXIT, 0, 0, 0, 0)];
    let p = prog_load(PROG_SOCKET_FILTER, &prog, 0, b"c3_prog");
    check_ok!(p, "prog_load");
    let mut i = [0u8; 232];
    let (r, _) = info(p, &mut i);
    check!(r == 0 && le32(&i, 0) == PROG_SOCKET_FILTER && le32(&i, 4) > 0, "prog_info");
    check_eq!(le32(&i, 20), 16, "prog_xlated_len");
    check!(le32(&i, 16) > 0, "prog_jited_len");
    check_eq!(&i[64..72], b"c3_prog\0", "prog_name");
    let pid = le32(&i, 4);
    let mut cur = 0u32;
    let mut found = false;
    for _ in 0..10000 {
        let mut a = Attr::new().u32(0, cur);
        if bpf(BPF_PROG_GET_NEXT_ID, &mut a) != 0 {
            break;
        }
        cur = a.get32(4);
        found |= cur == pid;
    }
    check!(found, "prog_get_next_id");

    // A program using a map (ld_imm64 with BPF_PSEUDO_MAP_FD) holds it.
    let m = map_create(ARRAY, 4, 4, 1, 0, b"c3_used");
    let mid = map_id(m);
    let with_map = [
        ins(LD_IMM64, 1, 1, 0, m as i32),
        ins(0, 0, 0, 0, 0),
        ins(MOV_R0_0, 0, 0, 0, 0),
        ins(EXIT, 0, 0, 0, 0),
    ];
    let pm = prog_load(PROG_SOCKET_FILTER, &with_map, 0, b"c3_map_user");
    check_ok!(pm, "prog_load_with_map");
    let mut ids = [0u32; 4];
    let mut i = [0u8; 232];
    i[52..56].copy_from_slice(&4u32.to_ne_bytes());
    i[56..64].copy_from_slice(&(ids.as_mut_ptr() as u64).to_ne_bytes());
    let (r, _) = info(pm, &mut i);
    check!(r == 0 && le32(&i, 52) == 1 && ids[0] == mid, "prog_map_ids", "n={} id={} want {}", le32(&i, 52), ids[0], mid);
    close(m as i32);
    let mut a = Attr::new().u32(0, mid);
    let again = bpf(BPF_MAP_GET_FD_BY_ID, &mut a);
    check!(again >= 0, "prog_keeps_map");
    close(again as i32);
    close(pm as i32);

    let bad = [ins(LD_IMM64, 1, 1, 0, 999), ins(0, 0, 0, 0, 0), ins(EXIT, 0, 0, 0, 0)];
    check_err!(prog_load(PROG_SOCKET_FILTER, &bad, 0, b"bad"), EBADF, "prog_bad_map_fd");
    check_err!(prog_load(PROG_SOCKET_FILTER, &[], 0, b"bad"), EINVAL, "prog_no_insns");
    check_err!(prog_load(0, &prog, 0, b"bad"), EINVAL, "prog_type_unspec");

    // cgroup attach / query / detach (netd attaches its programs to the
    // cgroup v2 root and checks with BPF_PROG_QUERY).
    let cg = cpath!("{}/cg", Bytes(dir.as_bytes()));
    mkdirat(AT_FDCWD, cg.as_ptr(), 0o755);
    let none = c"none".as_ptr() as *const u8;
    if mount(none, cg.as_ptr(), c"cgroup2".as_ptr() as *const u8, 0, core::ptr::null()) == 0 {
        let cgfd = open(cg.as_ptr(), O_RDONLY | O_DIRECTORY, 0);
        let skb = prog_load(PROG_CGROUP_SKB, &prog, BPF_CGROUP_INET_EGRESS, b"c3_egress");
        check_ok!(skb, "prog_load_cgroup_skb");
        let mut a = Attr::new().u32(0, cgfd as u32).u32(4, skb as u32).u32(8, BPF_CGROUP_INET_EGRESS).u32(12, BPF_F_ALLOW_MULTI);
        check_eq!(bpf(BPF_PROG_ATTACH, &mut a), 0, "cgroup_attach");
        let mut out = 0u32;
        let mut q = Attr::new().u32(0, cgfd as u32).u32(4, BPF_CGROUP_INET_EGRESS).u64(16, ptr(&out)).u32(24, 1);
        let r = bpf(BPF_PROG_QUERY, &mut q);
        out = unsafe { core::ptr::read_volatile(&out) };
        let mut si = [0u8; 232];
        info(skb, &mut si);
        check!(r == 0 && q.get32(24) == 1 && out == le32(&si, 4), "cgroup_query", "r={} cnt={} id={}", r, q.get32(24), out);
        let mut d = Attr::new().u32(0, cgfd as u32).u32(4, skb as u32).u32(8, BPF_CGROUP_INET_EGRESS);
        check_eq!(bpf(BPF_PROG_DETACH, &mut d), 0, "cgroup_detach");
        let mut q = Attr::new().u32(0, cgfd as u32).u32(4, BPF_CGROUP_INET_EGRESS).u64(16, ptr(&out)).u32(24, 1);
        check!(bpf(BPF_PROG_QUERY, &mut q) == 0 && q.get32(24) == 0, "cgroup_query_after_detach");
        close(skb as i32);
        close(cgfd as i32);
        umount2(cg.as_ptr(), 0);
    } else {
        skip("cgroup_attach", "cannot mount cgroup2");
    }
    close(p as i32);
}

fn btf() {
    // Header, one "int" type (BTF_KIND_INT, 4 bytes, 32 bits), strings.
    let mut b = [0u8; 24 + 16 + 5];
    b[0..2].copy_from_slice(&0xeb9fu16.to_le_bytes());
    b[2] = 1;
    b[4..8].copy_from_slice(&24u32.to_le_bytes()); // hdr_len
    b[8..12].copy_from_slice(&0u32.to_le_bytes()); // type_off
    b[12..16].copy_from_slice(&16u32.to_le_bytes()); // type_len
    b[16..20].copy_from_slice(&16u32.to_le_bytes()); // str_off
    b[20..24].copy_from_slice(&5u32.to_le_bytes()); // str_len
    b[24..28].copy_from_slice(&1u32.to_le_bytes()); // name_off "int"
    b[28..32].copy_from_slice(&(1u32 << 24).to_le_bytes()); // kind INT
    b[32..36].copy_from_slice(&4u32.to_le_bytes()); // size
    b[36..40].copy_from_slice(&(32u32 | (1 << 24)).to_le_bytes()); // signed, 32 bits
    b[40..45].copy_from_slice(b"\0int\0");
    let mut a = Attr::new().u64(0, b.as_ptr() as u64).u32(16, b.len() as u32);
    let fd = bpf(BPF_BTF_LOAD, &mut a);
    check_ok!(fd, "btf_load");
    let mut i = [0u8; 32];
    let (r, _) = info(fd, &mut i);
    check!(r == 0 && le32(&i, 8) == b.len() as u32 && le32(&i, 12) > 0, "btf_info");
    // A map with BTF key/value types.
    let mut a = Attr::new().u32(0, HASH).u32(4, 4).u32(8, 4).u32(12, 2).bytes(28, b"c3_btf_map").u32(48, fd as u32).u32(52, 1).u32(56, 1);
    let m = bpf(BPF_MAP_CREATE, &mut a);
    check_ok!(m, "map_with_btf");
    close(m as i32);
    close(fd as i32);
    b[0] = 0;
    let mut a = Attr::new().u64(0, b.as_ptr() as u64).u32(16, b.len() as u32);
    check_err!(bpf(BPF_BTF_LOAD, &mut a), EINVAL, "btf_bad_magic");
}

fn locks(dir: &Path) {
    // netd's mapLockTest: two fds of one map, OFD locks at byte `map id`
    // of the shared anon inode.
    let fs = cpath!("{}/bpf2", Bytes(dir.as_bytes()));
    mkdirat(AT_FDCWD, fs.as_ptr(), 0o755);
    if mount(c"bpf".as_ptr() as *const u8, fs.as_ptr(), c"bpf".as_ptr() as *const u8, 0, core::ptr::null()) != 0 {
        skip("locks", "cannot mount bpffs");
        return;
    }
    let m1 = map_create(ARRAY, 4, 4, 1, 0, b"lock1");
    let m2 = map_create(HASH, 4, 4, 1, 0, b"lock2");
    let (p1, p2) = (cpath!("{}/m1", Bytes(fs.as_bytes())), cpath!("{}/m2", Bytes(fs.as_bytes())));
    pin(m1, &p1);
    pin(m2, &p2);
    let (id1, id2) = (map_id(m1) as i64, map_id(m2) as i64);
    let fd0 = obj_get(&p1, 0);
    check_eq!(ofd_lock(fd0, F_WRLCK, id1), 0, "ofd_wrlock");
    let fd1 = obj_get(&p2, 0);
    check_eq!(ofd_lock(fd1, F_WRLCK, id2), 0, "ofd_wrlock_other_map");
    let fd2 = obj_get(&p2, 0);
    check_err!(ofd_lock(fd2, F_WRLCK, id2), EAGAIN, "ofd_wrlock_busy");
    check_err!(ofd_lock(fd2, F_RDLCK, id2), EAGAIN, "ofd_rdlock_busy");
    close(fd1 as i32); // drops its lock
    check_eq!(ofd_lock(fd2, F_RDLCK, id2), 0, "ofd_rdlock_after_close");
    let fd3 = obj_get(&p2, 0);
    check_eq!(ofd_lock(fd3, F_RDLCK, id2), 0, "ofd_shared_rdlocks");
    check_err!(ofd_lock(fd3, F_WRLCK, id2), EAGAIN, "ofd_upgrade_blocked");
    // A read-only fd cannot take a write lock.
    let ro = obj_get(&p1, BPF_F_RDONLY);
    check_err!(ofd_lock(ro, F_WRLCK, 1000), EBADF, "ofd_wrlock_rdonly_fd");
    for fd in [fd0, fd2, fd3, ro, m1, m2] {
        close(fd as i32);
    }
    umount2(fs.as_ptr(), 0);
}
