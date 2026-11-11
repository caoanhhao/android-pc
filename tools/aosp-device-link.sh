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
DST=$ROOT/aosp/device/android-pc/pc_x86_64
mkdir -p "$DST"
rsync -a --delete "$ROOT/aosp-device/" "$DST/"
# Modules that must sit under vendor/ for visibility reasons (see their Android.bp).
mkdir -p "$ROOT/aosp/vendor/android-pc"
rsync -a --delete "$ROOT/aosp-device-vendor/" "$ROOT/aosp/vendor/android-pc/"
