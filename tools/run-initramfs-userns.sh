#!/usr/bin/env bash
# Smoke-test an initramfs on the host kernel without root or a VM: unpack it,
# then run its /init as PID 1 inside new user/pid/mount/net namespaces with
# the host /dev bind-mounted (device nodes can't be created unprivileged).
# This checks the archive and ktest-init's PID-1 path (mounts, reaping,
# power-off), using Linux as the oracle.
#
#   tools/run-initramfs-userns.sh [CPIO] [-- FILTER...]
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cpio_file="$root/out/ktest-initramfs.cpio"
if [ $# -gt 0 ] && [ "$1" != "--" ]; then cpio_file="$(realpath "$1")"; shift; fi
[ "${1:-}" = "--" ] && shift
[ -f "$cpio_file" ] || { echo "missing $cpio_file; run tools/mkinitramfs.sh first" >&2; exit 1; }

dir="$(mktemp -d)"
trap 'rm -rf "$dir"' EXIT
case "$cpio_file" in
    *.gz) gzip -dc "$cpio_file" ;;
    *) cat "$cpio_file" ;;
esac | (cd "$dir" && cpio -id --quiet 2>/dev/null || true)
mkdir -p "$dir/dev"

log="$dir.log"
unshare --user --map-root-user --pid --fork --mount --net --kill-child \
    sh -c 'mount --rbind /dev "$0/dev" && exec chroot "$0" /init "$@"' "$dir" "$@" | tee "$log"
# The last END line is /init's own (ktest-init, or the single test).
end="$(grep '^END ' "$log" | tail -n1 || true)"
rm -f "$log"
[[ "$end" == *" fail=0" ]] || { echo "initramfs run failed: ${end:-no END line}" >&2; exit 1; }
