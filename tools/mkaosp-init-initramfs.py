#!/usr/bin/env python3
"""Build the initramfs that boots unmodified AOSP /init on the Rust kernel.

The archive is AOSP's own generic ramdisk, taken unchanged out of
init_boot.img (first-stage /init, the ramdisk build.prop, snapuserd, ...),
plus one device file of ours: the first-stage fstab /fstab.rustpc, which
first-stage init finds through androidboot.fstab_suffix=rustpc. Cuttlefish's
vendor_boot ramdisk is not used: it only adds kernel modules (we have no
module loader) and an fstab with AVB, /metadata and /data entries we cannot
serve yet. The fstab mounts the logical partitions of super.img read-only
(dm-linear, erofs), without AVB, and tmpfs on /metadata and /data (empty
on every boot: there are no ext4 writes or file-based encryption yet).
First-stage init wants a block device for each of those two entries:
the kernel's RAM disks ram0 and ram1 (brd.rd_nr=2 on the command line),
which tmpfs ignores.

The same fstab (plus the raw /misc partition) also goes on the kernel
command line as dtfstab= (`--dtfstab` prints it): the kernel presents it as
a device-tree fstab, which first stage reads instead of /fstab.rustpc and
which stays readable after switch_root.

Usage: mkaosp-init-initramfs.py [-o OUT] [--product DIR] | --dtfstab

Default output: out/aosp-init-initramfs.cpio. Needs AOSP's host tools
unpack_bootimg and lz4 (aosp/out/host/linux-x86/bin).
"""
import argparse
import os
import stat
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Dynamic partitions of aosp_cf_x86_64_only_phone, as in Cuttlefish's
# fstab.cf.*: system and vendor are required, the rest optional.
FSTAB = """\
# First-stage fstab for the Rust kernel (see tools/mkaosp-init-initramfs.py).
# Logical partitions come from /dev/block/by-name/super
# (androidboot.partition_map=vda,super); no AVB, all read-only erofs.
# /metadata and /data are tmpfs (the RAM disks only satisfy first-stage
# init's wait for a block device).
/dev/block/ram0 /metadata tmpfs nodev,noatime,nosuid,mode=0755 wait,first_stage_mount
/dev/block/ram1 /data     tmpfs nodev,noatime,nosuid,mode=0771 wait,first_stage_mount
system      /system      erofs ro wait,logical,first_stage_mount,slotselect
vendor      /vendor      erofs ro wait,logical,first_stage_mount,slotselect
system_ext  /system_ext  erofs ro wait,logical,first_stage_mount,slotselect,nofail
product     /product     erofs ro wait,logical,first_stage_mount,slotselect,nofail
odm         /odm         erofs ro wait,logical,first_stage_mount,slotselect,nofail
vendor_dlkm /vendor_dlkm erofs ro wait,logical,first_stage_mount,slotselect,nofail
odm_dlkm    /odm_dlkm    erofs ro wait,logical,first_stage_mount,slotselect,nofail
system_dlkm /system_dlkm erofs ro wait,logical,first_stage_mount,slotselect,nofail
# Raw partitions (skipped by first stage and mount_all): the boot HAL's misc.
/dev/block/by-name/misc /misc emmc defaults defaults
"""


def dtfstab():
    """FSTAB as the kernel's dtfstab= parameter: NAME:DEV:MNT_POINT:TYPE:
    MNT_FLAGS:FSMGR_FLAGS entries separated by ';'. The kernel shows them in
    the device-tree fstab layout androidboot.android_dt_dir points to, so the
    entries (/misc in particular) stay in fs_mgr's default fstab after first
    stage, when the ramdisk's /fstab.rustpc is gone."""
    out = []
    for line in FSTAB.splitlines():
        f = line.split()
        if not f or f[0].startswith("#"):
            continue
        dev, mnt, ty, mnt_flags, fsmgr = f
        out.append(":".join([mnt.strip("/").replace("/", "_"), dev, mnt, ty, mnt_flags, fsmgr]))
    return ";".join(out)


