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
#                                     "END ktest-init ... fail=0"; attaches
#                                     the C2 test disk (out/c2/c2-disk.img)
#   tools/run-qemu.sh --aosp-sh-test  build out/aosp-initramfs.cpio from the
#                                     AOSP build (tools/mkaosp-initramfs.sh),
#                                     boot it and run tools/aosp-sh-test.py:
#                                     commands typed into the AOSP mksh on
#                                     the serial console, output checked
#   tools/run-qemu.sh --aosp          the same initramfs, interactive shell
#   tools/run-qemu.sh --aosp-disk-test
#                                     AOSP's super.img on a read-only virtio
#                                     disk: dmctl maps system_a/vendor_a from
#                                     the dynamic partition metadata, both are
#                                     mounted (erofs), toybox runs from the
#                                     image and every file's md5 is checked
#                                     against the host (aosp-sh-test.py
#                                     --suite disk)
#   tools/run-qemu.sh --aosp-init     unmodified AOSP /init (init_boot.img's
#                                     ramdisk + our first-stage fstab,
#                                     tools/mkaosp-init-initramfs.py) with
#                                     super.img as vda: first stage mounts
#                                     system/vendor via dm-linear, second
#                                     stage runs init.rc; interactive
#   tools/run-qemu.sh --aosp-init-test
#                                     the same, checked by
#                                     tools/aosp-init-test.py: boot
#                                     milestones on the serial log, then
#                                     getprop/setprop in init's console shell
#   tools/run-qemu.sh --aosp-adb-test the same boot with the host port forwarded
#                                     to adbd: tools/aosp-adb-test.py waits
#                                     for adbd, connects the host adb (its
#                                     own server) and runs shell commands
#   tools/run-qemu.sh --aosp-gfx-test the adb test's boot with a virtio-gpu
#                                     display: tools/aosp-gfx-test.py checks
#                                     the graphics HALs and surfaceflinger
#                                     run, takes `adb shell screencap -p`
#                                     and a QEMU screendump and checks both
#                                     are not blank (zygote is stopped:
#                                     --aosp-boot-test covers the framework)
#   tools/run-qemu.sh --aosp-input-test
#                                     the graphics test's boot (virtio-gpu,
#                                     virtio-input devices, monitor):
#                                     tools/aosp-input-test.py checks the
#                                     /dev/input nodes, `getevent -lp`, keys
#                                     typed with the monitor's `sendkey` and
#                                     `mouse_button` reaching getevent, and
#                                     `adb exec-out` (text, 2 MB, a
#                                     `screencap -p` PNG)
#   tools/run-qemu.sh --aosp-boot-test
#                                     the graphics test's boot with zygote
#                                     left running (6 GiB, 6 CPUs):
#                                     tools/aosp-boot-test.py waits for
#                                     system_server and sys.boot_completed=1,
#                                     checks the boot animation exited, the
#                                     framework services (service list,
#                                     dumpsys activity/window, pm path) and
#                                     that system_server and zygote stay up
#   tools/run-qemu.sh --aosp-apk-test the boot test's machine plus a monitor:
#                                     builds apps/hello (apps/hello/build.sh)
#                                     and tools/aosp-apk-test.py checks the
#                                     launcher on screen, `adb install` of
#                                     the APK (installd, dex2oat), `am start`
#                                     of its activity, resumed, visible on
#                                     screencap and the scanout (D3b)
#   tools/run-qemu.sh [opts] <ELF>    boot a given kernel ELF (cargo runner)
#
# Every VM has a virtio-net card (PCI 00:03.0) on QEMU's user-mode network:
# the guest is 10.0.2.15, the gateway 10.0.2.2.
#
# Options: --release, --bios (legacy BIOS instead of UEFI), --no-kvm,
#          --smp N, --mem SIZE, --gdb (wait for gdb on :1234),
#          --initrd FILE (boot module, "none" for no initramfs),
#          --append "WORDS" (kernel cmdline: init=PATH, strace, -- init args),
#          --timeout SECS (tests; default 120, --ktest 600),
#          --disk FILE[,rw|,snapshot][,legacy] (repeatable): attach a raw
#          image as a virtio-blk disk (vda, vdb, ... in order). Read-only
#          unless ",rw" (writes reach FILE) or ",snapshot" (writes go to a
#          throw-away overlay); ",legacy" makes the device legacy-only
#          (virtio 0.9 I/O ports) instead of modern.
#          --gpu (virtio-gpu-pci at 00:02.0, no VGA; default for --ktest
#          and --aosp-gfx-test), --no-gpu, --gpu-size WxH (scanout size,
#          default 720x1280), --display none|vnc=:N|gtk|sdl (default none;
#          the screen can always be saved with the monitor's screendump),
#          --input (virtio-keyboard-pci 00:10.0, virtio-tablet-pci 00:11.0,
#          virtio-multitouch-pci 00:12.0; default for --ktest and the
#          --aosp-gfx-test/--aosp-input-test boots), --no-input,
#          --monitor PATH (QEMU monitor on a unix socket, e.g. for
#          `echo "screendump out.ppm" | socat - UNIX:PATH`),
#          --no-net (no network card), --adb-port PORT (forward host
#          127.0.0.1:PORT to the guest's TCP 5555, adbd; default 15555
#          for --aosp-init*/--aosp-adb-test, none otherwise),
#          --sh-arg ARG (repeatable): extra argument for aosp-sh-test.py,
#          e.g. --sh-arg --run --sh-arg "ls /dev/block".
#          Extra args after `--` go to QEMU.
#
# Inside `kernel/`, `cargo run` and `cargo test` use this script as runner.
# Serial console is on stdio; quit an interactive run with Ctrl-A X.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
KERNEL_DIR="$ROOT/kernel"

