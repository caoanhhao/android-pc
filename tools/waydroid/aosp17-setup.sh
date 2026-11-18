#!/bin/bash
# Prepare this repo's AOSP 17 tree (aosp/) so the PC images also run in Waydroid
# (docs/waydroid-aosp17.md):
#   1. fetch waydroid-preinit into external/waydroid-preinit and Waydroid's composer and
#      HIDL interfaces into hardware/waydroid (pinned commits);
#   2. apply tools/waydroid/aosp17-patches/<project>.patch to each AOSP project, unless
#      already applied. Every change there is a run-time branch taken only inside Waydroid's
#      container (/vendor/waydroid.prop exists), so the PC behaves as before.
# Usage: tools/waydroid/aosp17-setup.sh [--check]   (--check: only report patch state)
set -euo pipefail
shopt -s nullglob
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
AOSP=$ROOT/aosp
PATCHES=$ROOT/tools/waydroid/aosp17-patches
CHECK=${1:-}

INIT_REPO=https://github.com/waydroid/android_vendor_waydroid_init
INIT_COMMIT=8d1d42cf42c567c6944f68970faab7a578e9c0ad   # dev/lineage-23.2
HW_REPO=https://github.com/waydroid/android_hardware_waydroid
HW_COMMIT=3f8653798de35f534ff6c5ae90368aab432c18b1     # dev/lineage-23.2

# fetch REPO COMMIT DEST RSYNC_FILTER...: a pinned subset of a git repository.
fetch() {
    local repo=$1 commit=$2 dst=$3; shift 3
    [ "$(cat "$dst/.commit" 2>/dev/null)" = "$commit" ] && return 0
    local tmp
    tmp=$(mktemp -d)
    git -C "$tmp" init -q
    git -C "$tmp" fetch -q --depth 1 "$repo" "$commit"
    git -C "$tmp" checkout -q FETCH_HEAD
    mkdir -p "$dst"
    rsync -a --delete --exclude .git "$@" --exclude '*' "$tmp/" "$dst/"
    echo "$commit" > "$dst/.commit"
    /usr/bin/rm -rf "$tmp"
    echo "fetched $repo $commit -> $dst"
}

if [ "$CHECK" != --check ]; then
    fetch "$INIT_REPO" "$INIT_COMMIT" "$AOSP/external/waydroid-preinit" \
        --exclude Android.bp --include 'common/***' --include 'waydroid-preinit/***' --include LICENSE
    cp "$ROOT/tools/waydroid/preinit/Android.bp" "$AOSP/external/waydroid-preinit/Android.bp"
    # The composer and its HIDL interfaces only: the other HALs there are Android.mk modules
    # that need libraries this tree does not have.
    fetch "$HW_REPO" "$HW_COMMIT" "$AOSP/hardware/waydroid" \
        --include 'hwcomposer/***' --include 'interfaces/***'
    # libsync's LLNDK stubs lack sw_sync_*: link them from aosp-device/waydroid instead.
    grep -q libwaydroid_sw_sync "$AOSP/hardware/waydroid/hwcomposer/Android.bp" ||
        sed -i '0,/static_libs: \[/s//static_libs: [\n        "libwaydroid_sw_sync",/' \
            "$AOSP/hardware/waydroid/hwcomposer/Android.bp"
fi

for patch in "$PATCHES"/*.patch; do
    proj=$(basename "$patch" .patch | tr '+' '/')
    if git -C "$AOSP/$proj" apply --check -R "$patch" 2>/dev/null; then
        echo "applied:     $proj"
    elif git -C "$AOSP/$proj" apply --check "$patch" 2>/dev/null; then
        if [ "$CHECK" = --check ]; then
            echo "not applied: $proj"
        else
            git -C "$AOSP/$proj" apply "$patch"
            echo "applying:    $proj"
        fi
    else
        echo "CONFLICT:    $proj ($patch does not apply)" >&2
        exit 1
    fi
done