def aosp_dir():
    out = subprocess.check_output(["git", "worktree", "list", "--porcelain"], cwd=ROOT, text=True)
    main = out.splitlines()[0].removeprefix("worktree ")
    return os.path.join(main, "aosp")


def read_cpio(data):
    """Entries of a newc archive: (name, mode, uid, gid, mtime, rmaj, rmin, data)."""
    off, entries = 0, []
    while True:
        h = data[off:off + 110]
        if h[:6] not in (b"070701", b"070702"):
            sys.exit(f"bad cpio header at {off}")
        f = [int(h[6 + i * 8:14 + i * 8], 16) for i in range(13)]
        namesize, filesize = f[11], f[6]
        name = data[off + 110:off + 110 + namesize - 1]
        start = (off + 110 + namesize + 3) & ~3
        body = data[start:start + filesize]
        off = (start + filesize + 3) & ~3
        if name == b"TRAILER!!!":
            return entries
        entries.append((name, f[1], f[2], f[3], f[5], f[9], f[10], body))


def write_cpio(entries):
    out = bytearray()
    for ino, (name, mode, uid, gid, mtime, rmaj, rmin, body) in enumerate(entries + [(b"TRAILER!!!", 0, 0, 0, 0, 0, 0, b"")], 721):
        nlink = 2 if stat.S_ISDIR(mode) else 1
        fields = [ino, mode, uid, gid, nlink, mtime, len(body), 0, 0, rmaj, rmin, len(name) + 1, 0]
        h = b"070701" + b"".join(b"%08X" % x for x in fields) + name + b"\0"
        out += h + b"\0" * (-len(h) % 4) + body + b"\0" * (-len(body) % 4)
    return bytes(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("-o", dest="out", default=os.path.join(ROOT, "out", "aosp-init-initramfs.cpio"))
    ap.add_argument("--product")
    ap.add_argument("--dtfstab", action="store_true", help="print the kernel's dtfstab= value and exit")
    a = ap.parse_args()
    if a.dtfstab:
        print(dtfstab())
        return
    aosp = aosp_dir()
    product = a.product or os.environ.get("AOSP_PRODUCT_OUT") or os.path.join(aosp, "out/target/product/vsoc_x86_64_only")
    host = os.path.join(aosp, "out/host/linux-x86/bin")
    img = os.path.join(product, "init_boot.img")
    for p in (img, os.path.join(host, "unpack_bootimg"), os.path.join(host, "lz4")):
        if not os.path.exists(p):
            sys.exit(f"mkaosp-init-initramfs: missing {p} (build AOSP first, see docs/aosp-build.md)")
    if os.path.exists(a.out) and os.path.getmtime(a.out) > max(os.path.getmtime(img), os.path.getmtime(__file__)):
        print(f"{a.out} (up to date)")
        return
    with tempfile.TemporaryDirectory() as tmp:
        subprocess.run([os.path.join(host, "unpack_bootimg"), "--boot_img", img, "--out", tmp],
                       check=True, stdout=subprocess.DEVNULL)
        ramdisk = subprocess.run([os.path.join(host, "lz4"), "-dc", os.path.join(tmp, "ramdisk")],
                                 check=True, stdout=subprocess.PIPE).stdout
    entries = [e for e in read_cpio(ramdisk) if e[0] not in (b".", b"fstab.rustpc")]
    entries.append((b"fstab.rustpc", stat.S_IFREG | 0o644, 0, 0, 0, 0, 0, FSTAB.encode()))
    os.makedirs(os.path.dirname(a.out), exist_ok=True)
    with open(a.out + ".tmp", "wb") as f:
        f.write(write_cpio(entries))
    os.replace(a.out + ".tmp", a.out)
    print(f"{a.out} ({os.path.getsize(a.out) // 1024} KiB, {len(entries)} entries)")


if __name__ == "__main__":
    main()
