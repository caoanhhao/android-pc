#!/vendor/bin/sh
# Phase K1 (docs/nvidia-proprietary.md), run by init.pc.rc in early-init on the nvidia boot
# entry, after the modprobes: creates the device nodes of NVIDIA's own driver, which the
# modules register (char major 195, nvidia-uvm and nvidia-caps dynamic) without creating any
# node (on desktop Linux nvidia-modprobe does it), and reports to the kernel log.
#   /dev/nvidiactl 195:255, /dev/nvidia-modeset 195:254, /dev/nvidiaN 195:N per GPU,
#   /dev/nvidia-uvm and /dev/nvidia-uvm-tools (minors 0, 1), /dev/nvidia-caps/nvidia-capN.
# Mode 0666, group graphics, as on desktop Linux; the K2 sidecar opens them.
export PATH=/system/bin:/vendor/bin:$PATH
k() { echo "pcnvidia: $*" > /dev/kmsg; }
node() {   # node PATH MAJOR MINOR
    [ -e "$1" ] || mknod -m 0666 "$1" c "$2" "$3"
    chown root:graphics "$1"
}
major() { grep " $1\$" /proc/devices | cut -d' ' -f1; }

if ! grep -q '^nvidia ' /proc/modules; then
    k "nvidia.ko not loaded (no NVIDIA GPU, or it failed: see the NVRM lines above)"
    exit 0
fi
node /dev/nvidiactl 195 255
grep -q '^nvidia_modeset ' /proc/modules && node /dev/nvidia-modeset 195 254
n=0
for g in /proc/driver/nvidia/gpus/*; do
    [ -d "$g" ] || continue
    minor=$(sed -n 's/^Device Minor:[[:space:]]*//p' "$g/information")
    node /dev/nvidia${minor:-$n} 195 ${minor:-$n}
    n=$((n + 1))
done
m=$(major nvidia-uvm)
if [ -n "$m" ]; then node /dev/nvidia-uvm "$m" 0; node /dev/nvidia-uvm-tools "$m" 1; fi
m=$(major nvidia-caps)
if [ -n "$m" ]; then
    mkdir -p /dev/nvidia-caps
    for c in /proc/driver/nvidia/capabilities/mig/config /proc/driver/nvidia/capabilities/mig/monitor; do
        minor=$(sed -n 's/^DeviceFileMinor:[[:space:]]*//p' "$c" 2>/dev/null)
        [ -n "$minor" ] && node /dev/nvidia-caps/nvidia-cap$minor "$m" "$minor"
    done
fi
k "$n GPU(s); $(ls /dev/nvidia* /dev/nvidia-caps/* 2>/dev/null | tr '\n' ' ')"
k "$(cat /proc/driver/nvidia/version 2>/dev/null | head -n 1)"
for c in /sys/class/drm/card*; do
    [ -e "$c/device/driver" ] && k "${c##*/}: $(basename "$(readlink $c/device/driver)")"
done
