#!/usr/bin/env python3
"""Build the C2 block/file-system test images (no root needed).

    tools/mkc2-images.py [--out DIR] [--hello ELF]

Writes into DIR (default out/c2):
  tree/                the source tree (files of many sizes, a sparse file,
                       300-entry directory, deep path, short and long
                       symlinks, a hard link, modes, user.* xattrs, and a
                       copy of a static test ELF as bin/hello)
  c2.ext4              ext4, 4 KiB blocks (mke2fs -d)
  c2-1k.ext3           ext3 (indirect blocks), 1 KiB blocks, directories
                       htree-indexed by `e2fsck -fD`
  c2.erofs             erofs, uncompressed
  c2-lz4.erofs         erofs, lz4hc (compact indexes), like AOSP's images
  c2-lz4-legacy.erofs  erofs, lz4 with full (legacy) indexes
  c2-disk.img          GPT disk with those five images as partitions
                       (names c2_ext4, c2_ext3_1k, c2_erofs, c2_erofs_lz4,
                       c2_erofs_legacy) plus an empty 4 MiB c2_scratch
The ext4 tools come from the host (e2fsprogs), mkfs.erofs from the AOSP
build (aosp/out/host/linux-x86/bin). Outputs are rebuilt only when this
script or the ELF is newer than c2-disk.img.
"""
import argparse
import os
import shutil
import struct
import subprocess
import sys
import uuid
import zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MIB = 1024 * 1024
PARTS = [
    ("c2_ext4", "c2.ext4"),
    ("c2_ext3_1k", "c2-1k.ext3"),
    ("c2_erofs", "c2.erofs"),
    ("c2_erofs_lz4", "c2-lz4.erofs"),
    ("c2_erofs_legacy", "c2-lz4-legacy.erofs"),
    ("c2_scratch", None),
]


def aosp_bin(name):
    top = subprocess.run(["git", "-C", ROOT, "worktree", "list", "--porcelain"], capture_output=True, text=True).stdout
    main = top.splitlines()[0].split(" ", 1)[1]
    p = os.path.join(main, "aosp/out/host/linux-x86/bin", name)
    if not os.path.exists(p):
        sys.exit(f"mkc2-images: missing {p} (build AOSP host tools, see docs/aosp-build.md)")
    return p


def rnd(seed, n):
    """Deterministic incompressible bytes."""
    out = bytearray()
    x = seed
    while len(out) < n:
        x = (x * 6364136223846793005 + 1442695040888963407) & (2**64 - 1)
        out += struct.pack("<Q", x)
    return bytes(out[:n])


def make_tree(t, hello):
    if os.path.exists(t):
        shutil.rmtree(t)
    os.makedirs(t)
    w = lambda p, data, mode=0o644: (open(os.path.join(t, p), "wb").write(data), os.chmod(os.path.join(t, p), mode))
    w("small.txt", b"hello c2\n")
    w("empty", b"")
    w("page.bin", rnd(1, 4096))
    w("page1.bin", rnd(2, 4097))
    w("random.bin", rnd(3, MIB + 123))
    text = b"".join(b"line %06d: the quick brown fox jumps over the lazy dog\n" % i for i in range(6000))
    w("text.txt", text)
    w("secret", b"mode 0600\n", 0o600)
    w("run.sh", b"#!/system/bin/sh\necho run\n", 0o755)
    # Sparse: 64 KiB data, 1 MiB hole, 8 KiB data, hole to 3 MiB.
    with open(os.path.join(t, "sparse.bin"), "wb") as f:
        f.write(rnd(4, 65536))
        f.seek(65536 + MIB)
        f.write(rnd(5, 8192))
        f.truncate(3 * MIB)
    os.makedirs(os.path.join(t, "many"))
    for i in range(300):
        w(f"many/file-{i:03d}-{'x' * (i % 40)}", b"%d\n" % i)
    os.makedirs(os.path.join(t, "a/b/c/d"))
    w("a/b/c/d/deep.txt", b"deep\n")
    os.symlink("small.txt", os.path.join(t, "link"))
    os.symlink("a/b/c/d/" + "../" * 4 + "a/b/c/d/" + "./" * 30 + "deep.txt", os.path.join(t, "longlink"))
    os.symlink("a/b", os.path.join(t, "dirlink"))
    os.link(os.path.join(t, "small.txt"), os.path.join(t, "hard.txt"))
    os.setxattr(os.path.join(t, "small.txt"), "user.c2", b"hello-xattr")
    os.setxattr(os.path.join(t, "a"), "user.big", b"x" * 200)
    os.makedirs(os.path.join(t, "bin"))
    shutil.copyfile(hello, os.path.join(t, "bin/hello"))
    os.chmod(os.path.join(t, "bin/hello"), 0o755)
    os.chmod(t, 0o755)


