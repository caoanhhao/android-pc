#!/vendor/bin/sh
# Phase G3/G4 (docs/nvidia.md): Vulkan (NVK, vulkan.nouveau) and GLES (ANGLE on NVK) on the
# RTX 4070. Started by init.pcvktest.rc when gpuservice starts on the NVK entry; writes into
# this boot's pclog directory (see pclog.sh), once per build (a marker keeps later boots of
# the same build from running the tests again; `rm /data/vendor/pcgpu/vktest.done` forces a
# rerun):
#   vk/vkjson.json     `cmd gpu vkjson`, Android's vulkaninfo (devices, properties, extensions)
#   vk/props.txt       the Vulkan HAL properties and which driver libvulkan loaded
#   vk/deqp-GROUP.qpa  dEQP-VK log of each smoke group, deqp-GROUP.txt its result counts
#   vk/summary.txt     one line per group: pass / fail / not supported
#   gles/...           the same for the dEQP-GLES3 smoke groups (G4), plus gles/egl.txt
# The dEQP APK (com.drawelements.deqp, from external/deqp) ships in
# /system_ext/etc/pcvktest/ and is installed here; it runs the cases through
# DeqpInstrumentation, so wsi.android and the GLES tests get a real window.
export PATH=/system/bin:/vendor/bin:$PATH
mnt=/mnt/vendor/pclog
apk=/system_ext/etc/pcvktest/com.drawelements.deqp.apk
marker=/data/vendor/pcgpu/vktest.done
k() { echo "pcvktest: $*" > /dev/kmsg; }

# pclog.sh mounts the partition and picks this boot's directory; wait for it.
for i in $(seq 60); do
    n=$(cat $mnt/last 2>/dev/null)
    [ -n "$n" ] && [ -d $mnt/boot-$n ] && break
    sleep 2
done
d=$mnt/boot-$n/vk
mkdir -p $d /data/vendor/pcgpu || exit 1
k "results in boot-$n/vk"

{
    getprop | grep -iE 'vulkan|hardware\.(egl|gralloc|hwcomposer)|android_pc\.gpu'
    echo "== libvulkan"; logcat -d -s vulkan:* 2>&1 | tail -n 40
    echo "== dri"; ls -l /dev/dri
    echo "== vulkan.nouveau"; ls -l /apex/com.google.cf.vulkan/lib64/ 2>&1
} > $d/props.txt 2>&1
timeout 60 cmd gpu vkjson > $d/vkjson.json 2>&1
grep -E '"deviceName"|"driverName"|"driverInfo"|"apiVersion"' $d/vkjson.json | head -n 8 |
    while read -r l; do k "$l"; done
sync

# dEQP needs the package manager and a window.
for i in $(seq 120); do [ "$(getprop sys.boot_completed)" = 1 ] && break; sleep 5; done
if [ "$(getprop sys.boot_completed)" != 1 ]; then
    k "Android did not finish booting in 10 min, no dEQP run"
    exit 1
fi
fp=$(getprop ro.build.fingerprint)
if [ "$(cat $marker 2>/dev/null)" = "$fp" ]; then
    k "dEQP already ran for this build"
    exit 0
fi
pm install -t -r -g $apk > $d/install.txt 2>&1 || { k "dEQP install failed"; exit 1; }

# run SUITE DIR GROUPS [EXTRA-ARGS]: each group's cases through DeqpInstrumentation.
run() {
    suite=$1 out=$2 groups=$3 extra=$4
    mkdir -p $out
    : > $out/summary.txt
    for g in $groups; do
        log=/sdcard/deqp-$suite-$g.qpa
        k "dEQP-$suite.$g.* start"
        timeout 3600 am instrument -w \
            -e deqpLogFilename $log \
            -e deqpCmdLine "--deqp-case=dEQP-$suite.$g.* --deqp-log-images=disable --deqp-log-shader-sources=disable --deqp-watchdog=enable $extra" \
            -e deqpLogData false \
            com.drawelements.deqp/com.drawelements.deqp.testercore.DeqpInstrumentation \
            > $out/deqp-$g.txt 2>&1
        # The app writes /sdcard; init services see it as /data/media/0.
        cp /data/media/0/deqp-$suite-$g.qpa $out/deqp-$g.qpa 2>/dev/null
        # qpa: "#beginTestCaseResult CASE" ... <Result StatusCode="..."> per case. Quality and
        # compatibility warnings count as passes, as in CTS.
        s=$(awk -v g="dEQP-$suite.$g" -v f=$out/failures-$g.txt '
            /^#beginTestCaseResult/ { c = $2 }
            /<Result StatusCode=/ {
                match($0, /StatusCode="[A-Za-z]+"/); r = substr($0, RSTART + 12, RLENGTH - 13)
                if (r ~ /^(Pass|QualityWarning|CompatibilityWarning)$/) p++
                else if (r == "NotSupported") ns++
                else { x++; print c, r > f }
            }
            END { printf "%s: pass %d fail %d notsupported %d\n", g, p, x, ns }' $out/deqp-$g.qpa 2>/dev/null)
        [ -n "$s" ] || s="dEQP-$suite.$g: no results (see deqp-$g.txt)"
        echo "$s" >> $out/summary.txt
        k "$s"
        sync
    done
}

# G3's smoke groups: the API (with AHardwareBuffer interop), memory, Android WSI.
run VK $d "api.smoke api.info api.device_init api.external.memory.android_hardware_buffer
memory.allocation.basic memory.requirements memory.mapping.suballocation.full wsi.android"

# G4: GLES 3.0 through ANGLE on NVK. Which EGL/GLES the system uses (ANGLE's renderer string
# names the Vulkan driver), then the smoke groups, in a window with CTS's config.
gd=$mnt/boot-$n/gles
mkdir -p $gd
{
    getprop | grep -iE 'hardware\.(egl|vulkan)|opengles|angle|renderengine|hwui'
    echo "== SurfaceFlinger"
    timeout 10 dumpsys SurfaceFlinger 2>&1 | grep -iE 'GLES|EGL|RenderEngine|vulkan|skia' | head -20
} > $gd/egl.txt 2>&1
run GLES3 $gd "info functional.prerequisite functional.implementation_limits functional.color_clear
functional.depth_stencil_clear functional.buffer.write functional.texture.format
functional.fbo.completeness functional.draw.draw_arrays functional.shaders.linkage" \
    "--deqp-gl-config-name=rgba8888d24s8ms0 --deqp-surface-type=window --deqp-screen-rotation=unspecified"
echo "$fp" > $marker
k "done"
