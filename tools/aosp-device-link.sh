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
# Run from a git worktree (e.g. ../android-pc-gts7l), the worktree is the source while aosp/
# and out/ are the main checkout's.
WORKTREE=$ROOT
[[ -d $ROOT/aosp ]] || ROOT="$(cd "$(git -C "$ROOT" rev-parse --path-format=absolute --git-common-dir)/.." && pwd)"
# While parallel sessions share this checkout (Phase E), builds take out/.phase-e.lock with
# flock. The mirror is the build's input, so refreshing it outside the lock would change
# another session's running build: refuse unless the lock is held by this process or one of
# its ancestors. Without the lock file (no parallel sessions) nothing is checked.
LOCK=$ROOT/out/.phase-e.lock
if [[ -e $LOCK && -z ${HYPR_LINK_NO_LOCK:-} ]]; then
    # lslocks lists waiting flock processes too; they have no children yet, so no harm.
    holders=$(lslocks -n -r -o PID,PATH 2>/dev/null | awk -v l="$(realpath "$LOCK")" '$2 == l { print $1 }')
    held=
    for pid in $holders; do
        p=$$
        while [[ $p -gt 1 ]]; do
            [[ $p == "$pid" ]] && { held=1; break 2; }
            p=$(awk '{ print $4 }' "/proc/$p/stat" 2>/dev/null || echo 1)
        done
    done
    if [[ -z $held ]]; then
        echo "aosp-device-link.sh: run builds inside the shared lock:" >&2
        echo "  flock $LOCK tools/aosp17-build.sh ...   (lock processes: $(echo ${holders:-none}))" >&2
        exit 1
    fi
fi
# AOSP_DEVICE_SRC=DIR mirrors DIR/aosp-device and DIR/aosp-device-vendor instead (e.g. a
# `git archive HEAD` export, so other sessions' uncommitted work stays out of a release build).
SRC=${AOSP_DEVICE_SRC:-$WORKTREE}
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
# The gts7l kernel (tools/gts7l-kernel.sh writes out/gts7l/ of the main checkout): Image.gz,
# the gts7l DTB (alone in dtb/, the boot image's dtb section) and modules/ for aosp-device/gts7l
# (aosp_gts7l.mk, BoardConfig.mk), which use a placeholder without it.
if [[ -f $ROOT/out/gts7l/Image.gz && -f $ROOT/out/gts7l/sm8250-samsung-gts7l.dtb ]]; then
    K=$ROOT/aosp/device/android-pc/gts7l-kernel
    mkdir -p "$K/dtb"
    rsync -a --delete --include=/Image.gz --include=/dtb/ --include='/modules/***' --exclude='*' \
        "$ROOT/out/gts7l/" "$K/"
    rsync -a --delete "$ROOT/out/gts7l/sm8250-samsung-gts7l.dtb" "$K/dtb/"
    # The modules' vendor package (gts7l_kernel_modules).
    install -m 0644 "$SRC/aosp-device/gts7l/kernel/modules.bp" "$K/Android.bp"
fi
# Mesa freedreno/Turnip for gts7l (tools/gts7l-mesa.sh writes out/gts7l-mesa/prebuilt/: the
# libraries and their Android.bp), which aosp_gts7l.mk picks up when the Android.bp is there.
if [[ -f $ROOT/out/gts7l-mesa/prebuilt/Android.bp ]]; then
    mkdir -p "$ROOT/aosp/device/android-pc/gts7l-mesa"
    rsync -a --delete "$ROOT/out/gts7l-mesa/prebuilt/" "$ROOT/aosp/device/android-pc/gts7l-mesa/"
fi
# Google Play Store for gts7l (tools/gts7l-playstore.sh writes out/gts7l-playstore/prebuilt/),
# which aosp_gts7l.mk installs when the Android.bp is there. Proprietary: never in git.
if [[ -f $ROOT/out/gts7l-playstore/prebuilt/Android.bp ]]; then
    mkdir -p "$ROOT/aosp/device/android-pc/gts7l-playstore"
    rsync -a --delete "$ROOT/out/gts7l-playstore/prebuilt/" "$ROOT/aosp/device/android-pc/gts7l-playstore/"
fi
