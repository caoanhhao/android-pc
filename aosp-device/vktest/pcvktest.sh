#!/vendor/bin/sh
# Phase G3 (docs/nvidia.md): Vulkan on the RTX 4070 (NVK, vulkan.nouveau). Started by
# init.pcvktest.rc when gpuservice starts on the NVK entry; writes into this boot's pclog directory
# (boot-N/vk/, see pclog.sh), once per build (a marker keeps later boots of the same build
# from running the tests again; `rm /data/vendor/pcgpu/vktest.done` forces a rerun):
#   vkjson.json     `cmd gpu vkjson`, Android's vulkaninfo (devices, properties, extensions)
#   props.txt       the Vulkan HAL properties and which driver libvulkan loaded
#   deqp-GROUP.qpa  dEQP-VK log of each smoke group, deqp-GROUP.txt its result counts
#   summary.txt     one line per group: pass / fail / not supported / other
# The dEQP APK (com.drawelements.deqp, from external/deqp) ships in
# /system_ext/etc/pcvktest/ and is installed here; it runs the cases through
# DeqpInstrumentation, so wsi.android gets a real window.
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

# The smoke groups of G3: the API (with AHardwareBuffer interop), memory, Android WSI.
groups="api.smoke api.info api.device_init api.external.memory.android_hardware_buffer
memory.allocation.basic memory.requirements memory.mapping.suballocation.full wsi.android"
: > $d/summary.txt
for g in $groups; do
    log=/sdcard/deqp-$g.qpa
    k "dEQP-VK.$g.* start"
    timeout 3600 am instrument -w \
        -e deqpLogFilename $log \
        -e deqpCmdLine "--deqp-case=dEQP-VK.$g.* --deqp-log-images=disable --deqp-log-shader-sources=disable --deqp-watchdog=enable" \
        -e deqpLogData false \
        com.drawelements.deqp/com.drawelements.deqp.testercore.DeqpInstrumentation \
        > $d/deqp-$g.txt 2>&1
    # The app writes /sdcard; init services see it as /data/media/0.
    cp /data/media/0/deqp-$g.qpa $d/deqp-$g.qpa 2>/dev/null
    # qpa: "#beginTestCaseResult CASE" ... <Result StatusCode="..."> per case. Quality and
    # compatibility warnings count as passes, as in CTS.
    s=$(awk -v g="dEQP-VK.$g" -v f=$d/failures-$g.txt '
        /^#beginTestCaseResult/ { c = $2 }
        /<Result StatusCode=/ {
            match($0, /StatusCode="[A-Za-z]+"/); r = substr($0, RSTART + 12, RLENGTH - 13)
            if (r ~ /^(Pass|QualityWarning|CompatibilityWarning)$/) p++
            else if (r == "NotSupported") ns++
            else { x++; print c, r > f }
        }
        END { printf "%s: pass %d fail %d notsupported %d\n", g, p, x, ns }' $d/deqp-$g.qpa 2>/dev/null)
    [ -n "$s" ] || s="dEQP-VK.$g: no results (see deqp-$g.txt)"
    echo "$s" >> $d/summary.txt
    k "$s"
    sync
done
echo "$fp" > $marker
k "done"
