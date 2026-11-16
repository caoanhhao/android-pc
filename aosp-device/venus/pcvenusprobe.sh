#!/vendor/bin/sh
# Phase K3: choose the graphics stack of the Venus entry (init.venus.rc, post-fs-data). With the
# vtest socket of K2's sidecar: Venus, minigbm's vtest backend, ES 3.2 with the Android Extension
# Pack and ANGLE's gpuid list. Without it: SwiftShader and minigbm's DRM scan, ES 3.1, as on the
# default entry. Writes the choice to the kernel log and /data/vendor/pcgpu/venus.txt.
sock=$(getprop ro.vendor.minigbm.vtest.socket)
sock=${sock:-/dev/socket/vtest}

if [ -S "$sock" ]; then
    # minigbm: GPU buffers from the server, the rest udmabuf; every process reads it
    setprop ro.vendor.minigbm.vtest true
    # Venus (Mesa reads MESA_*/VN_* options from vendor.mesa.*): vtest only, this socket
    setprop vendor.mesa.vn.debug vtest
    setprop vendor.mesa.vtest.socket.name "$sock"
    setprop ro.hardware.vulkan virtio
    setprop ro.opengles.version 196610
    # ANGLE's per-app GPU names and the AEP feature: empty files on the other entries
    mount -o bind /vendor/etc/gpuid.nvidia.conf /vendor/etc/gpuid.conf
    mount -o bind /vendor/etc/venus/android.hardware.opengles.aep.xml \
        /vendor/etc/permissions/android.hardware.opengles.aep.pc.xml
    choice="venus ($sock)"
else
    setprop ro.hardware.vulkan pastel
    setprop ro.opengles.version 196609
    choice="swiftshader (no $sock)"
fi
setprop vendor.venus.probe "${choice%% *}"

echo "pcvenusprobe: $choice" > /dev/kmsg
mkdir -p /data/vendor/pcgpu
echo "$choice" > /data/vendor/pcgpu/venus.txt
