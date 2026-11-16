#!/vendor/bin/sh
# Phase K3: set up the graphics stack of the Venus entry (init.venus.rc, post-fs-data): Venus over
# the vtest socket of K2's sidecar, minigbm's vtest backend, ES 3.2 with the Android Extension
# Pack and ANGLE's gpuid list. There is no CPU fallback: this project draws on the NVIDIA GPU, so
# a missing socket is logged as an error and the entry stays on Venus. Writes the result to the
# kernel log and /data/vendor/pcgpu/venus.txt.
sock=$(getprop ro.vendor.minigbm.vtest.socket)
sock=${sock:-/dev/socket/vtest}

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

if [ -S "$sock" ]; then
    choice="venus ($sock)"
    setprop vendor.venus.probe venus
else
    choice="venus, ERROR: no $sock (the sidecar did not start; nothing will draw)"
    setprop vendor.venus.probe no-socket
fi
echo "pcvenusprobe: $choice" > /dev/kmsg
mkdir -p /data/vendor/pcgpu
echo "$choice" > /data/vendor/pcgpu/venus.txt
