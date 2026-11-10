#!/usr/bin/env python3
"""Build a USB stick image with the android-pc layout, without root.

    mkusbimg.py --esp DIR --super super.img --out stick.img [--userdata-mib N]

GPT (1 MiB aligned) with, in order:
  1 ESP       1 GiB  FAT32 labelled ANDROIDPC, holding the files of DIR
                     (tools/mkusb.sh --stage DIR)
  2 super     8 GiB  super.img (copied by data extents: the image stays sparse)
  3 frp       1 MiB  zeroed
  4 misc      1 MiB  zeroed
  5 userdata  the rest (--userdata-mib, default 64), zeroed

The ESP is formatted by mkfs.fat (as the user formats the real stick) and
filled by a small FAT32 writer: one directory per path component, long
file names (VFAT LFN entries) where 8.3 does not fit, files in contiguous
clusters, both FATs. The disk GUID is fixed, so the kernel's boot-disk
match (Limine's executable file GPT disk GUID) is exercised the same way
on every run.
"""
import argparse
import os
import struct
import subprocess
import sys
import tempfile
import uuid
import zlib

MIB = 1 << 20
SECTOR = 512
ESP_TYPE = uuid.UUID("c12a7328-f81f-11d2-ba4b-00a0c93ec93b")
DATA_TYPE = uuid.UUID("0fc63daf-8483-4772-8e79-3d69d8477de4")
DISK_GUID = uuid.UUID("a9d70c3e-5b1f-4c0e-9b52-616e64726f69")


def layout(userdata_mib):
    """(name, type, first byte, size) of the five partitions."""
    parts, off = [], MIB
    for name, ty, size in [("ESP", ESP_TYPE, 1024 * MIB), ("super", DATA_TYPE, 8192 * MIB),
                           ("frp", DATA_TYPE, MIB), ("misc", DATA_TYPE, MIB),
                           ("userdata", DATA_TYPE, userdata_mib * MIB)]:
        parts.append((name, ty, off, size))
        off += size
    return parts, off + MIB  # room for the backup GPT


