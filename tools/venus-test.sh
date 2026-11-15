#!/bin/bash
# Phase K3 test without the NVIDIA GPU (docs/nvidia-proprietary.md, "K3 test"): run a vtest
# server inside the Android guest, on lavapipe, so that Venus, minigbm's vtest backend and ANGLE
# get a real vtest peer in QEMU. A server on the build host cannot serve a VM: vtest hands
# dma-bufs and shared memory over the socket as file descriptors (SCM_RIGHTS), which do not
# cross into a guest.
#
#   tools/venus-test.sh build            lavapipe (glibc, Mesa from tools/venus-android-build.sh's
#                                        checkout) + the server tree in out/venus/test-root
#   tools/venus-test.sh push [SERIAL]    adb push it to /data/local/tmp/vtest (default serial
#                                        127.0.0.1:5556, tools/run-qemu-aosp17-usb.sh's adb)
#
# The tree is a private glibc root: the host's ld.so and every library the programs need
# (ldd), waydroid-nvidia's patched virgl_test_server/virgl_render_server (virglrenderer 1.3.0 +
# vtest protocol 6, $VREND_DIST, default ~/projects/waydroid-nvidia/release/dist/host), the
# host's Vulkan loader and lavapipe. run.sh starts it on /dev/socket/vtest; init.venus.rc runs
# run.sh before SurfaceFlinger on an entry with androidboot.android_pc.venus_test=1.
# Test only: nothing here goes into an image.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MESA_DIR=${MESA_DIR:-$ROOT/out/venus/mesa}
# lavapipe builds in its own worktree of the same checkout: it carries a test-only patch
# (tools/patches/venus-test/: two queues, which Android's HWUI asks for; lavapipe has one)
LVP_DIR=${LVP_DIR:-$ROOT/out/venus/mesa-lvp}
LVP_BUILD=$LVP_DIR/build-host-lavapipe
VREND_DIST=${VREND_DIST:-$HOME/projects/waydroid-nvidia/release/dist/host}
TR=$ROOT/out/venus/test-root
GUEST=/data/local/tmp/vtest

build() {
    [[ -d $MESA_DIR/.git ]] || { echo "venus-test: run tools/venus-android-build.sh first" >&2; exit 1; }
    if [[ ! -d $LVP_DIR ]]; then
        git -C "$MESA_DIR" worktree add -q --detach "$LVP_DIR" pc-venus
        git -C "$LVP_DIR" apply "$ROOT"/tools/patches/venus-test/*.patch
    fi
    if [[ ! -f $LVP_BUILD/build.ninja ]]; then
        (cd "$LVP_DIR" && meson setup "$LVP_BUILD" -Dbuildtype=release -Dplatforms= \
            -Dglx=disabled -Degl=disabled -Dgles1=disabled -Dgles2=disabled -Dopengl=false \
            -Dgbm=disabled -Dgallium-drivers=llvmpipe -Dvulkan-drivers=swrast -Dllvm=enabled \
            -Dshared-llvm=enabled -Dvulkan-layers= -Dtools= -Dvideo-codecs= -Dzstd=disabled)
    fi
    "$ROOT/tools/limited-build.sh" ninja -C "$LVP_BUILD" -j8 src/gallium/targets/lavapipe/libvulkan_lvp.so

    /usr/bin/rm -rf "$TR"; mkdir -p "$TR/bin" "$TR/lib" "$TR/icd"
    cp "$VREND_DIST/bin/virgl_test_server" "$VREND_DIST/bin/virgl_render_server" "$TR/bin/"
    cp -P "$VREND_DIST"/lib/* "$TR/lib/"
    cp "$LVP_BUILD/src/gallium/targets/lavapipe/libvulkan_lvp.so" "$TR/lib/"
    cp -L /usr/lib/libvulkan.so.1 "$TR/lib/"
    # every library the programs load, from the host (the vtest server's own libvirglrenderer
    # comes from $VREND_DIST, not /usr/lib)
    for f in "$TR"/bin/* "$TR"/lib/*.so*; do
        LD_LIBRARY_PATH=$TR/lib ldd "$f" | awk '/=> \//{print $3}'
    done | sort -u | while read -r l; do
        [[ -e $TR/lib/$(basename "$l") ]] || cp -L "$l" "$TR/lib/"
    done
    cp -L /usr/lib64/ld-linux-x86-64.so.2 "$TR/lib/"
    cat > "$TR/icd/lvp_icd.json" <<EOF
{ "file_format_version": "1.0.1",
  "ICD": { "library_path": "$GUEST/lib/libvulkan_lvp.so", "api_version": "1.4.0" } }
EOF
    # virgl_test_server execs the render server by path: a wrapper runs it through the private
    # ld.so (the binaries' PT_INTERP, /lib64/ld-linux-x86-64.so.2, does not exist on Android)
    cat > "$TR/bin/render-server" <<EOF
#!/system/bin/sh
exec $GUEST/lib/ld-linux-x86-64.so.2 --library-path $GUEST/lib $GUEST/bin/virgl_render_server "\$@"
EOF
    cat > "$TR/run.sh" <<EOF
#!/system/bin/sh
# Started by init.venus.rc (service vendor.venus_test). Clients connect as other uids.
umask 0
rm -f /dev/socket/vtest
export VK_DRIVER_FILES=$GUEST/icd/lvp_icd.json
export RENDER_SERVER_EXEC_PATH=$GUEST/bin/render-server
export MESA_SHADER_CACHE_DISABLE=true
export LP_NUM_THREADS=2
exec $GUEST/lib/ld-linux-x86-64.so.2 --library-path $GUEST/lib $GUEST/bin/virgl_test_server \\
    --venus --no-virgl --no-fork --multi-clients --socket-path /dev/socket/vtest \\
    > /data/local/tmp/vtest.log 2>&1
EOF
    chmod 755 "$TR/bin/render-server" "$TR/run.sh"
    du -sh "$TR"
    # lavapipe must offer what the server's allocator and Venus need
    sed "s|$GUEST|$TR|" "$TR/icd/lvp_icd.json" > "$TR/../lvp_host_icd.json"
    VK_DRIVER_FILES=$TR/../lvp_host_icd.json vulkaninfo 2>/dev/null |
        grep -E 'deviceName|apiVersion|VK_EXT_external_memory_dma_buf|VK_EXT_image_drm_format_modifier' | sort -u
}

push() {
    local serial=${1:-127.0.0.1:5556}
    [[ -x $TR/run.sh ]] || build
    adb -s "$serial" root >/dev/null; adb -s "$serial" wait-for-device
    adb -s "$serial" shell "rm -rf $GUEST && mkdir -p $GUEST"
    adb -s "$serial" push -q "$TR/." "$GUEST/"
    adb -s "$serial" shell "chmod 755 $GUEST/run.sh $GUEST/bin/* $GUEST/lib/ld-linux-x86-64.so.2"
}

case ${1:-} in
    build) build ;;
    push) shift; push "$@" ;;
    *) sed -n '2,20p' "$0"; exit 2 ;;
esac