def run(*cmd):
    subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL)


def make_images(out, t):
    env_uuid = "c2c2c2c2-0000-4000-8000-00000000c2c2"
    ext4 = os.path.join(out, "c2.ext4")
    for p in (ext4, os.path.join(out, "c2-1k.ext3")):
        if os.path.exists(p):
            os.remove(p)
    os.environ["E2FSPROGS_FAKE_TIME"] = "1700000000"
    run("mke2fs", "-q", "-F", "-t", "ext4", "-b", "4096", "-I", "256", "-U", env_uuid, "-L", "c2ext4",
        "-E", "root_owner=0:0", "-d", t, ext4, "16M")
    ext3 = os.path.join(out, "c2-1k.ext3")
    run("mke2fs", "-q", "-F", "-t", "ext3", "-b", "1024", "-I", "256", "-U", env_uuid, "-d", t, ext3, "24M")
    r = subprocess.run(["e2fsck", "-fyD", ext3], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if r.returncode not in (0, 1):
        sys.exit(f"mkc2-images: e2fsck -fD failed ({r.returncode})")
    mkerofs = aosp_bin("mkfs.erofs")
    common = ["--quiet", "-T", "1700000000", "-U", env_uuid, "--all-root"]
    run(mkerofs, *common, os.path.join(out, "c2.erofs"), t)
    run(mkerofs, *common, "-zlz4hc,9", os.path.join(out, "c2-lz4.erofs"), t)
    run(mkerofs, *common, "-zlz4", "-Elegacy-compress", os.path.join(out, "c2-lz4-legacy.erofs"), t)


def guid(s):
    return uuid.UUID(s).bytes_le


def make_gpt(path, parts):
    """parts: [(name, image path or None, size)] -> GPT disk image."""
    linux = guid("0fc63daf-8483-4772-8e79-3d69d8477de4")
    entries = b""
    lba = 2048
    layout = []
    for i, (name, img, size) in enumerate(parts):
        nsect = (size + MIB - 1) // MIB * MIB // 512
        first, last = lba, lba + nsect - 1
        layout.append((first, img))
        uniq = uuid.UUID(int=0xC2 << 120 | i + 1).bytes_le
        nm = name.encode("utf-16-le").ljust(72, b"\0")
        entries += linux + uniq + struct.pack("<QQQ", first, last, 0) + nm
        lba = last + 1
    entries = entries.ljust(128 * 128, b"\0")
    total = lba + 2048
    ecrc = zlib.crc32(entries)

    def header(cur, backup, entries_lba):
        h = struct.pack("<8sIIIIQQQQ16sQIII", b"EFI PART", 0x10000, 92, 0, 0, cur, backup, 2048, total - 34,
                        guid("c2c2c2c2-1111-4000-8000-000000000001"), entries_lba, 128, 128, ecrc)
        h = h[:16] + struct.pack("<I", zlib.crc32(h)) + h[20:]
        return h.ljust(512, b"\0")

    with open(path + ".tmp", "wb") as f:
        f.truncate(total * 512)
        mbr = bytearray(512)
        mbr[446:462] = struct.pack("<BBBBBBBBII", 0, 0, 2, 0, 0xEE, 0xFF, 0xFF, 0xFF, 1, min(total - 1, 0xFFFFFFFF))
        mbr[510:512] = b"\x55\xaa"
        f.seek(0)
        f.write(mbr)
        f.write(header(1, total - 1, 2))
        f.write(entries)
        f.seek((total - 33) * 512)
        f.write(entries)
        f.write(header(total - 1, 1, total - 33))
        for first, img in layout:
            if img:
                f.seek(first * 512)
                with open(img, "rb") as src:
                    shutil.copyfileobj(src, f)
    os.replace(path + ".tmp", path)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", default=os.path.join(ROOT, "out/c2"))
    ap.add_argument("--hello", default=os.path.join(ROOT, "tests/target/x86_64-unknown-linux-gnu/release/b1_hello"))
    ap.add_argument("--force", action="store_true")
    a = ap.parse_args()
    disk = os.path.join(a.out, "c2-disk.img")
    deps = [os.path.abspath(__file__), a.hello]
    if not a.force and os.path.exists(disk) and all(os.path.getmtime(d) <= os.path.getmtime(disk) for d in deps):
        print(disk)
        return
    os.makedirs(a.out, exist_ok=True)
    tree = os.path.join(a.out, "tree")
    make_tree(tree, a.hello)
    make_images(a.out, tree)
    parts = []
    for name, img in PARTS:
        p = os.path.join(a.out, img) if img else None
        parts.append((name, p, os.path.getsize(p) if p else 4 * MIB))
    make_gpt(disk, parts)
    print(disk)


if __name__ == "__main__":
    main()