def gpt(parts, disk_bytes):
    """Protective MBR, primary and backup GPT: [(offset, bytes)]."""
    sectors = disk_bytes // SECTOR
    entries = bytearray(128 * 128)
    for i, (name, ty, off, size) in enumerate(parts):
        part_guid = uuid.uuid5(DISK_GUID, name)
        struct.pack_into("<16s16sQQQ72s", entries, i * 128, ty.bytes_le, part_guid.bytes_le,
                         off // SECTOR, (off + size) // SECTOR - 1, 0, name.encode("utf-16-le"))
    ecrc = zlib.crc32(entries)

    def header(my_lba, alt_lba, entries_lba):
        h = bytearray(struct.pack("<8sIIIIQQQQ16sQIII", b"EFI PART", 0x10000, 92, 0, 0, my_lba, alt_lba,
                                  34, sectors - 34, DISK_GUID.bytes_le, entries_lba, 128, 128, ecrc))
        struct.pack_into("<I", h, 16, zlib.crc32(h))
        return bytes(h) + bytes(SECTOR - len(h))

    mbr = bytearray(SECTOR)
    struct.pack_into("<B3sB3sII", mbr, 446, 0, b"\x00\x02\x00", 0xee, b"\xff\xff\xff", 1, min(sectors - 1, 0xffffffff))
    mbr[510:512] = b"\x55\xaa"
    return [(0, bytes(mbr)), (SECTOR, header(1, sectors - 1, 2)), (2 * SECTOR, bytes(entries)),
            ((sectors - 33) * SECTOR, bytes(entries)), ((sectors - 1) * SECTOR, header(sectors - 1, 1, sectors - 33))]


class Fat32:
    """Writes files into a freshly formatted FAT32 image (file object)."""

    def __init__(self, f):
        self.f = f
        b = self.read(0, SECTOR)
        self.bps, self.spc = struct.unpack_from("<HB", b, 11)
        self.rsvd, self.nfats = struct.unpack_from("<HB", b, 14)
        self.fatsz, = struct.unpack_from("<I", b, 36)
        self.root, = struct.unpack_from("<I", b, 44)
        total, = struct.unpack_from("<I", b, 32)
        if self.fatsz == 0 or b[82:87] != b"FAT32":
            raise SystemExit("mkusbimg: not a FAT32 file system")
        self.csize = self.bps * self.spc
        self.data = (self.rsvd + self.nfats * self.fatsz) * self.bps
        self.clusters = (total - self.rsvd - self.nfats * self.fatsz) // self.spc
        self.next_free = self.root + 1
        self.dirs = {"": self.root}
        self.fat = {}

    def read(self, off, n):
        self.f.seek(off)
        return self.f.read(n)

    def write(self, off, data):
        self.f.seek(off)
        self.f.write(data)

    def coff(self, c):
        return self.data + (c - 2) * self.csize

    def alloc(self, nbytes):
        n = max(1, -(-nbytes // self.csize))
        first = self.next_free
        if first + n - 2 > self.clusters:
            raise SystemExit("mkusbimg: ESP full")
        for c in range(first, first + n):
            self.fat[c] = c + 1 if c + 1 < first + n else 0x0fffffff
        self.next_free += n
        return first, n

    def flush_fat(self):
        for i in range(self.nfats):
            base = (self.rsvd + i * self.fatsz) * self.bps
            for c, v in self.fat.items():
                self.write(base + c * 4, struct.pack("<I", v))

    @staticmethod
    def short_ok(name):
        base, _, ext = name.partition(".")
        ok = set("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-~!#$%&'()@^{}")
        return 0 < len(base) <= 8 and len(ext) <= 3 and name.count(".") <= 1 and set(base.upper() + ext.upper()) <= ok

    def entries(self, name, attr, cluster, size, taken):
        """Directory entries (LFN ones first) for `name`."""
        if self.short_ok(name) and name.upper() == name:
            base, _, ext = name.partition(".")
            short, lfn = (base.ljust(8) + ext.ljust(3)).encode(), False
        elif self.short_ok(name):
            # Lower case 8.3: the NT case flags keep it without LFN entries.
            base, _, ext = name.partition(".")
            short, lfn = (base.upper().ljust(8) + ext.upper().ljust(3)).encode(), False
        else:
            stem = "".join(c for c in name.rpartition(".")[0] or name if c.isalnum()).upper()[:6] or "FILE"
            ext = "".join(c for c in name.rpartition(".")[2] if c.isalnum()).upper()[:3] if "." in name else ""
            i = 1
            while (stem + f"~{i}").ljust(8)[:8] + ext.ljust(3) in taken:
                i += 1
            short, lfn = ((stem + f"~{i}").ljust(8)[:8] + ext.ljust(3)).encode(), True
        taken.add(short.decode())
        out = []
        if lfn:
            csum = 0
            for ch in short:
                csum = (((csum & 1) << 7) + (csum >> 1) + ch) & 0xff
            units = list(name.encode("utf-16-le"))
            units = [units[i] | units[i + 1] << 8 for i in range(0, len(units), 2)] + [0]
            chunks = [units[i:i + 13] for i in range(0, len(units), 13)]
            for seq in range(len(chunks), 0, -1):
                c = chunks[seq - 1] + [0xffff] * (13 - len(chunks[seq - 1]))
                e = struct.pack("<B10sBBB12sH4s", seq | (0x40 if seq == len(chunks) else 0),
                                struct.pack("<5H", *c[0:5]), 0x0f, 0, csum, struct.pack("<6H", *c[5:11]), 0,
                                struct.pack("<2H", *c[11:13]))
                out.append(e)
        nt = 0
        if not lfn and name.upper() != name:
            base, _, ext = name.partition(".")
            nt = (0x08 if base.islower() else 0) | (0x10 if ext and ext.islower() else 0)
        out.append(struct.pack("<11sBBBHHHHHHHI", short, attr, nt, 0, 0, 0x5921, 0x5921, cluster >> 16, 0, 0x5921,
                               cluster & 0xffff, size))
        return out

    def add_entries(self, dir_cluster, ents):
        """Append entries to a (one-cluster) directory."""
        off = self.coff(dir_cluster)
        buf = self.read(off, self.csize)
        i = 0
        while i < self.csize and buf[i] not in (0, 0xe5):
            i += 32
        blob = b"".join(ents)
        if i + len(blob) > self.csize:
            raise SystemExit("mkusbimg: directory full")
        self.write(off + i, blob)

    def taken(self, dir_cluster):
        buf = self.read(self.coff(dir_cluster), self.csize)
        return {buf[i:i + 11].decode("latin-1") for i in range(0, self.csize, 32) if buf[i] not in (0, 0xe5)}

    def mkdir(self, path):
        if path in self.dirs:
            return self.dirs[path]
        parent, _, name = path.rpartition("/")
        pc = self.mkdir(parent)
        c, _ = self.alloc(self.csize)
        self.write(self.coff(c), bytes(self.csize))
        dot = struct.pack("<11sBBBHHHHHHHI", b".          ", 0x10, 0, 0, 0, 0x5921, 0x5921, c >> 16, 0, 0x5921, c & 0xffff, 0)
        pcl = 0 if pc == self.root else pc
        dotdot = struct.pack("<11sBBBHHHHHHHI", b"..         ", 0x10, 0, 0, 0, 0x5921, 0x5921, pcl >> 16, 0, 0x5921,
                             pcl & 0xffff, 0)
        self.write(self.coff(c), dot + dotdot)
        self.add_entries(pc, self.entries(name, 0x10, c, 0, self.taken(pc)))
        self.dirs[path] = c
        return c

    def add_file(self, path, data):
        parent, _, name = path.rpartition("/")
        pc = self.mkdir(parent)
        c, _ = self.alloc(len(data)) if data else (0, 0)
        if data:
            self.write(self.coff(c), data)
        self.add_entries(pc, self.entries(name, 0x20, c, len(data), self.taken(pc)))


def make_esp(src_dir, size, work_dir):
    """A FAT32 image (temporary file in work_dir) of `size` bytes with
    src_dir's files."""
    tmp = tempfile.NamedTemporaryFile(prefix="esp-", suffix=".img", dir=work_dir, delete=False)
    tmp.close()
    os.truncate(tmp.name, size)
    subprocess.run(["mkfs.fat", "-F", "32", "-n", "ANDROIDPC", "-s", "8", tmp.name], check=True,
                   stdout=subprocess.DEVNULL)
    with open(tmp.name, "r+b") as f:
        fs = Fat32(f)
        for root, dirs, files in os.walk(src_dir):
            dirs.sort()
            rel = os.path.relpath(root, src_dir)
            rel = "" if rel == "." else rel
            if rel:
                fs.mkdir(rel)
            for name in sorted(files):
                with open(os.path.join(root, name), "rb") as s:
                    fs.add_file(f"{rel}/{name}".lstrip("/"), s.read())
        fs.flush_fat()
        # FSInfo: free count and next free unknown.
        fsinfo = struct.unpack_from("<H", fs.read(0, SECTOR), 48)[0]
        fs.write(fsinfo * fs.bps + 488, struct.pack("<II", 0xffffffff, 0xffffffff))
    return tmp.name


def copy_sparse(src, dst_fd, dst_off, limit):
    """Copy the data extents of `src` to dst_fd at dst_off (holes stay)."""
    with open(src, "rb") as s:
        fd = s.fileno()
        end = os.fstat(fd).st_size
        if end > limit:
            raise SystemExit(f"mkusbimg: {src} is {end} bytes, the partition {limit}")
        off = 0
        while off < end:
            try:
                off = os.lseek(fd, off, os.SEEK_DATA)
            except OSError:
                break
            hole = os.lseek(fd, off, os.SEEK_HOLE)
            while off < hole:
                n = min(hole - off, 64 * MIB)
                os.pwrite(dst_fd, os.pread(fd, n, off), dst_off + off)
                off += n


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--esp", required=True, help="directory with the ESP's files")
    ap.add_argument("--super", required=True, help="AOSP super.img")
    ap.add_argument("--out", required=True)
    ap.add_argument("--userdata-mib", type=int, default=64)
    a = ap.parse_args()
    parts, size = layout(a.userdata_mib)
    if os.path.exists(a.out):
        os.unlink(a.out)
    esp = make_esp(a.esp, parts[0][3], os.path.dirname(os.path.abspath(a.out)))
    try:
        fd = os.open(a.out, os.O_RDWR | os.O_CREAT | os.O_TRUNC, 0o644)
        try:
            os.ftruncate(fd, size)
            for off, data in gpt(parts, size):
                os.pwrite(fd, data, off)
            copy_sparse(esp, fd, parts[0][2], parts[0][3])
            copy_sparse(a.super, fd, parts[1][2], parts[1][3])
        finally:
            os.close(fd)
    finally:
        os.unlink(esp)
    used = os.stat(a.out).st_blocks * 512
    print(f"mkusbimg: {a.out}: {size >> 20} MiB ({used >> 20} MiB allocated), "
          + ", ".join(f"{p[0]} {p[3] >> 20} MiB" for p in parts))


if __name__ == "__main__":
    sys.exit(main())
