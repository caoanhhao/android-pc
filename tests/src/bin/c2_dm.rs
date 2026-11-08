//! C2: device mapper through raw /dev/device-mapper ioctls, as libdm
//! issues them: VERSION, LIST_VERSIONS, GET_TARGET_VERSION, DEV_CREATE
//! (name + uuid, EBUSY on duplicates), TABLE_LOAD (inactive until resume),
//! DEV_SUSPEND/resume, DEV_STATUS flags, TABLE_STATUS (table and status),
//! TABLE_DEPS, LIST_DEVICES, DEV_RENAME, DEV_REMOVE (EBUSY while
//! mounted), linear / zero / error targets, multi-target tables, verity
//! accepted as a pass-through, /dev/block/dm-N nodes and sysfs dm/name.
//! The linear targets map partitions of the C2 test disk; erofs is mounted
//! from a dm device.
//!
//! Without the disk or permission (host: needs root) cases are skipped.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("c2_dm", main);

const DM_SIZE: usize = 312;
const fn dm_cmd(nr: u32) -> u32 {
    (3 << 30) | ((DM_SIZE as u32) << 16) | (0xfd << 8) | nr
}
const DM_VERSION: u32 = dm_cmd(0);
const DM_LIST_DEVICES: u32 = dm_cmd(2);
const DM_DEV_CREATE: u32 = dm_cmd(3);
const DM_DEV_REMOVE: u32 = dm_cmd(4);
const DM_DEV_RENAME: u32 = dm_cmd(5);
const DM_DEV_SUSPEND: u32 = dm_cmd(6);
const DM_DEV_STATUS: u32 = dm_cmd(7);
const DM_TABLE_LOAD: u32 = dm_cmd(9);
const DM_TABLE_DEPS: u32 = dm_cmd(11);
const DM_TABLE_STATUS: u32 = dm_cmd(12);
const DM_LIST_VERSIONS: u32 = dm_cmd(13);
const DM_GET_TARGET_VERSION: u32 = dm_cmd(17);

const DM_READONLY_FLAG: u32 = 1;
const DM_SUSPEND_FLAG: u32 = 2;
const DM_STATUS_TABLE_FLAG: u32 = 16;
const DM_ACTIVE_PRESENT_FLAG: u32 = 32;
const DM_INACTIVE_PRESENT_FLAG: u32 = 64;

const BLKGETSIZE64: u32 = 0x8008_1272;
const MS_RDONLY: usize = 1;

/// One ioctl buffer: struct dm_ioctl + data.
struct Dm {
    b: [u8; 16384],
}

