#!/usr/bin/env bash
# Run a build command under the project's resource limits:
#   the 8 E-cores (CPUs 8-15, 4.2 GHz), 20 GB RAM, nice 10, low I/O weight; abort if free
#   space on / drops below 10 GB.
# The host's i7-13700K (hyperthreading off: CPUs 0-7 are P-cores up to 5.3-5.4 GHz,
# 8-15 E-cores) has Raptor Lake Vmin degradation: builds on the P-cores froze the host or
# oopsed its kernel (docs/aosp-build.md, "Flaky host"). The E-cores run at lower voltage.
# The user manager has no cpuset controller, so systemd's AllowedCPUs is silently ignored;
# taskset sets the affinity, which every child inherits.
# BUILD_CPUS / BUILD_MEM override the defaults (e.g. BUILD_CPUS=4-15 for a faster run).
# Usage: tools/limited-build.sh <command> [args...]
# Example: tools/limited-build.sh m -j8 systemimage
set -euo pipefail

CPUS=${BUILD_CPUS:-8-15}
CORES=0; IFS=, read -ra _r <<<"$CPUS"
for r in "${_r[@]}"; do if [[ $r == *-* ]]; then CORES=$(( CORES + ${r#*-} - ${r%-*} + 1 )); else CORES=$(( CORES + 1 )); fi; done
MEM=${BUILD_MEM:-20G}
DISK_FLOOR_GB=${BUILD_DISK_FLOOR_GB:-10}

free_gb() { df -BG --output=avail / | tail -1 | tr -dc '0-9'; }

if (( $(free_gb) < DISK_FLOOR_GB )); then
    echo "limited-build: only $(free_gb) GB free on /, floor is ${DISK_FLOOR_GB} GB" >&2
    exit 1
fi

unit="limited-build-$$"
export NINJA_ARGS="${NINJA_ARGS--j${CORES}}"   # set NINJA_ARGS= (empty) for siso (AOSP 17), which rejects -jN
export CARGO_BUILD_JOBS="${CORES}"
export MAKEFLAGS="-j${CORES}"

systemd-run --user --scope --quiet --unit="$unit" --nice=10 \
    -p AllowedCPUs="$CPUS" \
    -p CPUQuota="$((CORES * 100))%" \
    -p MemoryHigh="$MEM" -p MemoryMax="$MEM" -p MemorySwapMax=4G \
    -p IOWeight=20 \
    taskset -c "$CPUS" "$@" <&0 &   # keep stdin (bash gives background jobs /dev/null)
pid=$!

# Disk guard: stop the whole scope if free space falls under the floor.
while kill -0 "$pid" 2>/dev/null; do
    if (( $(free_gb) < DISK_FLOOR_GB )); then
        echo "limited-build: free space under ${DISK_FLOOR_GB} GB, stopping $unit" >&2
        systemctl --user kill --signal=SIGTERM "$unit.scope" 2>/dev/null || true
        wait "$pid" || true
        exit 2
    fi
    sleep 15
done
wait "$pid"
