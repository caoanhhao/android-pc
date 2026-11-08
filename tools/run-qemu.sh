#!/usr/bin/env bash
# Build a bootable Limine ISO (UEFI + BIOS) from the kernel and run it in QEMU.
#
#   tools/run-qemu.sh                 build (debug) and boot the kernel with
#                                     out/ktest-initramfs.cpio (if built)
#   tools/run-qemu.sh --test          build and run the in-kernel test suite
#   tools/run-qemu.sh --ktest [FILTER...]
#                                     build the test initramfs, boot it, run
#                                     ktest-init (all tests, or FILTER prefixes
#                                     such as b1) and exit 0 iff it reports
#                                     "END ktest-init ... fail=0"
#   tools/run-qemu.sh --aosp-sh-test  build out/aosp-initramfs.cpio from the
#                                     AOSP build (tools/mkaosp-initramfs.sh),
#                                     boot it and run tools/aosp-sh-test.py:
#                                     commands typed into the AOSP mksh on
#                                     the serial console, output checked
#   tools/run-qemu.sh --aosp          the same initramfs, interactive shell
#   tools/run-qemu.sh [opts] <ELF>    boot a given kernel ELF (cargo runner)
#
# Options: --release, --bios (legacy BIOS instead of UEFI), --no-kvm,
#          --smp N, --mem SIZE, --gdb (wait for gdb on :1234),
#          --initrd FILE (boot module, "none" for no initramfs),
#          --append "WORDS" (kernel cmdline: init=PATH, strace, -- init args),
#          --timeout SECS (tests; default 120, --ktest 600).
#          Extra args after `--` go to QEMU.
#
# Inside `kernel/`, `cargo run` and `cargo test` use this script as runner.
# Serial console is on stdio; quit an interactive run with Ctrl-A X.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
KERNEL_DIR="$ROOT/kernel"

mode=run profile=dev firmware=uefi kvm=auto smp=4 mem=2G gdb=0 timeout="" elf=""
initrd="" append="" ktest_filters=()
qemu_extra=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --test) mode=test-build ;;
        --release) profile=release ;;
        --bios) firmware=bios ;;
        --uefi) firmware=uefi ;;
        --no-kvm) kvm=0 ;;
        --smp) smp="$2"; shift ;;
        --mem) mem="$2"; shift ;;
        --gdb) gdb=1 ;;
        --timeout) timeout="$2"; shift ;;
        --initrd) initrd="$2"; shift ;;
        --append) append="$2"; shift ;;
        --aosp-sh-test) mode=aosp-test ;;
        --aosp) mode=aosp ;;
        --ktest)
            mode=ktest
            while [[ $# -gt 1 && $2 != -* ]]; do ktest_filters+=("$2"); shift; done ;;
        --) shift; qemu_extra=("$@"); break ;;
        -*) echo "run-qemu.sh: unknown option $1" >&2; exit 2 ;;
        *) elf="$1" ;;
    esac
    shift
done
[[ -z $timeout ]] && { timeout=120; [[ $mode == ktest ]] && timeout=600; }

# rust-toolchain.toml must win over a toolchain forced from the environment.
cargo_() { (cd "$KERNEL_DIR" && env -u RUSTUP_TOOLCHAIN cargo "$@"); }
fwd_opts=()
[[ $firmware == bios ]] && fwd_opts+=(--bios)
[[ $kvm == 0 ]] && fwd_opts+=(--no-kvm)
fwd_opts+=(--smp "$smp" --mem "$mem" --timeout "$timeout")

if [[ $mode == test-build ]]; then
    # cargo calls back into this script (runner) with the test kernel ELF.
    export RUN_QEMU_OPTS="${fwd_opts[*]}"
    exec bash -c 'cd "$1" && exec env -u RUSTUP_TOOLCHAIN cargo test --profile "$2"' _ "$KERNEL_DIR" "$profile"
