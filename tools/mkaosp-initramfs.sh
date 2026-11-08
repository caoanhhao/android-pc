#!/usr/bin/env bash
# Pack unmodified AOSP userspace (mksh, toybox, bionic) into a cpio
# initramfs for the Rust kernel, from an existing AOSP build. Nothing is
# rebuilt.
#
#   tools/mkaosp-initramfs.sh [-o OUT] [--product DIR]
#
# Source: aosp/out/target/product/vsoc_x86_64_only (override with --product
# or $AOSP_PRODUCT_OUT). The runtime APEX (linker64, libc, libm, libdl) is
# extracted from system/apex/com.android.runtime.apex with AOSP's host
# fsck.erofs (aosp/out/host/linux-x86/bin).
#
# Layout (real Android paths):
#   /init                         mksh script: mounts, then `exec sh`
#   /system/bin/{sh,toybox}       + one symlink per toybox applet
#   /system/bin/linker64       -> /apex/com.android.runtime/bin/linker64
#   /system/lib64/*.so            toybox's NEEDED closure (libc etc. are
#                                 symlinks into the runtime APEX, as on device)
#   /apex/com.android.runtime/{bin,lib64,lib64/bionic}
#   /linkerconfig/ld.config.txt   one non-isolated namespace on /system/lib64
#   /dev /proc /sys /tmp /data    mount points (+ console/null nodes)
# Output defaults to out/aosp-initramfs.cpio. When the interactive shell
# exits, init exits and the kernel powers off.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
aosp="$(cd "$root" && git worktree list --porcelain | sed -n '1s/^worktree //p')/aosp"
product="${AOSP_PRODUCT_OUT:-$aosp/out/target/product/vsoc_x86_64_only}"
host_bin="$aosp/out/host/linux-x86/bin"
out="$root/out/aosp-initramfs.cpio"
while [ $# -gt 0 ]; do
    case "$1" in
        -o) out="$2"; shift 2 ;;
        --product) product="$2"; shift 2 ;;
        -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
sys="$product/system"
for f in "$sys/bin/sh" "$sys/bin/toybox" "$sys/apex/com.android.runtime.apex" "$host_bin/fsck.erofs"; do
    [ -e "$f" ] || { echo "mkaosp-initramfs: missing $f (build AOSP first, see docs/aosp-build.md)" >&2; exit 1; }
done

stage="$(mktemp -d)"
spec="$stage/spec"
trap 'rm -rf "$stage" "$out.tmp"' EXIT

# Runtime APEX: zip -> apex_payload.img (erofs) -> directory tree.
rt="$stage/runtime"
unzip -q -o "$sys/apex/com.android.runtime.apex" apex_payload.img -d "$stage"
"$host_bin/fsck.erofs" --extract="$rt" "$stage/apex_payload.img" >/dev/null

# Map an on-device path to the host file that provides it.
host_path() {
    case "$1" in
        /apex/com.android.runtime/*) echo "$rt/${1#/apex/com.android.runtime/}" ;;
        /system/*) echo "$sys/${1#/system/}" ;;
        *) return 1 ;;
    esac
}

declare -A seen=()
emit_file() { # device-path [mode]
    local dev="$1" mode="${2:-0755}" src
    [ -n "${seen[$dev]:-}" ] && return
    seen[$dev]=1
    src="$(host_path "$dev")"
    if [ -L "$src" ]; then
        local tgt
        tgt="$(readlink "$src")"
        echo "slink $dev $tgt 0777" >>"$spec"
        case "$tgt" in /*) emit_file "$tgt" "$mode" ;; esac
    else
        [ -f "$src" ] || { echo "mkaosp-initramfs: no host file for $dev ($src)" >&2; exit 1; }
        echo "file $dev $src $mode" >>"$spec"
    fi
}

resolve_dev() { # follow on-device symlinks to the host file
    local d="$1" s
    for _ in 1 2 3 4; do
        s="$(host_path "$d")"
        [ -L "$s" ] || { echo "$s"; return; }
        d="$(readlink "$s")"
    done
    echo "mkaosp-initramfs: symlink loop at $1" >&2; exit 1
}

{
    for d in /dev /proc /sys /data /system /system/bin /system/lib64 /system/etc \
             /apex /apex/com.android.runtime /apex/com.android.runtime/bin \
             /apex/com.android.runtime/lib64 /apex/com.android.runtime/lib64/bionic \
             /linkerconfig; do
        echo "dir $d 0755"
    done
    echo "dir /tmp 1777"
    echo "nod /dev/console 0600 c 5 1"
    echo "nod /dev/null 0666 c 1 3"
    echo "slink /bin /system/bin 0777"
    echo "slink /etc /system/etc 0777"
    echo "slink /sbin /system/bin 0777"
} >"$spec"

emit_file /system/bin/linker64
emit_file /system/bin/sh
emit_file /system/bin/toybox
# Shared-library closure, searched like the linker's default namespace
# (/system/lib64; libc & co. are symlinks into the runtime APEX there).
queue=("$sys/bin/sh" "$sys/bin/toybox")
i=0
while [ $i -lt ${#queue[@]} ]; do
    for lib in $(readelf -d "${queue[$i]}" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p'); do
        [ -n "${seen[/system/lib64/$lib]:-}" ] && continue
        emit_file "/system/lib64/$lib" 0644
        queue+=("$(resolve_dev "/system/lib64/$lib")")
    done
    i=$((i + 1))
done

# Toybox applets: every system/bin symlink to toybox in the build.
for l in "$sys"/bin/*; do
    [ -L "$l" ] && [ "$(readlink "$l")" = toybox ] || continue
    n="$(basename "$l")"
    [ -n "${seen[/system/bin/$n]:-}" ] && continue
    seen[/system/bin/$n]=1
    echo "slink /system/bin/$n toybox 0777" >>"$spec"
done

cat >"$stage/ld.config.txt" <<'EOF'
# Minimal linker config for the initramfs: everything in /system/bin uses one
# non-isolated namespace that searches /system/lib64 (libc & co. are symlinks
# into /apex/com.android.runtime there, as on a device).
dir.system = /system/bin/
dir.system = /apex/com.android.runtime/bin/
[system]
additional.namespaces =
namespace.default.isolated = false
namespace.default.search.paths = /system/${LIB}
EOF
echo "file /linkerconfig/ld.config.txt $stage/ld.config.txt 0644" >>"$spec"

cat >"$stage/init" <<'EOF'
#!/system/bin/sh
# initramfs /init: mount the kernel filesystems, then run the shell.
export PATH=/system/bin HOME=/ TMPDIR=/tmp ANDROID_ROOT=/system
# Full path: until /proc is mounted, bionic's linker finds the executable
# by stat()ing argv[0] instead of /proc/self/exe.
/system/bin/mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev
mount -t tmpfs tmpfs /tmp
mount -t tmpfs tmpfs /data
cd /
echo "aosp-init: ready ($(uname -sr))"
exec sh -i
EOF
echo "file /init $stage/init 0755" >>"$spec"

mkdir -p "$(dirname "$out")"
python3 "$root/tools/mkcpio.py" "$spec" "$out.tmp"
mv "$out.tmp" "$out"
echo "$out ($(du -h "$out" | cut -f1), $(grep -c '^file' "$spec") files)"
