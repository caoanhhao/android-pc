#!/usr/bin/env bash
# Pack the test programs into a cpio initramfs for the Rust kernel.
#
#   tools/mkinitramfs.sh [-o OUT] [--init NAME] [--gzip]
#
# Default /init is ktest-init, which mounts proc/sys/devtmpfs/tmpfs, runs
# every test in tests/tests.list, prints "END ktest-init pass=N fail=M" and
# powers off. Pass extra kernel cmdline words after "--" to filter tests,
# e.g. "... -- b1" runs only the B1 tests.
# --init NAME makes a single test binary /init instead (early bring-up,
# before fork/execve work), e.g. --init b1_hello.
#
# Layout:  /init  /tests/<name>  /dev/{console,null,zero,random,urandom,tty}
#          /proc /sys /tmp (empty mount points)
# Output defaults to out/ktest-initramfs.cpio[.gz]. The archive is
# reproducible: same binaries -> same bytes.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
out=""
init="ktest-init"
gzip=0
while [ $# -gt 0 ]; do
    case "$1" in
        -o) out="$2"; shift 2 ;;
        --init) init="$2"; shift 2 ;;
        --gzip) gzip=1; shift ;;
        -h|--help) sed -n '2,17p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
[ -n "$out" ] || out="$root/out/ktest-initramfs.cpio$([ $gzip -eq 1 ] && echo .gz || true)"

(cd "$root/tests" && cargo build --release --quiet)
bin="$root/tests/target/x86_64-unknown-linux-gnu/release"
grep -qx "$init" <(echo ktest-init; cat "$root/tests/tests.list") || { echo "no such test: $init" >&2; exit 1; }

# Strip debug info for the guest (keep the symbol table for backtraces);
# symbolize guest faults with the unstripped binaries in $bin.
stage="$(mktemp -d)"
spec="$stage/spec"
trap 'rm -rf "$stage" "$out.tmp"' EXIT
for f in "$bin"/ktest-init $(grep -Ev '^\s*(#|$)' "$root/tests/tests.list" | sed "s|^|$bin/|"); do
    strip --strip-debug -o "$stage/$(basename "$f")" "$f"
done
{
    echo "dir /dev 0755"
    echo "nod /dev/console 0600 c 5 1"
    echo "nod /dev/tty 0666 c 5 0"
    echo "nod /dev/null 0666 c 1 3"
    echo "nod /dev/zero 0666 c 1 5"
    echo "nod /dev/random 0666 c 1 8"
    echo "nod /dev/urandom 0666 c 1 9"
    echo "dir /proc 0555"
    echo "dir /sys 0555"
    echo "dir /tmp 1777"
    echo "dir /tests 0755"
    echo "file /init $stage/$init 0755"
    echo "file /tests/ktest-init $stage/ktest-init 0755"
    grep -Ev '^\s*(#|$)' "$root/tests/tests.list" | while read -r name; do
        echo "file /tests/$name $stage/$name 0755"
    done
} >"$spec"

mkdir -p "$(dirname "$out")"
python3 "$root/tools/mkcpio.py" "$spec" "$out.tmp"
if [ $gzip -eq 1 ]; then
    gzip -9 -n -c "$out.tmp" >"$out"
else
    mv "$out.tmp" "$out"
fi
echo "$out ($(du -h "$out" | cut -f1), /init = $init)"
