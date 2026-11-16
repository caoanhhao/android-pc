#!/usr/bin/env bash
# Boot a tools/mkusb-aosp17.sh image in QEMU with the host's RTX 4070 passed through (VFIO),
# so the NVIDIA boot entries (L1/L3: NVIDIA's own driver, nouveau) run on the real card in a
# VM instead of on the PC from the stick.
#
#   tools/run-qemu-aosp17-vfio.sh [IMAGE]    default out/aosp17-usb.img (not a --disk stick image:
#                                            its userdata does not fit in the file)
#   tools/run-qemu-aosp17-vfio.sh --release  give the card back to nvidia / snd_hda_intel
#
# The image must boot an NVIDIA entry: an image made without --menu has a single entry, so
# give it the entry's words with mkusb-aosp17.sh --append (docs/nvidia-native.md, "VFIO in
# QEMU"). Android's picture comes out of the
# RTX 4070's own ports (the monitor's input on that card); the QEMU window keeps the std VGA
# (boot menu, pcfb on the other entries). Everything else is tools/run-qemu-aosp17-usb.sh
# (same VM limits, lock, adb, QEMU_EXTRA, WRITABLE, NO_WINDOW, ...).
#
# The host must not be using the card (docs/nvidia-native.md, "VFIO in QEMU"):
#   - IOMMU on: VT-d in the firmware, intel_iommu=on iommu=pt on the kernel command line;
#   - Hyprland and SDDM's Xorg on the iGPU (~/.config/hypr/scripts/gpu-mode igpu, monitor
#     cable on the motherboard, reboot), no process with /dev/nvidia* or the card's DRM nodes open.
# The script asks for the password (sudo) to unload the nvidia modules and bind the card's
# functions to vfio-pci. They stay there when QEMU exits, ready for the next run: given back,
# the card's new DRM node would be opened by Hyprland's hotplug handling (AQ_DRM_DEVICES
# covers only the devices present at its start), so it could not be passed again until the
# next login. --release (or a reboot) gives it back.
# A card the guest left in a bad state (Xid) may need a host reboot to be usable again.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
release=""
[[ ${1:-} == --release ]] && { release=1; shift; }
img=${1:-$ROOT/out/aosp17-usb.img}
[[ -n $release || -f $img ]] || { echo "run-qemu-aosp17-vfio.sh: no $img" >&2; exit 1; }
die() { echo "run-qemu-aosp17-vfio.sh: $*" >&2; exit 1; }

gpu=$(lspci -Dnd 10de::0300 | awk 'NR==1 {print $1}')
[[ -n $gpu ]] || die "no NVIDIA VGA controller"
slot=${gpu%.*}
fns=( $(lspci -Dns "$slot" | awk '{print $1}') )   # the GPU and its HDMI audio (and USB-C, if any)

[[ -e /sys/bus/pci/devices/$gpu/iommu_group ]] ||
    die "IOMMU is off: enable VT-d and boot with intel_iommu=on iommu=pt"
group=$(basename "$(readlink "/sys/bus/pci/devices/$gpu/iommu_group")")
for d in /sys/kernel/iommu_groups/"$group"/devices/*; do
    d=$(basename "$d")
    [[ $d == "$slot".* ]] && continue
    # Bridges (the root port) stay with the host; any other device would have to go too.
    [[ $(cat "/sys/bus/pci/devices/$d/class") == 0x0604* ]] ||
        die "IOMMU group $group also holds $d ($(lspci -s "$d" | cut -d' ' -f2-)); not passing it"
done

# Nothing on the host may hold the card.
nodes=(/dev/nvidia* /dev/dri/by-path/pci-"$gpu"-*)
holders=$(fuser "${nodes[@]}" 2>/dev/null | tr -s ' ' '\n' | sort -un | xargs -r -I{} ps -o pid=,comm= -p {} || true)
[[ -z $holders ]] || die "the RTX 4070 is in use (switch Hyprland to the iGPU, close these):
$holders"

driver_of() { basename "$(readlink "/sys/bus/pci/devices/$1/driver" 2>/dev/null)" 2>/dev/null || true; }
declare -A host_driver
for f in "${fns[@]}"; do host_driver[$f]=$(driver_of "$f"); done

bind_vfio() {
    sudo -v
    sudo modprobe vfio-pci
    # Root processes too (SDDM's greeter Xorg: ~/.config/hypr/scripts/gpu-mode igpu moves it).
    local root_holders
    root_holders=$(sudo fuser "${nodes[@]}" 2>/dev/null | tr -s ' ' '\n' | sort -un | xargs -r -I{} ps -o pid=,user=,comm= -p {} || true)
    [[ -z $root_holders ]] || die "the RTX 4070 is in use:
$root_holders"
    if [[ ${host_driver[$gpu]} == nvidia ]]; then
        sudo modprobe -r nvidia_drm nvidia_modeset nvidia_uvm nvidia ||
            die "could not unload the nvidia modules (something still uses them: lsmod | grep nvidia)"
    fi
    for f in "${fns[@]}"; do
        [[ ${host_driver[$f]} == vfio-pci ]] && continue   # still bound from an earlier run
        echo vfio-pci | sudo tee "/sys/bus/pci/devices/$f/driver_override" >/dev/null
        [[ -e /sys/bus/pci/devices/$f/driver ]] && echo "$f" | sudo tee "/sys/bus/pci/devices/$f/driver/unbind" >/dev/null
        echo "$f" | sudo tee /sys/bus/pci/drivers_probe >/dev/null
        [[ $(driver_of "$f") == vfio-pci ]] || die "$f did not bind to vfio-pci"
    done
    sudo chown "$(id -u)" "/dev/vfio/$group"
    # VFIO pins all guest RAM: QEMU (a child of this shell) needs that much locked memory.
    sudo prlimit --pid $$ --memlock=unlimited:unlimited
}

give_back() {
    echo "run-qemu-aosp17-vfio.sh: giving the card back to the host"
    for f in "${fns[@]}"; do
        echo | sudo tee "/sys/bus/pci/devices/$f/driver_override" >/dev/null || true
        [[ $(driver_of "$f") == vfio-pci ]] && echo "$f" | sudo tee /sys/bus/pci/drivers/vfio-pci/unbind >/dev/null || true
    done
    sudo modprobe nvidia_drm || true
    for f in "${fns[@]}"; do echo "$f" | sudo tee /sys/bus/pci/drivers_probe >/dev/null || true; done
    for f in "${fns[@]}"; do echo "  $f: $(driver_of "$f")"; done
}

if [[ -n $release ]]; then
    give_back
    exit
fi
trap give_back EXIT   # only if binding fails: once QEMU starts the card stays on vfio-pci
bind_vfio
trap - EXIT
extra=""
for f in "${fns[@]}"; do
    # rombar=0: the guest boots from the std VGA; the card's VBIOS is read by its own driver.
    extra+=" -device vfio-pci,host=$f,rombar=0"
done
status=0
QEMU_EXTRA="$extra ${QEMU_EXTRA:-}" "$ROOT/tools/run-qemu-aosp17-usb.sh" "$img" || status=$?
echo "run-qemu-aosp17-vfio.sh: the card stays on vfio-pci; $0 --release gives it back to the host"
exit "$status"