impl Dm {
    fn new(name: &str, uuid: &str, flags: u32) -> Dm {
        let mut d = Dm { b: [0; 16384] };
        d.put32(0, 4);
        d.put32(12, 16384);
        d.put32(16, DM_SIZE as u32);
        d.put32(28, flags);
        d.b[48..48 + name.len()].copy_from_slice(name.as_bytes());
        d.b[176..176 + uuid.len()].copy_from_slice(uuid.as_bytes());
        d
    }
    fn put32(&mut self, o: usize, v: u32) {
        self.b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn get32(&self, o: usize) -> u32 {
        u32::from_le_bytes(self.b[o..o + 4].try_into().unwrap())
    }
    fn get64(&self, o: usize) -> u64 {
        u64::from_le_bytes(self.b[o..o + 8].try_into().unwrap())
    }
    fn flags(&self) -> u32 {
        self.get32(28)
    }
    fn dev(&self) -> u64 {
        self.get64(40)
    }
    fn data(&self) -> &[u8] {
        let start = self.get32(16) as usize;
        let size = (self.get32(12) as usize).min(self.b.len());
        &self.b[start.min(size)..size]
    }
    /// Append a dm_target_spec + params for TABLE_LOAD.
    fn target(&mut self, at: &mut usize, start: u64, len: u64, ty: &str, params: &[u8]) {
        let o = *at;
        self.b[o..o + 8].copy_from_slice(&start.to_le_bytes());
        self.b[o + 8..o + 16].copy_from_slice(&len.to_le_bytes());
        self.b[o + 24..o + 24 + ty.len()].copy_from_slice(ty.as_bytes());
        self.b[o + 40..o + 40 + params.len()].copy_from_slice(params);
        let next = (40 + params.len() + 1).next_multiple_of(8);
        self.put32(o + 20, next as u32);
        *at += next;
        let count = self.get32(20) + 1;
        self.put32(20, count);
    }
    fn call(&mut self, fd: i32, cmd: u32) -> isize {
        ioctl(fd, cmd, self.b.as_mut_ptr() as usize)
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn size64(path: &Path) -> u64 {
    let fd = open(path.as_ptr(), O_RDONLY, 0) as i32;
    let mut v = 0u64;
    ioctl(fd, BLKGETSIZE64, &mut v as *mut u64 as usize);
    close(fd);
    v
}

fn read_at(path: &Path, off: i64, buf: &mut [u8]) -> isize {
    let fd = open(path.as_ptr(), O_RDONLY, 0) as i32;
    if fd < 0 {
        return fd as isize;
    }
    let r = pread(fd, buf, off);
    close(fd);
    r
}

fn dm_node(dev: u64) -> Path {
    let minor = (dev & 0xff) | ((dev >> 12) & 0xfff00);
    cpath!("/dev/block/dm-{}", minor)
}

fn create(fd: i32, name: &str, uuid: &str) -> (isize, u64) {
    let mut d = Dm::new(name, uuid, 0);
    let r = d.call(fd, DM_DEV_CREATE);
    (r, d.dev())
}

fn main() {
    let fd = open(c"/dev/device-mapper".as_ptr() as *const u8, O_RDWR, 0) as i32;
    if fd < 0 {
        skip("all", "no usable /dev/device-mapper (needs root on Linux)");
        return;
    }
    let part = cpath!("/dev/block/by-name/c2_erofs");
    let psize = size64(&part);
    if psize == 0 {
        skip("all", "no C2 test disk; Rust kernel with run-qemu.sh --ktest only");
        return;
    }
    let psect = psize / 512;

    let mut d = Dm::new("", "", 0);
    let r = d.call(fd, DM_VERSION);
    check!(r == 0 && d.get32(0) == 4, "version", "{} v{}.{}", errname(r), d.get32(0), d.get32(4));
    let mut d = Dm::new("", "", 0);
    let r = d.call(fd, DM_LIST_VERSIONS);
    check!(r == 0 && contains(d.data(), b"linear\0") && contains(d.data(), b"verity\0"), "list_versions");
    let mut d = Dm::new("", "", 0);
    d.b[DM_SIZE..DM_SIZE + 7].copy_from_slice(b"linear\0");
    let r = d.call(fd, DM_GET_TARGET_VERSION);
    check!(r == 0 && contains(d.data(), b"linear") && d.get32(DM_SIZE + 4) == 1, "get_target_version");

    // Create: a device with no table.
    let (r, dev) = create(fd, "c2-lin", "C2-UUID-LIN");
    let node = dm_node(dev);
    let mut st = Stat::default();
    let nr = fstatat(AT_FDCWD, node.as_ptr(), &mut st, 0);
    check!(r == 0 && dev >> 8 & 0xfff == 253 && nr == 0 && st.rdev == dev && size64(&node) == 0, "create",
           "{} dev {:#x} node {}", errname(r), dev, errname(nr));
    check_err!(create(fd, "c2-lin", "").0, EBUSY, "create_same_name_ebusy");
    check_err!(create(fd, "c2-other", "C2-UUID-LIN").0, EBUSY, "create_same_uuid_ebusy");

    // Load a read-only linear table over the erofs partition.
    let mut d = Dm::new("c2-lin", "", DM_READONLY_FLAG);
    let mut at = DM_SIZE;
    let params = cpath!("{} 0", Bytes(part.as_bytes()));
    d.target(&mut at, 0, psect, "linear", params.as_bytes());
    let r = d.call(fd, DM_TABLE_LOAD);
    let mut s = Dm::new("c2-lin", "", 0);
    s.call(fd, DM_DEV_STATUS);
    check!(r == 0 && s.flags() & DM_INACTIVE_PRESENT_FLAG != 0 && s.flags() & DM_ACTIVE_PRESENT_FLAG == 0
           && size64(&node) == 0, "table_load_inactive", "{} flags {:#x}", errname(r), s.flags());
    let mut d = Dm::new("c2-lin", "", 0);
    let r = d.call(fd, DM_DEV_SUSPEND);
    let mut s = Dm::new("", "C2-UUID-LIN", 0);
    s.call(fd, DM_DEV_STATUS);
    check!(r == 0 && s.flags() & DM_ACTIVE_PRESENT_FLAG != 0 && s.flags() & DM_READONLY_FLAG != 0
           && s.get32(20) == 1 && size64(&node) == psize && &s.b[48..54] == b"c2-lin", "resume_live",
           "{} flags {:#x} targets {} size {}", errname(r), s.flags(), s.get32(20), size64(&node));
    let (mut a, mut b) = ([0u8; 8192], [0u8; 8192]);
    read_at(&node, 0, &mut a);
    read_at(&part, 0, &mut b);
    check!(a == b && a[1024..1028] == [0xe2, 0xe1, 0xf5, 0xe0], "linear_reads_partition");
    let mut sys = [0u8; 64];
    let fdn = open(cpath!("/sys/block/dm-{}/dm/name", dev & 0xff).as_ptr(), O_RDONLY, 0) as i32;
    let n = read(fdn, &mut sys);
    close(fdn);
    check!(n > 0 && &sys[..n as usize] == b"c2-lin\n", "sysfs_dm_name");

    // Table and status, deps, device list.
    let mut t = Dm::new("c2-lin", "", DM_STATUS_TABLE_FLAG);
    let r = t.call(fd, DM_TABLE_STATUS);
    let data = t.data();
    let want = cpath!("254:3 0");
    check!(r == 0 && t.get32(20) == 1 && data.len() > 40 && cfield(&data[24..40]) == b"linear"
           && cfield(&data[40..]) == want.as_bytes() && u64::from_le_bytes(data[8..16].try_into().unwrap()) == psect,
           "table_status", "{} {}", errname(r), Bytes(cfield(data.get(40..).unwrap_or(b""))));
    let mut t = Dm::new("c2-lin", "", 0);
    let r = t.call(fd, DM_TABLE_DEPS);
    let dd = t.data();
    check!(r == 0 && dd.len() >= 16 && dd[0] == 1 && u64::from_le_bytes(dd[8..16].try_into().unwrap()) == (3 | 254 << 8),
           "table_deps");

    // Mounting from the dm device makes it busy.
    let mnt = cpath!("/tmp/c2dm-mnt");
    mkdirat(AT_FDCWD, mnt.as_ptr(), 0o755);
    let m = mount(node.as_ptr(), mnt.as_ptr(), c"erofs".as_ptr() as *const u8, MS_RDONLY, core::ptr::null());
    let mut txt = [0u8; 16];
    let fdt = open(cpath!("{}/small.txt", Bytes(mnt.as_bytes())).as_ptr(), O_RDONLY, 0) as i32;
    let n = read(fdt, &mut txt);
    close(fdt);
    let mut rm = Dm::new("c2-lin", "", 0);
    let busy = rm.call(fd, DM_DEV_REMOVE);
    check!(m == 0 && n == 9 && &txt[..9] == b"hello c2\n" && busy == -(EBUSY as isize), "mount_and_busy",
           "mount {} read {} remove {}", errname(m), n, errname(busy));
    umount2(mnt.as_ptr(), 0);

    // Three targets: linear (8 sectors), zero (8), linear at offset 8.
    let (r, dev2) = create(fd, "c2-cat", "");
    let mut d = Dm::new("c2-cat", "", 0);
    let mut at = DM_SIZE;
    d.target(&mut at, 0, 8, "linear", b"254:3 0");
    d.target(&mut at, 8, 8, "zero", b"");
    d.target(&mut at, 16, 8, "linear", b"/dev/block/vda3 8");
    let l = d.call(fd, DM_TABLE_LOAD);
    let mut d = Dm::new("c2-cat", "", 0);
    let res = d.call(fd, DM_DEV_SUSPEND);
    let node2 = dm_node(dev2);
    let mut c = [0xffu8; 12288];
    let n = read_at(&node2, 0, &mut c);
    check!(r == 0 && l == 0 && res == 0 && n == 12288 && c[..4096] == b[..4096] && c[4096..8192].iter().all(|&x| x == 0)
           && c[8192..] == b[4096..8192], "multi_target", "create {} load {} resume {} read {}", errname(r), errname(l), errname(res), n);
    // Suspend, load a gap-free error table, resume: reads fail.
    let mut d = Dm::new("c2-cat", "", DM_SUSPEND_FLAG);
    let sus = d.call(fd, DM_DEV_SUSPEND);
    let mut s = Dm::new("c2-cat", "", 0);
    s.call(fd, DM_DEV_STATUS);
    let suspended = s.flags() & DM_SUSPEND_FLAG != 0;
    let mut d = Dm::new("c2-cat", "", 0);
    let mut at = DM_SIZE;
    d.target(&mut at, 0, 16, "error", b"");
    d.call(fd, DM_TABLE_LOAD);
    let mut d = Dm::new("c2-cat", "", 0);
    d.call(fd, DM_DEV_SUSPEND);
    let e = read_at(&node2, 0, &mut c[..512]);
    check!(sus == 0 && suspended && e == -(EIO as isize) && size64(&node2) == 16 * 512, "suspend_swap_error_target",
           "suspend {} flag {} read {}", errname(sus), suspended, errname(e));
    // A table with a gap is refused.
    let mut d = Dm::new("c2-cat", "", 0);
    let mut at = DM_SIZE;
    d.target(&mut at, 8, 8, "zero", b"");
    check_err!(d.call(fd, DM_TABLE_LOAD), EINVAL, "table_with_gap_einval");

    // verity: accepted, data passes through unchecked.
    let (r, dev3) = create(fd, "c2-verity", "");
    let mut d = Dm::new("c2-verity", "", DM_READONLY_FLAG);
    let mut at = DM_SIZE;
    let vp = cpath!("1 {} {} 4096 4096 {} {} sha256 {} 00", Bytes(part.as_bytes()), Bytes(part.as_bytes()), psect / 8, psect / 8,
                    Hex64);
    d.target(&mut at, 0, psect, "verity", vp.as_bytes());
    let l = d.call(fd, DM_TABLE_LOAD);
    let mut d = Dm::new("c2-verity", "", 0);
    d.call(fd, DM_DEV_SUSPEND);
    a.fill(0);
    let n = read_at(&dm_node(dev3), 0, &mut a);
    check!(r == 0 && l == 0 && n == 8192 && a == b, "verity_passthrough", "create {} load {} read {}", errname(r), errname(l), n);

    // Listing, rename, removal.
    let mut d = Dm::new("", "", 0);
    let r = d.call(fd, DM_LIST_DEVICES);
    let ld = d.data();
    check!(r == 0 && contains(ld, b"c2-lin\0") && contains(ld, b"c2-cat\0") && contains(ld, b"c2-verity\0"), "list_devices");
    let mut d = Dm::new("c2-cat", "", 0);
    d.b[DM_SIZE..DM_SIZE + 8].copy_from_slice(b"c2-cat2\0");
    let rn = d.call(fd, DM_DEV_RENAME);
    let mut s = Dm::new("c2-cat2", "", 0);
    let st2 = s.call(fd, DM_DEV_STATUS);
    let mut s = Dm::new("c2-cat", "", 0);
    let old = s.call(fd, DM_DEV_STATUS);
    check!(rn == 0 && st2 == 0 && old == -(ENXIO as isize), "rename",
           "rename {} new {} old {}", errname(rn), errname(st2), errname(old));
    let mut ok = true;
    for name in ["c2-lin", "c2-cat2", "c2-verity"] {
        let mut d = Dm::new(name, "", 0);
        ok &= d.call(fd, DM_DEV_REMOVE) == 0;
    }
    let gone = fstatat(AT_FDCWD, node.as_ptr(), &mut st, 0);
    let mut s = Dm::new("c2-lin", "", 0);
    check!(ok && gone == -(ENOENT as isize) && s.call(fd, DM_DEV_STATUS) == -(ENXIO as isize), "remove", "node {}", errname(gone));
    close(fd);
}

/// 64 hex zeros (a sha256 root digest placeholder).
struct Hex64;

impl core::fmt::Display for Hex64 {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        for _ in 0..64 {
            f.write_str("0")?;
        }
        Ok(())
    }
}
