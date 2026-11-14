#!/bin/bash
# Mirror aosp-device/ into the AOSP tree as device/android-pc/pc_x86_64 and
# aosp-device-vendor/ as vendor/android-pc (real copies).
# Not a symlink: Soong's finder skips symlinked directories (following them needs
# ALLOW_BP_UNDER_SYMLINKS, which also walks AOSP's own symlink loops), and a farm of
# per-file symlinks is no better: Soong installs init_rc and other prebuilt files by
# copying the symlink itself, so the image would point back into this repo.
# The copy keeps mtimes, so ninja rebuilds only what changed. tools/aosp17-build.sh runs
# this before every build; never edit the mirror, edit aosp-device/.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# AOSP_DEVICE_SRC=DIR mirrors DIR/aosp-device and DIR/aosp-device-vendor instead (e.g. a
# `git archive HEAD` export, so other sessions' uncommitted work stays out of a release build).
SRC=${AOSP_DEVICE_SRC:-$ROOT}
DST=$ROOT/aosp/device/android-pc/pc_x86_64
mkdir -p "$DST"
# EXCLUDE_HALS="wifi camera" leaves those hals/ directories out (another session's
# work in progress must not break this build's Soong analysis).
excludes=(); for hal in ${EXCLUDE_HALS:-}; do excludes+=(--exclude "/hals/$hal/"); done
rsync -a --delete --delete-excluded "${excludes[@]}" "$SRC/aosp-device/" "$DST/"
# Modules that must sit under vendor/ because their dependencies are only visible there
# (libpcfmq re-exports libfmq_rust for the audio HAL).
mkdir -p "$ROOT/aosp/vendor/android-pc"
rsync -a --delete "$SRC/aosp-device-vendor/" "$ROOT/aosp/vendor/android-pc/"
