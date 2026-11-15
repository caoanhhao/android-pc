#!/vendor/bin/sh
# Phase K2 (docs/nvidia-proprietary.md): the NVIDIA glibc sidecar's vtest server, started by
# init.nvsidecar.rc on the entry with androidboot.android_pc.gpu=nvidia, in the nvsidecar
# domain. Output goes to the kernel log (stdio_to_kmsg); pclog.sh saves it.
#
# /vendor/bin/nvsidecar is a glibc root (tools/nvsidecar-build.sh): NVIDIA's Vulkan driver at the
# kernel modules' version and waydroid-nvidia's patched virglrenderer. Its binaries name
# /vendor/bin/nvsidecar/lib/ld-linux-x86-64.so.2 as their interpreter; nvidia-smi (NVIDIA's own)
# runs through that loader explicitly.
#
# Without a working NVIDIA GPU (QEMU, modules that did not load) the script exits before the
# socket exists, so K3's pcvenusprobe.sh falls back to SwiftShader. Otherwise it runs the
# server and starts it again if it dies (Venus clients then have to reconnect).
R=/vendor/bin/nvsidecar
SOCK=/dev/socket/vtest
export LD_LIBRARY_PATH=$R/lib
export HOME=/data/vendor/nvsidecar
export __GL_SHADER_DISK_CACHE_PATH=/data/vendor/nvsidecar/cache
# Only NVIDIA: no other ICD or EGL vendor exists in the root, and none is searched for.
export VK_DRIVER_FILES=$R/share/vulkan/icd.d/nvidia_icd.json
export __EGL_VENDOR_LIBRARY_FILENAMES=$R/share/glvnd/egl_vendor.d/10_nvidia.json
export RENDER_SERVER_EXEC_PATH=$R/bin/virgl_render_server

echo "nvsidecar: $(cat $R/VERSION | tr '\n' ';')"
if [ ! -c /dev/nvidiactl ]; then
    echo "nvsidecar: no /dev/nvidiactl (no NVIDIA GPU, or nvidia.ko did not load): not starting"
    exit 0
fi
# nvidia-smi talks to the driver through NVML, as the Vulkan driver does: if it cannot see the
# GPU (version mismatch, GSP failure) the server would only hand out failing devices.
if ! $R/lib/ld-linux-x86-64.so.2 $R/bin/nvidia-smi -L; then
    echo "nvsidecar: nvidia-smi sees no GPU: not starting"
    exit 0
fi

# Android apps connect with their own uids: the socket must be world-writable.
umask 000
n=0
while true; do
    n=$((n + 1))
    rm -f $SOCK
    echo "nvsidecar: virgl_test_server, start $n"
    # --no-fork: one process serves every client (each Venus context still gets its own
    # virgl_render_server child). No --rendernode: with only NVIDIA's ICD the dma-buf
    # allocator takes the one device that has the export extensions.
    $R/bin/virgl_test_server --venus --no-virgl --no-fork --multi-clients --socket-path $SOCK
    echo "nvsidecar: virgl_test_server exited ($?)"
    sleep 2
done
