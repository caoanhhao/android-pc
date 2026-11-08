#!/usr/bin/env python3
"""Write a "newc" cpio archive (the initramfs format) from a spec file.

Like the kernel's usr/gen_init_cpio, but needs no root to create device
nodes, and is byte-for-byte reproducible (fixed mtime/uid/gid, inode
numbers in spec order). Spec lines (paths are inside the archive):

    dir   <path> <mode>
    file  <path> <host-source> <mode>
    nod   <path> <mode> <c|b> <major> <minor>
    slink <path> <target> <mode>

Usage: mkcpio.py SPEC OUT [--mtime N]
"""
import argparse
import os
import stat
import sys


def header(ino, mode, nlink, mtime, size, rmaj, rmin, name):
    namesize = len(name) + 1
    fields = [ino, mode, 0, 0, nlink, mtime, size, 0, 0, rmaj, rmin, namesize, 0]
    h = b"070701" + b"".join(b"%08X" % f for f in fields)
    h += name + b"\0"
    return h + b"\0" * (-len(h) % 4)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("spec")
    ap.add_argument("out")
    ap.add_argument("--mtime", type=int, default=int(os.environ.get("SOURCE_DATE_EPOCH", "0")))
    a = ap.parse_args()

    out = bytearray()
    ino = 721
    with open(a.spec) as f:
        for lineno, line in enumerate(f, 1):
            parts = line.split()
            if not parts or parts[0].startswith("#"):
                continue
            kind, path = parts[0], parts[1].lstrip("/").encode()
            data, rmaj, rmin, nlink = b"", 0, 0, 1
            if kind == "dir":
                mode = stat.S_IFDIR | int(parts[2], 8)
                nlink = 2
            elif kind == "file":
                with open(parts[2], "rb") as src:
                    data = src.read()
                mode = stat.S_IFREG | int(parts[3], 8)
            elif kind == "nod":
                typ = stat.S_IFCHR if parts[3] == "c" else stat.S_IFBLK
                mode = typ | int(parts[2], 8)
                rmaj, rmin = int(parts[4]), int(parts[5])
            elif kind == "slink":
                data = parts[2].encode()
                mode = stat.S_IFLNK | int(parts[3], 8)
            else:
                sys.exit(f"{a.spec}:{lineno}: unknown entry type {kind!r}")
            out += header(ino, mode, nlink, a.mtime, len(data), rmaj, rmin, path)
            out += data + b"\0" * (-len(data) % 4)
            ino += 1
    out += header(0, 0, 1, 0, 0, 0, 0, b"TRAILER!!!")
    out += b"\0" * (-len(out) % 512)
    with open(a.out, "wb") as f:
        f.write(out)


if __name__ == "__main__":
    main()
