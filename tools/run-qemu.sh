#!/usr/bin/env bash
# Build a bootable Limine ISO (UEFI + BIOS) from the kernel and run it in QEMU.
#
#   tools/run-qemu.sh                 build (debug) and boot the kernel
#   tools/run-qemu.sh --test          build and run the in-kernel test suite
#   tools/run-qemu.sh [opts] <ELF>    boot a given kernel ELF (cargo runner)
#
# Options: --release, --bios (legacy BIOS instead of UEFI), --no-kvm,
#          --smp N, --mem SIZE, --gdb (wait for gdb on :1234),
#          --timeout SECS (tests; default 120). Extra args after `--` go to QEMU.
#
# Inside `kernel/`, `cargo run` and `cargo test` use this script as runner.
# Serial console is on stdio; quit an interactive run with Ctrl-A X.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
KERNEL_DIR="$ROOT/kernel"

mode=run profile=dev firmware=uefi kvm=auto smp=4 mem=2G gdb=0 timeout=120 elf=""
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
        --) shift; qemu_extra=("$@"); break ;;
        -*) echo "run-qemu.sh: unknown option $1" >&2; exit 2 ;;
        *) elf="$1" ;;
    esac
    shift
done

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
cat > "$iso_root/boot/limine/limine.conf" <<CONF
timeout: 0
verbose: no

/android-pc
    protocol: limine
    path: boot():/boot/kernel
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

if [[ $mode == test ]]; then
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
