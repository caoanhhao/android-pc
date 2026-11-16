#!/vendor/bin/sh
# Phase K3: takes the Venus entry off a GPU that stopped working (init.venus.rc service
# vendor.venus_watch, started by pcvenusprobe.sh when Venus runs). NVIDIA's driver logs every GPU
# error as "NVRM: Xid"; most are one app's fault and the GPU goes on, but some (Xid 154 with a
# reset required, after Xid 51 here) leave it unable to create devices, and then every new HWUI
# process aborts in vkCreateDevice. On each Xid this checks whether Venus still creates a device
# (pcvkprobe, at most every 10 s); when it cannot, it switches the vulkan.venus driver to
# SwiftShader (vendor.venus.state=off: new processes only; minigbm then allocates on the DRM
# node again, as on the other entries), turns HWUI to GL and restarts the graphics stack once
# (the composer, and the allocator, which restarts SurfaceFlinger, which restarts zygote and
# every app): the desktop comes back on SwiftShader until the next boot.
log() {
    echo "pcvenuswatch: $*" > /dev/kmsg
    echo "$*" >> /data/vendor/pcgpu/venus.txt
}

fall_back() {
    log "Venus cannot create a device after '$1' ($2): SwiftShader from now on, restarting graphics"
    setprop vendor.venus.state off
    setprop debug.hwui.renderer skiagl
    setprop vendor.venus.probe swiftshader-after-xid
    # the composer (drm_hwcomposer on the NVIDIA entry, ranchu in QEMU) imports the new buffers
    for svc in $(getprop | sed -n 's/^\[init\.svc\.\(vendor\.hwcomposer[^]]*\)\]: \[running\]/\1/p'); do
        setprop ctl.restart "$svc"
    done
    setprop ctl.restart vendor.graphics.allocator
}

last=0
# the whole kernel log, from the boot on: an Xid before this started counts too
cat /dev/kmsg | grep --line-buffered -E 'NVRM: Xid|NV_ERR_RESET_REQUIRED' | while read -r line; do
    now=$(cut -d. -f1 /proc/uptime)
    [ $((now - last)) -lt 10 ] && continue
    last=$now
    xid=${line#*NVRM: }
    if result=$(/system_ext/bin/pcvkprobe virtio --check); then
        log "after '$xid': Venus still works ($result)"
        continue
    fi
    fall_back "$xid" "$result"
    exit 0
done
