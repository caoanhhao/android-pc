#!/usr/bin/env bash
# Kernel for the Galaxy Tab S7 LTE (SM-T875, gts7l): mainline Linux v7.3 plus our patch
# series (aosp-device/gts7l/kernel/patches, forked from JeyKul/gts7xl-mainline), configured
# as arm64 defconfig + aosp-device/gts7l/kernel/gts7l.fragment, built with clang/LLVM.
#
# Steps (default: all, in order):
#   fetch   shallow clone of the v7.3 tag into gts7l-kernel/ at the main checkout root
#   patch   branch gts7l = v7.3 + `git am` of the series (skipped when already applied;
#           re-applied from v7.3 when the series changed)
#   config  defconfig + gts7l.fragment, checked: every fragment value must survive
#   build   Image.gz, modules and the gts7l/gts7xlwifi DTBs through tools/limited-build.sh
#   install out/gts7l/: Image.gz, the DTBs, Image.gz-dtb (gts7l), Image.gz-dtb.gts7xlwifi,
#           modules/ (stripped, with modules.load)
#
# Toolchain: LLVM=1 when the host has clang and ld.lld; otherwise the newest AOSP prebuilt
# clang under aosp/prebuilts or gki/prebuilts (the Arch host has clang but no `lld` package).
# Override with GTS7L_LLVM=/path/to/clang/bin/ (trailing slash) or GTS7L_LLVM=1.
# Other knobs: GTS7L_KERNEL_DIR (source tree), GTS7L_OUT (output dir), JOBS (default 8, the
# E-cores; never more than 8, see CLAUDE.md).
#
# Usage: tools/gts7l-kernel.sh [fetch|patch|config|build|install|all]...
set -euo pipefail

REPO=$(cd "$(dirname "$0")/.." && pwd)
MAIN=$(dirname "$(cd "$REPO" && realpath "$(git rev-parse --git-common-dir)")")
KDIR=${GTS7L_KERNEL_DIR:-$MAIN/gts7l-kernel}
OUT=${GTS7L_OUT:-$MAIN/out/gts7l}
KOUT=$KDIR/out
SERIES=$REPO/aosp-device/gts7l/kernel/patches
FRAGMENT=$REPO/aosp-device/gts7l/kernel/gts7l.fragment
TAG=v7.3
URL=${GTS7L_KERNEL_URL:-https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git}
JOBS=${JOBS:-8}
DTBS=(qcom/sm8250-samsung-gts7l.dtb qcom/sm8250-samsung-gts7xlwifi.dtb)

die() { echo "gts7l-kernel: $*" >&2; exit 1; }
log() { echo "==> $*"; }

(( JOBS <= 8 )) || die "JOBS=$JOBS: at most 8 (E-cores only)"

pick_llvm() {
    if [[ -n ${GTS7L_LLVM:-} ]]; then echo "$GTS7L_LLVM"; return; fi
    if command -v clang >/dev/null && command -v ld.lld >/dev/null; then echo 1; return; fi
    local d
    d=$(ls -d "$MAIN"/aosp/prebuilts/clang/host/linux-x86/clang-r* \
              "$MAIN"/gki/prebuilts/clang/host/linux-x86/clang-r* 2>/dev/null |
        while read -r p; do [[ -x $p/bin/ld.lld ]] && echo "$(basename "$p") $p"; done |
        sort -V | tail -1 | cut -d' ' -f2)
    [[ -n $d ]] || die "no ld.lld: install the 'lld' package or set GTS7L_LLVM"
    echo "$d/bin/"
}

# LOCALVERSION= (empty) keeps setlocalversion from adding "+" for the git tree: 7.3.0-gts7l
kmake() { make -C "$KDIR" O="$KOUT" ARCH=arm64 LLVM="$LLVM" LOCALVERSION= "$@"; }

