#!/vendor/bin/sh
# Phase L1 (docs/nvidia-proprietary.md): runs pcnvtest, a glibc Vulkan client, on NVIDIA's own
# driver at fixed times after boot on the L1 test entries (androidboot.android_pc.nvtest=1),
# where no other GPU client exists (no sidecar, no Venus: Android draws on its own). It tells
# whether the Xid 51 (BAD_TSG) comes from creating channels at all or only from a particular use:
#   T+0      nvidia-smi, instance info, one device (fill, copy, compute), two devices at once
#            (microG DroidGuard's pattern) x5, 16 devices one after another
#   T+0..10m every 30 s: one device and one pair
#   T+10m    one device with HIGH, then REALTIME global queue priority (root has CAP_SYS_NICE;
#            the build host, where the driver works, never got these)
#   ..T+30m  every 30 s: one device and one pair
# After every step the kernel log is checked for a new "NVRM: Xid". Lines go to the kernel log
# (pclog keeps it, and the on-screen log strip shows it) and to /data/vendor/pcgpu/nvtest.txt.
R=/vendor/bin/nvsidecar
export PATH=/system/bin:/vendor/bin:$PATH
# NVIDIA's own libraries are in the root's lib/ or, once L3 moves them, in /vendor/lib64/nvidia.
export LD_LIBRARY_PATH=$R/lib:/vendor/lib64/nvidia
export HOME=/data/vendor/pcgpu
export __GL_SHADER_DISK_CACHE_PATH=/data/vendor/pcgpu/glcache
export VK_DRIVER_FILES=$R/share/vulkan/icd.d/nvidia_icd.json
out=/data/vendor/pcgpu/nvtest.txt
mkdir -p /data/vendor/pcgpu/glcache
log() { echo "<4>pcnvtest: $*" > /dev/kmsg; echo "$(cut -d' ' -f1 /proc/uptime) $*" >> $out; }
xids() { dmesg | grep -c 'NVRM: Xid'; }
seen=$(xids)
step() {
    local label=$1; shift
    log "step $label: $*"
    sync
    "$@" 2>&1 | while IFS= read -r l; do log "  $l"; done
    local n=$(xids)
    if [ "$n" != "$seen" ]; then
        log "XID after step $label ($((n - seen)) new):"
        dmesg | grep 'NVRM: Xid' | tail -n $((n - seen)) | while IFS= read -r l; do log "  $l"; done
        seen=$n
    fi
    sync
}
t() { $R/lib/ld-linux-x86-64.so.2 /vendor/bin/pcnvtest "$@"; }
smi() { $R/lib/ld-linux-x86-64.so.2 $R/bin/nvidia-smi "$@"; }

: > $out
sleep 20   # T+0 is 20 s after sys.boot_completed, once the boot's own work has settled
if [ ! -c /dev/nvidiactl ]; then log "no /dev/nvidiactl (no NVIDIA GPU or nvidia.ko did not load): nothing to test"; exit 0; fi
log "start, $(grep CapEff /proc/self/status), Xid so far: $seen"
step smi smi -L
step smi-q smi -q -d PERFORMANCE,CLOCK,TEMPERATURE
step info t info
step copy t copy
step pair t pair 5
step many t many 16
t0=$(cut -d. -f1 /proc/uptime)
elapsed() { echo $(( $(cut -d. -f1 /proc/uptime) - t0 )); }
prio_done=
while [ "$(elapsed)" -lt 1800 ]; do
    sleep 30
    if [ -z "$prio_done" ] && [ "$(elapsed)" -ge 600 ]; then
        step copy-high t copy high
        step copy-realtime t copy realtime
        step copy-after-prio t copy
        prio_done=1
        continue
    fi
    step "soak $(elapsed)s" t copy
    step "soak-pair $(elapsed)s" t pair 1
done
log "done after $(elapsed) s, Xid in this boot: $(xids)"
