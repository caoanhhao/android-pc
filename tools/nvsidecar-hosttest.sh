#!/usr/bin/env bash
# Run the NVIDIA sidecar root (tools/nvsidecar-build.sh) on the build host's own NVIDIA GPU,
# the way Android runs it: in a bwrap sandbox that has nothing but /vendor/bin/nvsidecar, the
# NVIDIA and DRM device nodes, /proc, /sys and an empty /data, so a library missing from the
# root fails here as it would on the stick (Phase K2, docs/nvidia-proprietary.md).
#
#   tools/nvsidecar-hosttest.sh
#
# 1. nvidia-smi -L inside the sandbox;
# 2. virgl_test_server with the service's flags and environment (aosp-device/hals/nvsidecar/nvsidecar.sh) on a
#    private socket, and the host's Mesa Venus (virtio_icd.json, VN_DEBUG=vtest) running
#    vulkaninfo --summary through it. Expected: "Virtio-GPU Venus (NVIDIA GeForce RTX 4070)".
# Uses its own socket under $XDG_RUNTIME_DIR; Waydroid's waydroid-vtest service and its
# socket are not touched.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
D=$ROOT/aosp/device/android-pc/nvsidecar
R=${XDG_RUNTIME_DIR:-/tmp}/nvsidecar-test
[[ -x $D/bin/virgl_test_server ]] || { echo "no sidecar root: run tools/nvsidecar-build.sh" >&2; exit 1; }
mkdir -p "$R"; /usr/bin/rm -f "$R/vtest.sock"

box=(bwrap --unshare-all --die-with-parent --clearenv
     --ro-bind "$D" /vendor/bin/nvsidecar --proc /proc --ro-bind /sys /sys --tmpfs /data
     --dev /dev --dev-bind /dev/dri /dev/dri --bind "$R" /dev/socket/vtest)
for n in /dev/nvidia*; do [[ -c $n ]] && box+=(--dev-bind "$n" "$n"); done
box+=(--setenv HOME /data --setenv LD_LIBRARY_PATH /vendor/bin/nvsidecar/lib
      --setenv __EGL_VENDOR_LIBRARY_FILENAMES /vendor/bin/nvsidecar/share/glvnd/egl_vendor.d/10_nvidia.json)

echo "== nvidia-smi -L (sandbox)"
"${box[@]}" /vendor/bin/nvsidecar/lib/ld-linux-x86-64.so.2 /vendor/bin/nvsidecar/bin/nvidia-smi -L

echo "== virgl_test_server (sandbox), vulkaninfo through Venus (host)"
"${box[@]}" --setenv VK_DRIVER_FILES /vendor/bin/nvsidecar/share/vulkan/icd.d/nvidia_icd.json \
    --setenv RENDER_SERVER_EXEC_PATH /vendor/bin/nvsidecar/bin/virgl_render_server \
    /vendor/bin/nvsidecar/bin/virgl_test_server --venus --no-virgl --no-fork --multi-clients \
    --socket-path /dev/socket/vtest/vtest.sock > "$R/server.log" 2>&1 &
srv=$!
for _ in $(seq 50); do [[ -S $R/vtest.sock ]] && break; sleep 0.1; done
rc=0
VK_DRIVER_FILES=/usr/share/vulkan/icd.d/virtio_icd.json VN_DEBUG=vtest VTEST_SOCKET_NAME=$R/vtest.sock \
    timeout 30 vulkaninfo --summary 2>&1 | tee "$R/client.log" | grep -E "deviceName|driverName|apiVersion|conformanceVersion" || rc=1
# vtest protocol 7 (real sync_file export) with waydroid-nvidia's patched Venus and its test,
# when that checkout has them built (venus/tests/run-sync-fd-test.sh)
WN=${WAYDROID_NVIDIA:-$HOME/projects/waydroid-nvidia}
icd=$WN/src/mesa-fence/build-host/src/virtio/vulkan/virtio_devenv_icd.x86_64.json
if [[ -f $icd && -x $WN/venus/tests/build/vtest-sync-fd ]]; then
    echo "== vtest-sync-fd (waydroid-nvidia's patched Venus, protocol 7)"
    VK_ICD_FILENAMES=$icd VN_DEBUG=vtest VTEST_SOCKET_NAME=$R/vtest.sock \
        timeout 60 "$WN/venus/tests/build/vtest-sync-fd" 5 2>&1 | tail -5 || rc=1
fi
kill $srv 2>/dev/null; wait $srv 2>/dev/null || true
echo "== server log"; cat "$R/server.log"
exit $rc
