#!/vendor/bin/sh
# Phase G4 (docs/nvidia.md): a short frame-rate benchmark, run on every boot entry so the
# NVK entry (ANGLE on the RTX 4070) can be compared with pcfb (ANGLE on SwiftShader). Started
# by init.pcvktest.rc at sys.boot_completed; waits 90 s (and for pcvktest.sh on the NVK entry),
# then runs once per build, Vulkan driver and GPU entry (marker
# /data/vendor/pcgpu/bench.DRIVER-GPU; remove it to run again). Takes a few minutes (longer on
# SwiftShader) and draws test scenes over the desktop. Writes this boot's pclog directory,
# boot-N/bench/:
#   angeles.txt          San Angeles demo (GLES 1 through ANGLE), on screen: "N fps"
#   hwui-offscreen.json  hwuimacro scenes rendered off screen, their FPS counter is throughput
#   hwui-onscreen.json   the same scenes through SurfaceFlinger and the composer (vsync bound)
#   summary.txt          one line per result, with the entry it ran on
# angeles and hwuimacro are native tests that normally go to /data; they ship as plain files in
# /system_ext/etc/pcvktest/ (the stick update never writes userdata) and run from a copy.
export PATH=/system/bin:/vendor/bin:$PATH
mnt=/mnt/vendor/pclog
src=/system_ext/etc/pcvktest
bin=/data/local/tmp/pcbench
vk=$(getprop ro.hardware.vulkan)
marker=/data/vendor/pcgpu/bench.$vk-$(getprop ro.boot.android_pc.gpu)
k() { echo "pcgfxbench: $*" > /dev/kmsg; }

sleep 90
# On the NVK entry pcvktest.sh's dEQP windows would share the GPU and the screen; wait for it.
for i in $(seq 720); do [ "$(getprop init.svc.vendor.pcvktest)" = running ] || break; sleep 10; done

n=$(cat $mnt/last 2>/dev/null)
[ -n "$n" ] && [ -d $mnt/boot-$n ] || exit 1
fp=$(getprop ro.build.fingerprint)
[ "$(cat $marker 2>/dev/null)" = "$fp" ] && exit 0
d=$mnt/boot-$n/bench
mkdir -p $d $bin /data/vendor/pcgpu || exit 1
for t in angeles hwuimacro; do cp $src/$t $bin/$t && chmod 0755 $bin/$t; done
entry="vulkan=$vk gpu=$(getprop ro.boot.android_pc.gpu) composer=$(getprop ro.boot.vendor.apex.com.android.hardware.graphics.composer)"
mode=$(timeout 10 dumpsys display 2>&1 | grep -m1 -oE 'mActiveSfDisplayMode=[^}]*' |
    grep -oE 'width=[0-9]+|height=[0-9]+|peakRefreshRate=[0-9.]+' | tr '\n' ' ')
echo "$entry display: $mode" > $d/summary.txt
k "start ($entry)"

timeout 300 $bin/angeles > $d/angeles.txt 2>&1
echo "angeles: $(grep -oE '[0-9.]+ fps' $d/angeles.txt || echo failed)" >> $d/summary.txt

scenes="shadowgrid2 rectgrid listview recents savelayer2 backdropblur"
for m in offscreen onscreen; do
    timeout 900 $bin/hwuimacro --$m --count 200 --skip-leak-check --benchmark_format=json \
        $scenes > $d/hwui-$m.json 2>&1
    # json (hwuimacro has no csv): "name": "SCENE", ... "FPS": N per run.
    awk -v m=$m -F'"' '/"name":/ { n = $4 } /"FPS":/ { split($3, v, /[:, ]+/); printf "hwui %s %s: %.1f fps\n", m, n, v[2] }' \
        $d/hwui-$m.json >> $d/summary.txt
done
grep -c fps $d/summary.txt >/dev/null || echo "no results, see the raw files" >> $d/summary.txt
while read -r l; do k "$l"; done < $d/summary.txt
echo "$fp" > $marker
sync
