#!/vendor/bin/sh
# Phase G6 (docs/nvidia.md), started by init.pc.rc on the nova boot entry only, once Android
# has booted on pcfb: loads NVIDIA's Rust drivers (nova_core.ko, then nova.ko; they meet on
# the auxiliary bus, so modprobe does not pull one in for the other) and records how far they
# bring up the RTX 4070. Loading late means pclog is already writing dmesg to its partition if
# the GPU or the kernel hangs. Output: /data/vendor/pcgpu/nova.txt (pclog.sh copies it into
# its state dumps).
export PATH=/system/bin:/vendor/bin:$PATH
out=/data/vendor/pcgpu
mkdir -p $out
k() { echo "pcnova: $*" > /dev/kmsg; }
sleep 30
k "modprobe nova_core.ko nova.ko"
t0=$(cut -d' ' -f1 /proc/uptime)
modprobe -d /vendor/lib/modules nova_core.ko > $out/nova-modprobe.txt 2>&1
rc=$?
modprobe -d /vendor/lib/modules nova.ko >> $out/nova-modprobe.txt 2>&1
rc="$rc $?"
k "modprobe exited ($rc) after $(cut -d' ' -f1 /proc/uptime) s (started $t0 s)"
sleep 5
{
    echo "== modprobe ($rc)"; cat $out/nova-modprobe.txt
    echo "== lsmod"; lsmod | grep -E '^(nova|nova_core|nouveau) '
    echo "== pci"; for p in /sys/bus/pci/devices/*; do
        [ "$(cat $p/vendor)" = 0x10de ] || continue
        echo "${p##*/} $(cat $p/device) class $(cat $p/class) driver $(basename "$(readlink $p/driver)") enable $(cat $p/enable) power $(cat $p/power/runtime_status 2>/dev/null)"
        grep -H . $p/resource 2>/dev/null | head -n 4
    done
    echo "== auxiliary"; ls -l /sys/bus/auxiliary/devices/ 2>&1
    echo "== drm"; for c in /sys/class/drm/card* /sys/class/drm/renderD*; do
        echo "${c##*/}: $(basename "$(readlink $c/device/driver)") $(cat $c/device/uevent 2>/dev/null | tr '\n' ' ')"
    done
    cat /sys/kernel/debug/dri/*/name 2>&1
    echo "== dmesg"; dmesg | grep -iE 'nova|pcnova|10de|rust' | head -n 200
    echo "== iomem"; grep -iE 'nova|bootfb|efifb|simple' /proc/iomem
} > $out/nova.txt 2>&1
k "state in $out/nova.txt"