fi
if [[ $mode == ktest ]]; then
    "$ROOT/tools/mkinitramfs.sh" >/dev/null
    [[ -z $initrd ]] && initrd="$ROOT/out/ktest-initramfs.cpio"
    [[ ${#ktest_filters[@]} -gt 0 ]] && append="$append -- ${ktest_filters[*]}"
fi
if [[ $mode == aosp-test || $mode == aosp ]]; then
    "$ROOT/tools/mkaosp-initramfs.sh" >/dev/null
    [[ -z $initrd ]] && initrd="$ROOT/out/aosp-initramfs.cpio"
    [[ $append == *init=* ]] || append="init=/init $append"
    [[ $mode == aosp ]] && mode=run
fi
if [[ -z $elf ]]; then
    cargo_ build --profile "$profile"
    dir=debug; [[ $profile == release ]] && dir=release
    elf="$KERNEL_DIR/target/x86_64-unknown-none/$dir/kernel"
else
    # Invoked as cargo runner: pick up options forwarded by `--test`.
    if [[ -n ${RUN_QEMU_OPTS:-} ]]; then
        # shellcheck disable=SC2086
        set -- $RUN_QEMU_OPTS
        while [[ $# -gt 0 ]]; do
            case "$1" in
                --bios) firmware=bios ;; --no-kvm) kvm=0 ;;
                --smp) smp="$2"; shift ;; --mem) mem="$2"; shift ;;
                --timeout) timeout="$2"; shift ;;
            esac
            shift
        done
    fi
    # cargo test binaries carry a hash suffix (kernel-0123abcd...).
    [[ $(basename "$elf") =~ -[0-9a-f]{16}$ ]] && mode=test
fi

# --- host tools -------------------------------------------------------------
missing=()
command -v qemu-system-x86_64 >/dev/null || missing+=(qemu-desktop)
command -v xorriso >/dev/null || missing+=(xorriso)
LIMINE_DIR="${LIMINE_DIR:-/usr/share/limine}"
[[ -f $LIMINE_DIR/limine-bios-cd.bin ]] && command -v limine >/dev/null || missing+=(limine)
OVMF_CODE="" OVMF_VARS=""
for d in /usr/share/edk2/x64 /usr/share/edk2-ovmf/x64 /usr/share/OVMF /usr/share/ovmf/x64; do
    if [[ -f $d/OVMF_CODE.4m.fd ]]; then OVMF_CODE=$d/OVMF_CODE.4m.fd OVMF_VARS=$d/OVMF_VARS.4m.fd; break; fi
    if [[ -f $d/OVMF_CODE.fd ]]; then OVMF_CODE=$d/OVMF_CODE.fd OVMF_VARS=$d/OVMF_VARS.fd; break; fi