series_id() { cat "$SERIES"/*.patch | sha256sum | cut -c1-16; }

do_fetch() {
    if [[ -d $KDIR/.git ]]; then log "fetch: $KDIR exists"; return; fi
    log "fetch: $TAG from $URL"
    git clone --depth 1 --branch "$TAG" "$URL" "$KDIR"
}

do_patch() {
    local id stamp=$KDIR/.git/gts7l-series
    id=$(series_id)
    cd "$KDIR"
    if [[ -f $stamp && $(cat "$stamp") == "$id" && $(git rev-parse --abbrev-ref HEAD) == gts7l ]]; then
        log "patch: series $id already applied"; return
    fi
    [[ -z $(git status --porcelain --untracked-files=no) ]] || die "$KDIR has local changes; commit or drop them first"
    git am --abort 2>/dev/null || true
    log "patch: gts7l = $TAG + $(ls "$SERIES"/*.patch | wc -l) patches"
    git checkout -q -B gts7l "$TAG"
    # Our committer identity; authorship stays with each patch's From: line
    git -c user.name="$(git -C "$REPO" config user.name)" -c user.email="$(git -C "$REPO" config user.email)" \
        am -q --keep-cr "$SERIES"/*.patch
    echo "$id" > "$stamp"
}

do_config() {
    log "config: arm64 defconfig + $(basename "$FRAGMENT")"
    mkdir -p "$KOUT"
    kmake -s defconfig
    "$KDIR"/scripts/kconfig/merge_config.sh -m -O "$KOUT" "$KOUT/.config" "$FRAGMENT" >/dev/null
    kmake -s olddefconfig
    # merge_config only warns; fail when a fragment value did not survive olddefconfig
    local bad=0 line sym want got
    while IFS= read -r line; do
        if [[ $line =~ ^(CONFIG_[A-Za-z0-9_]+)=(.*)$ ]]; then
            sym=${BASH_REMATCH[1]}; want=${BASH_REMATCH[2]}
        elif [[ $line =~ ^#\ (CONFIG_[A-Za-z0-9_]+)\ is\ not\ set$ ]]; then
            sym=${BASH_REMATCH[1]}; want=n
        else continue; fi
        [[ $want == n ]] && want=""
        got=$(grep -E "^$sym=" "$KOUT/.config" | cut -d= -f2- || true)
        if [[ $got != "$want" ]]; then
            echo "  $sym: fragment ${want:-n}, got ${got:-n}" >&2; bad=1
        fi
    done < "$FRAGMENT"
    (( bad == 0 )) || die "fragment values lost (missing dependencies above)"
}

do_build() {
    if pgrep -x 'soong_ui|ninja|cargo|rustc|gradle|siso' >/dev/null; then
        die "another build is running ($(pgrep -xl 'soong_ui|ninja|cargo|rustc|gradle|siso' | head -3 | tr '\n' ' '))"
    fi
    log "build: Image.gz modules ${DTBS[*]} (LLVM=$LLVM, -j$JOBS)"
    "$REPO"/tools/limited-build.sh make -C "$KDIR" O="$KOUT" ARCH=arm64 LLVM="$LLVM" LOCALVERSION= \
        -j"$JOBS" Image.gz modules "${DTBS[@]}"
}

do_install() {
    local b=$KOUT/arch/arm64/boot d stage
    log "install: $OUT"
    rm -rf "$OUT/modules" && mkdir -p "$OUT/modules"
    cp "$b/Image.gz" "$OUT/"
    for d in "${DTBS[@]}"; do cp "$b/dts/$d" "$OUT/"; done
    # Samsung's ABL boots a v1 boot image whose kernel is Image.gz with the DTB appended
    cat "$b/Image.gz" "$b/dts/qcom/sm8250-samsung-gts7l.dtb" > "$OUT/Image.gz-dtb"
    cat "$b/Image.gz" "$b/dts/qcom/sm8250-samsung-gts7xlwifi.dtb" > "$OUT/Image.gz-dtb.gts7xlwifi"
    stage=$(mktemp -d); trap '/usr/bin/rm -rf "$stage"' RETURN
    kmake -s INSTALL_MOD_PATH="$stage" INSTALL_MOD_STRIP=1 modules_install
    find "$stage/lib/modules" -name '*.ko' -exec cp {} "$OUT/modules/" \;
    (cd "$OUT/modules" && ls ./*.ko | sed 's|^\./||' > modules.load)
    cp "$KOUT/.config" "$OUT/config"
    kmake -s kernelrelease > "$OUT/kernelrelease"
    log "install: $(cat "$OUT/kernelrelease"), Image.gz-dtb $(stat -c %s "$OUT/Image.gz-dtb") bytes, $(wc -l < "$OUT/modules/modules.load") modules"
}

LLVM=$(pick_llvm)
steps=("$@"); (( ${#steps[@]} )) || steps=(all)
for s in "${steps[@]}"; do
    case $s in
        all) do_fetch; do_patch; do_config; do_build; do_install ;;
        fetch|patch|config|build|install) "do_$s" ;;
        *) die "unknown step '$s' (fetch|patch|config|build|install|all)" ;;
    esac
done
