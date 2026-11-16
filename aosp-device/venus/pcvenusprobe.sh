#!/vendor/bin/sh
# Phase K3: choose the graphics stack of the Venus entry (init.venus.rc, post-fs-data). With the
# vtest socket of K2's sidecar and a Venus device that pcvkprobe can create: Venus (through the
# vulkan.venus switch, so pcvenuswatch.sh can take it back), minigbm's vtest backend, ES 3.2
# with the Android Extension Pack and ANGLE's gpuid list. Otherwise: SwiftShader and minigbm's
# DRM scan, ES 3.1, as on the default entry. Writes the choice to the kernel log and
# /data/vendor/pcgpu/venus.txt.
sock=$(getprop ro.vendor.minigbm.vtest.socket)
sock=${sock:-/dev/socket/vtest}
mkdir -p /data/vendor/pcgpu

log() {
    echo "pcvenusprobe: $*" > /dev/kmsg
    echo "$*" >> /data/vendor/pcgpu/venus.txt
}

fallback() {
    setprop vendor.venus.state off
    setprop ro.hardware.vulkan pastel
    setprop ro.opengles.version 196609
    # SwiftShader has one queue, and HWUI's Vulkan pipeline asks for two
    setprop debug.hwui.renderer skiagl
    setprop vendor.venus.probe swiftshader
    log "swiftshader ($1)"
}

: > /data/vendor/pcgpu/venus.txt
if [ ! -S "$sock" ]; then
    fallback "no $sock"
    exit 0
fi

# Venus (Mesa reads MESA_*/VN_* options from vendor.mesa.*): vtest only, this socket. Set before
# the device probe, which loads Venus itself.
setprop vendor.mesa.vn.debug vtest
setprop vendor.mesa.vtest.socket.name "$sock"
if ! result=$(/system_ext/bin/pcvkprobe virtio --check); then
    fallback "Venus: $result"
    exit 0
fi

# minigbm: GPU buffers from the server, the rest udmabuf; every process reads it
setprop ro.vendor.minigbm.vtest true
setprop vendor.venus.state on
setprop ro.hardware.vulkan venus
setprop ro.opengles.version 196610
# ANGLE's per-app GPU names and the AEP feature: empty files on the other entries
mount -o bind /vendor/etc/gpuid.nvidia.conf /vendor/etc/gpuid.conf
mount -o bind /vendor/etc/venus/android.hardware.opengles.aep.xml \
    /vendor/etc/permissions/android.hardware.opengles.aep.pc.xml
setprop vendor.venus.probe venus
log "venus ($sock, $result)"
setprop ctl.start vendor.venus_watch
