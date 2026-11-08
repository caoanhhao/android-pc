#!/usr/bin/env bash
# Run a build command under the project's resource limits:
#   12 CPU cores (CPUs 0-11: 8 P-cores + 4 E-cores, hyperthreading off), 25 GB RAM, abort if free space on / drops below 10 GB.
# Usage: tools/limited-build.sh <command> [args...]
# Example: tools/limited-build.sh m -j12 systemimage
set -euo pipefail

CORES=${BUILD_CORES:-12}
MEM=${BUILD_MEM:-25G}
DISK_FLOOR_GB=${BUILD_DISK_FLOOR_GB:-10}

free_gb() { df -BG --output=avail / | tail -1 | tr -dc '0-9'; }

if (( $(free_gb) < DISK_FLOOR_GB )); then
    echo "limited-build: only $(free_gb) GB free on /, floor is ${DISK_FLOOR_GB} GB" >&2
    exit 1
fi

unit="limited-build-$$"
export NINJA_ARGS="${NINJA_ARGS:--j${CORES}}"
export CARGO_BUILD_JOBS="${CORES}"
export MAKEFLAGS="-j${CORES}"

systemd-run --user --scope --quiet --unit="$unit" \
    -p AllowedCPUs="0-$((CORES - 1))" \
    -p CPUQuota="$((CORES * 100))%" \
    -p MemoryMax="$MEM" -p MemorySwapMax=4G \
    "$@" &
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