mode=run profile=dev firmware=uefi kvm=auto smp=4 mem=2G gdb=0 timeout="" elf=""
initrd="" append="" ktest_filters=() disks=() sh_args=() net=1 adb_port=
gpu="" input="" gpu_size=720x1280 display=none monitor=""
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
        --disk) disks+=("$2"); shift ;;
        --sh-arg) sh_args+=("$2"); shift ;;
        --no-net) net=0 ;;
        --gpu) gpu=1 ;;
        --no-gpu) gpu=0 ;;
        --input) input=1 ;;
        --no-input) input=0 ;;
        --gpu-size) gpu_size="$2"; shift ;;
        --display) display="$2"; shift ;;
        --monitor) monitor="$2"; shift ;;
        --aosp-gfx-test) mode=aosp-gfx-test ;;
        --aosp-input-test) mode=aosp-input-test ;;
        --aosp-boot-test) mode=aosp-boot-test ;;
        --aosp-apk-test) mode=aosp-apk-test ;;
        --adb-port) adb_port="$2"; shift ;;
        --aosp-adb-test) mode=aosp-adb-test ;;
        --aosp-sh-test) mode=aosp-test ;;
        --aosp) mode=aosp ;;
        --aosp-disk-test) mode=aosp-disk-test ;;
        --aosp-init) mode=aosp-init ;;
        --aosp-init-test) mode=aosp-init-test ;;
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
[[ -z $gpu && ( $mode == ktest || $mode == aosp-gfx-test || $mode == aosp-input-test || $mode == aosp-boot-test || $mode == aosp-apk-test ) ]] && gpu=1
[[ -z $input && ( $mode == ktest || $mode == aosp-gfx-test || $mode == aosp-input-test || $mode == aosp-boot-test || $mode == aosp-apk-test ) ]] && input=1
if [[ $mode == ktest ]]; then
    "$ROOT/tools/mkinitramfs.sh" >/dev/null
    [[ -z $initrd ]] && initrd="$ROOT/out/ktest-initramfs.cpio"
    # The C2 test disk (built by mkinitramfs.sh): vda modern and writable
    # through an overlay, vdb the same image legacy-only and read-only.
    if [[ ${#disks[@]} -eq 0 && -f $ROOT/out/c2/c2-disk.img ]]; then
        disks=("$ROOT/out/c2/c2-disk.img,snapshot" "$ROOT/out/c2/c2-disk.img,legacy")
    fi
    [[ ${#ktest_filters[@]} -gt 0 ]] && append="$append -- ${ktest_filters[*]}"
fi
if [[ $mode == aosp-disk-test ]]; then
    aosp_top="$(cd "$ROOT" && git worktree list --porcelain | sed -n '1s/^worktree //p')/aosp"
    product="${AOSP_PRODUCT_OUT:-$aosp_top/out/target/product/vsoc_x86_64_only}"
    "$ROOT/tools/mkaosp-initramfs.sh" --disk >/dev/null
    [[ -z $initrd ]] && initrd="$ROOT/out/aosp-disk-initramfs.cpio"
    [[ ${#disks[@]} -eq 0 ]] && disks=("$product/super.img")
    sh_args=(--suite disk "${sh_args[@]}")
    mode=aosp-test
fi
if [[ $mode == aosp-apk-test ]]; then
    # The APK under test, built from source (reproducible, a few seconds).
    "$ROOT/apps/hello/build.sh" >/dev/null
fi
if [[ $mode == aosp-init || $mode == aosp-init-test || $mode == aosp-adb-test || $mode == aosp-gfx-test || $mode == aosp-input-test || $mode == aosp-boot-test || $mode == aosp-apk-test ]]; then
    [[ -z $adb_port ]] && adb_port=15555
    aosp_top="$(cd "$ROOT" && git worktree list --porcelain | sed -n '1s/^worktree //p')/aosp"
    product="${AOSP_PRODUCT_OUT:-$aosp_top/out/target/product/vsoc_x86_64_only}"
    python3 "$ROOT/tools/mkaosp-init-initramfs.py" >/dev/null
    [[ -z $initrd ]] && initrd="$ROOT/out/aosp-init-initramfs.cpio"
    # Read-only: first-stage init only reads the dynamic partitions.
    # Two small partitions Cuttlefish keeps on its persistent disk, as
    # empty 1 MiB images written through snapshots: vdb (00:05.0) is the
    # factory reset protection partition ro.frp.pst names (system_server's
    # PersistentDataBlockService formats it), vdc (00:06.0) is misc (the
    # boot HAL keeps its A/B slot metadata there; vold waits for that HAL).
    if [[ ${#disks[@]} -eq 0 ]]; then
        mkdir -p "$ROOT/out"
        for p in frp misc; do
            [[ -f $ROOT/out/aosp-$p.img ]] || truncate -s 1M "$ROOT/out/aosp-$p.img"
        done
        disks=("$product/super.img" "$ROOT/out/aosp-frp.img,snapshot" "$ROOT/out/aosp-misc.img,snapshot")
    fi
    # The disk is PCI 00:04.0 (see the -device loop below), so it is the
    # boot device and vda is "super". Bootconfig values live on the
    # command line; /proc/bootconfig shows the androidboot.* ones.
    boot_args="init=/init androidboot.hardware=cutf_cvm androidboot.fstab_suffix=rustpc"
    boot_args+=" androidboot.slot_suffix=_a androidboot.force_normal_boot=0"
    boot_args+=" androidboot.boot_devices=pci0000:00/0000:00:04.0,pci0000:00/0000:00:05.0,pci0000:00/0000:00:06.0"
    boot_args+=" androidboot.partition_map=vda,super;vdb,frp;vdc,misc"
    # The fstab (with /misc) as a device-tree style fstab node: first
    # stage mounts from it, and fs_mgr's default fstab keeps it afterwards
    # (androidboot.android_dt_dir), see tools/mkaosp-init-initramfs.py.
    boot_args+=" dtfstab=$(python3 "$ROOT/tools/mkaosp-init-initramfs.py" --dtfstab)"
    boot_args+=" androidboot.android_dt_dir=/sys/firmware/devicetree/base/firmware/android/"
    boot_args+=" androidboot.selinux=permissive androidboot.verifiedbootstate=orange"
    boot_args+=" androidboot.veritymode=disabled androidboot.dynamic_partitions=true"
    boot_args+=" androidboot.serialno=RUSTPC000001 androidboot.console=ttyS0 printk.devkmsg=on"
    # RAM disks: the block devices of the tmpfs /metadata and /data.
    boot_args+=" brd.rd_nr=2 brd.rd_size=4096"
    boot_args+=" androidboot.hardware.gralloc=minigbm androidboot.hardware.hwcomposer=ranchu"
    boot_args+=" androidboot.hardware.egl=angle androidboot.hardware.vulkan=pastel androidboot.lcd_density=320"
    # The rest of cuttlefish's --gpu_mode=guest_swiftshader settings
    # (ranchu composer on DRM, SwiftShader Vulkan under ANGLE).
    boot_args+=" androidboot.hardware.hwcomposer.display_finder_mode=drm"
    boot_args+=" androidboot.hardware.hwcomposer.display_framebuffer_format=rgba"
    boot_args+=" androidboot.opengles.version=196609 androidboot.cpuvulkan.version=4202496"
    boot_args+=" androidboot.hardware.angle_feature_overrides_enabled=mapUnspecifiedColorSpaceToPassThrough"
    boot_args+=" androidboot.hardware.angle_feature_overrides_disabled=exposeES32ForTesting"
    boot_args+=" androidboot.enable_bootanimation=1"
    boot_args+=" androidboot.setupwizard_mode=DISABLED androidboot.hw_timeout_multiplier=3"
    boot_args+=" androidboot.hypervisor.vm.supported=0 androidboot.hypervisor.protected_vm.supported=0"
    # Cuttlefish picks one of several vendor APEX variants per module
    # (the values cvd-1's bootconfig uses); apexd aborts on duplicates.
    v="androidboot.vendor.apex"
    boot_args+=" $v.com.android.hardware.gatekeeper=com.android.hardware.gatekeeper.nonsecure"
    boot_args+=" $v.com.android.hardware.keymint=com.android.hardware.keymint.rust_nonsecure"
    boot_args+=" $v.com.android.hardware.graphics.composer=com.android.hardware.graphics.composer.ranchu"
    boot_args+=" $v.com.android.hardware.secure_element=com.android.hardware.secure_element"
    boot_args+=" $v.com.android.hardware.strongbox=none $v.com.google.cf.disabled=none"
    boot_args+=" $v.com.google.emulated.camera.provider.hal=com.google.emulated.camera.provider.hal"
    append="$boot_args $append"
    [[ $timeout == 120 ]] && timeout=300
    [[ ( $mode == aosp-gfx-test || $mode == aosp-input-test ) && $timeout == 300 ]] && timeout=420
    [[ ( $mode == aosp-gfx-test || $mode == aosp-input-test ) && $mem == 2G ]] && mem=4G
    # zygote, system_server and ART's compiler need room and CPUs.
    [[ ( $mode == aosp-boot-test || $mode == aosp-apk-test ) && $timeout == 300 ]] && timeout=900
    [[ ( $mode == aosp-boot-test || $mode == aosp-apk-test ) && $mem == 2G ]] && mem=6G
    [[ ( $mode == aosp-boot-test || $mode == aosp-apk-test ) && $smp == 4 ]] && smp=6
    [[ $mode == aosp-init ]] && mode=run
fi
if [[ $mode == aosp-test || $mode == aosp ]]; then
    if [[ -z $initrd ]]; then
        "$ROOT/tools/mkaosp-initramfs.sh" >/dev/null
        initrd="$ROOT/out/aosp-initramfs.cpio"
    fi
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
for i in "${!disks[@]}"; do
    IFS=, read -r -a d <<<"${disks[$i]}"
    file="${d[0]}" ro=on snap=off modern=""
    for o in "${d[@]:1}"; do
        case "$o" in
            rw) ro=off ;;
            snapshot) ro=off snap=on ;;
            legacy) modern=",disable-modern=on,disable-legacy=off" ;;
            *) echo "run-qemu.sh: unknown disk option $o" >&2; exit 2 ;;
        esac
    done
    [[ -f $file ]] || { echo "run-qemu.sh: no such disk image $file" >&2; exit 2; }
    # file.locking=off: images shared with a running VM (Cuttlefish) are
    # only ever opened read-only or through a snapshot here.
    # Fixed PCI slots from 00:04.0 on, so sysfs paths (and Android's
    # androidboot.boot_devices) are stable.
    args+=(-drive "file=$file,format=raw,if=none,id=disk$i,readonly=$ro,snapshot=$snap,file.locking=off"
           -device "virtio-blk-pci,drive=disk$i,addr=$(printf '0x%x' $((4 + i)))$modern")
done

if [[ $gpu == 1 ]]; then
    IFS=x read -r gw gh <<<"$gpu_size"
    args+=(-vga none -device "virtio-gpu-pci,addr=0x2,xres=$gw,yres=$gh")
fi
if [[ $input == 1 ]]; then
    # Modern-only, like Cuttlefish's; QEMU's `sendkey` and mouse events
    # go to these once the guest driver is up.
    args+=(-device virtio-keyboard-pci,addr=0x10,disable-legacy=on
           -device virtio-tablet-pci,addr=0x11,disable-legacy=on
           -device virtio-multitouch-pci,addr=0x12,disable-legacy=on)
fi
# Unix socket paths are limited to 107 bytes, too short for $work.
[[ ( $mode == aosp-gfx-test || $mode == aosp-input-test || $mode == aosp-apk-test ) && -z $monitor ]] && monitor="${XDG_RUNTIME_DIR:-/tmp}/android-pc-monitor-$$.sock"
if [[ -n $monitor ]]; then
    rm -f "$monitor"
    args+=(-monitor "unix:$monitor,server,nowait")
fi

if [[ $net == 1 ]]; then
    fwd=""
    [[ -n $adb_port ]] && fwd=",hostfwd=tcp:127.0.0.1:$adb_port-:5555"
    args+=(-netdev "user,id=net0$fwd" -device "virtio-net-pci,netdev=net0,addr=0x3")
fi

if [[ $mode == ktest ]]; then
    # Serial to stdout and a log; the verdict is ktest-init's END line.
    log="$work/ktest.log"
    args+=(-display "$display" -serial stdio)
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
    args+=(-display "$display" -serial stdio)
    if timeout --foreground $((timeout * 3)) python3 "$ROOT/tools/aosp-sh-test.py" --log "$log" "${sh_args[@]}" \
        -- qemu-system-x86_64 "${args[@]}" "${qemu_extra[@]}"; then
        exit 0
    fi
    echo "run-qemu.sh: AOSP shell test FAILED (serial log: $log)" >&2
    exit 1
elif [[ $mode == aosp-init-test ]]; then
    log="$work/aosp-init-test.log"
    args+=(-display "$display" -serial stdio)
    if timeout --foreground $((timeout + 60)) python3 "$ROOT/tools/aosp-init-test.py" --log "$log" \
        --boot-timeout "$timeout" "${sh_args[@]}" -- qemu-system-x86_64 "${args[@]}" "${qemu_extra[@]}"; then
        exit 0
    fi
    echo "run-qemu.sh: AOSP init test FAILED (serial log: $log)" >&2
    exit 1
elif [[ $mode == aosp-adb-test ]]; then
    log="$work/aosp-adb-test.log"
    args+=(-display "$display" -serial stdio)
    if timeout --foreground $((timeout + 120)) python3 "$ROOT/tools/aosp-adb-test.py" --log "$log" \
        --boot-timeout "$timeout" --port "$adb_port" "${sh_args[@]}" -- qemu-system-x86_64 "${args[@]}" "${qemu_extra[@]}"; then
        exit 0
    fi
    echo "run-qemu.sh: AOSP adb test FAILED (serial log: $log)" >&2
    exit 1
elif [[ $mode == aosp-gfx-test ]]; then
    log="$work/aosp-gfx-test.log"
    args+=(-display "$display" -serial stdio)
    if timeout --foreground $((timeout + 240)) python3 "$ROOT/tools/aosp-gfx-test.py" --log "$log" \
        --boot-timeout "$timeout" --port "$adb_port" --monitor "$monitor" --out "$work/gfx" "${sh_args[@]}" \
        -- qemu-system-x86_64 "${args[@]}" "${qemu_extra[@]}"; then
        exit 0
    fi
    echo "run-qemu.sh: AOSP graphics test FAILED (serial log: $log, screenshots: $work/gfx)" >&2
    exit 1
elif [[ $mode == aosp-input-test ]]; then
    log="$work/aosp-input-test.log"
    args+=(-display "$display" -serial stdio)
    if timeout --foreground $((timeout + 300)) python3 "$ROOT/tools/aosp-input-test.py" --log "$log" \
        --boot-timeout "$timeout" --port "$adb_port" --monitor "$monitor" "${sh_args[@]}" \
        -- qemu-system-x86_64 "${args[@]}" "${qemu_extra[@]}"; then
        exit 0
    fi
    echo "run-qemu.sh: AOSP input test FAILED (serial log: $log)" >&2
    exit 1
elif [[ $mode == aosp-boot-test ]]; then
    log="$work/aosp-boot-test.log"
    args+=(-display "$display" -serial stdio)
    if timeout --foreground $((timeout + 300)) python3 "$ROOT/tools/aosp-boot-test.py" --log "$log" \
        --boot-timeout "$timeout" --port "$adb_port" "${sh_args[@]}" \
        -- qemu-system-x86_64 "${args[@]}" "${qemu_extra[@]}"; then
        exit 0
    fi
    echo "run-qemu.sh: AOSP boot test FAILED (serial log: $log)" >&2
    exit 1
elif [[ $mode == aosp-apk-test ]]; then
    log="$work/aosp-apk-test.log"
    args+=(-display "$display" -serial stdio)
    if timeout --foreground $((timeout + 600)) python3 "$ROOT/tools/aosp-apk-test.py" --log "$log" \
        --boot-timeout "$timeout" --port "$adb_port" --monitor "$monitor" --out "$work/apk" \
        --apk "$ROOT/apps/hello/build/hello.apk" "${sh_args[@]}" \
        -- qemu-system-x86_64 "${args[@]}" "${qemu_extra[@]}"; then
        exit 0
    fi
    echo "run-qemu.sh: AOSP APK test FAILED (serial log: $log, screenshots: $work/apk)" >&2
    exit 1
elif [[ $mode == test ]]; then
    args+=(-display "$display" -serial stdio)
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
    args+=(-display "$display" -serial mon:stdio)
    exec qemu-system-x86_64 "${args[@]}" "${qemu_extra[@]}"
fi