done
[[ $firmware == uefi && -z $OVMF_CODE ]] && missing+=(edk2-ovmf)
if [[ ${#missing[@]} -gt 0 ]]; then
    echo "run-qemu.sh: missing host packages. Install them with:" >&2
    echo "    sudo pacman -S --needed ${missing[*]}" >&2
    exit 1
fi

# --- ISO image --------------------------------------------------------------
work="$(dirname "$elf")/qemu-$(basename "$elf")"
iso_root="$work/iso_root"
rm -rf "$iso_root"
mkdir -p "$iso_root/boot/limine" "$iso_root/EFI/BOOT"
cp "$elf" "$iso_root/boot/kernel"
# The test kernel never needs an initramfs; normal boots default to the
# test initramfs when it has been built (tools/mkinitramfs.sh).
if [[ -z $initrd && $mode == run && -f $ROOT/out/ktest-initramfs.cpio ]]; then
    initrd="$ROOT/out/ktest-initramfs.cpio"
fi
module_conf=""
if [[ -n $initrd && $initrd != none && $mode != test ]]; then
    cp "$initrd" "$iso_root/boot/initramfs.cpio"
    module_conf="    module_path: boot():/boot/initramfs.cpio"
fi
cat > "$iso_root/boot/limine/limine.conf" <<CONF
timeout: 0
verbose: no

/android-pc
    protocol: limine
    path: boot():/boot/kernel
    cmdline: ${append# }
$module_conf
CONF
cp "$LIMINE_DIR"/limine-bios.sys "$LIMINE_DIR"/limine-bios-cd.bin "$LIMINE_DIR"/limine-uefi-cd.bin \
    "$iso_root/boot/limine/"
cp "$LIMINE_DIR"/BOOTX64.EFI "$iso_root/EFI/BOOT/"
iso="$work/android-pc.iso"
xorriso -as mkisofs -R -r -J -b boot/limine/limine-bios-cd.bin \
    -no-emul-boot -boot-load-size 4 -boot-info-table -hfsplus \
    -apm-block-size 2048 --efi-boot boot/limine/limine-uefi-cd.bin \
    -efi-boot-part --efi-boot-image --protective-msdos-label \
    "$iso_root" -o "$iso" 2>/dev/null
limine bios-install "$iso" 2>/dev/null

# --- QEMU -------------------------------------------------------------------
args=(-machine q35 -smp "$smp" -m "$mem" -no-reboot
      -device isa-debug-exit,iobase=0xf4,iosize=0x04
      -cdrom "$iso" -boot d)
if [[ $kvm == auto && -r /dev/kvm && -w /dev/kvm ]]; then
    args+=(-enable-kvm -cpu host)
else
    args+=(-cpu max)
fi
if [[ $firmware == uefi ]]; then
    cp "$OVMF_VARS" "$work/ovmf-vars.fd"
    args+=(-drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE"
           -drive "if=pflash,format=raw,file=$work/ovmf-vars.fd")
fi
[[ $gdb == 1 ]] && args+=(-s -S)

if [[ $mode == ktest ]]; then
    # Serial to stdout and a log; the verdict is ktest-init's END line.
    log="$work/ktest.log"
    args+=(-display none -serial stdio)
    set +e
    timeout --foreground "$timeout" qemu-system-x86_64 "${args[@]}" "${qemu_extra[@]}" </dev/null | tee "$log"
    code=${PIPESTATUS[0]}
    set -e
    if grep -aqE '^END ktest-init pass=[0-9]+ fail=0' "$log"; then
        echo "run-qemu.sh: ktest-init passed ($(grep -a '^END ktest-init' "$log" | tr -d '\r'))"
        exit 0
    fi
    [[ $code == 124 ]] && echo "run-qemu.sh: timed out after ${timeout}s" >&2
    echo "run-qemu.sh: ktest-init FAILED (log: $log)" >&2
    exit 1
elif [[ $mode == aosp-test ]]; then
    log="$work/aosp-sh-test.log"
    args+=(-display none -serial stdio)
    if timeout --foreground $((timeout * 3)) python3 "$ROOT/tools/aosp-sh-test.py" --log "$log" \
        -- qemu-system-x86_64 "${args[@]}" "${qemu_extra[@]}"; then
        exit 0
    fi
    echo "run-qemu.sh: AOSP shell test FAILED (serial log: $log)" >&2
    exit 1
elif [[ $mode == test ]]; then
    args+=(-display none -serial stdio)
    set +e
    timeout --foreground "$timeout" qemu-system-x86_64 "${args[@]}" "${qemu_extra[@]}"
    code=$?
    set -e
    case $code in
        33) exit 0 ;;                                     # QemuExitCode::Success
        35) echo "run-qemu.sh: kernel tests FAILED" >&2; exit 1 ;;
        124) echo "run-qemu.sh: tests timed out after ${timeout}s" >&2; exit 1 ;;
        *) echo "run-qemu.sh: QEMU exited with status $code" >&2; exit 1 ;;
    esac
else
    args+=(-display none -serial mon:stdio)
    exec qemu-system-x86_64 "${args[@]}" "${qemu_extra[@]}"
fi
