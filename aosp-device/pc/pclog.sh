#!/vendor/bin/sh
# Boot log saver for the real PC (init.pc.rc): mounts the stick's FAT partition "pclog" and
# writes boot-N/{dmesg,logcat}.* plus state dumps, syncing every few seconds.
#
# Size limits (the 256 MiB partition must never fill up; docs/usb-boot-aosp17.md §5):
# - dmesg and logcat are written in pieces (split): the first piece(s) are kept, then only the
#   last few, so one boot takes at most about 40 MiB. Pieces dropped in between are listed
#   in boot-N/pruned.txt. Read a log with `cat boot-N/logcat.*`.
# - at most $keep boots stay; older ones, and more if the partition has less than $reserve_kb
#   free, are deleted before this boot's directory is made.
export PATH=/system/bin:/vendor/bin:$PATH   # logcat, dumpsys: system tools
dev=/dev/block/by-name/pclog
mnt=/mnt/vendor/pclog   # not under /mnt directly: vold unmounts those at start
keep=5                  # boots kept, this one included
reserve_kb=49152        # free space wanted for this boot before it starts
for i in 1 2 3 4 5 6 7 8 9 10; do [ -e $dev ] && break; sleep 1; done
mkdir -p $mnt
mount -t vfat -o rw,noatime,flush $dev $mnt || exit 1

free_kb() { set -- $(df -k $mnt | tail -n 1); echo ${4:-0}; }
# Boot numbers on the partition, oldest first.
boots() { ls -d $mnt/boot-* 2>/dev/null | sed 's/.*boot-//' | grep -E '^[0-9]+$' | sort -n; }
oldest() { boots | head -n 1; }

n=$(cat $mnt/last 2>/dev/null)
case "$n" in ''|*[!0-9]*) n=$(boots | tail -n 1) ;; esac
n=$((${n:-0} + 1))
while [ $(boots | wc -l) -ge $keep ] || [ $(free_kb) -lt $reserve_kb ]; do
    o=$(oldest)
    [ -n "$o" ] || break
    rm -rf $mnt/boot-$o
done
d=$mnt/boot-$n
mkdir $d || { rm -rf $mnt/boot-*; mkdir $d; } || exit 1
echo $n > $mnt/last
# Tombstones the previous boot had already seen (name size mtime), so that only new ones are
# copied below (J2). Compared as text, not by time: this PC's clock is often hours off.
mv $mnt/tombstones.seen $d/.tombstones-before 2>/dev/null || : > $d/.tombstones-before
# Phase K1b: what the kernel saved from a previous boot that died (panic, oops, or a lockup
# turned into a panic) through pstore: on the PC the backend is efi-pstore (EFI variables,
# CONFIG_EFI_VARS_PSTORE). Copied, then removed: they would fill the board's NVRAM otherwise.
if ls /sys/fs/pstore/* >/dev/null 2>&1; then
    mkdir -p $d/pstore && cp /sys/fs/pstore/* $d/pstore/ && rm -f /sys/fs/pstore/*
fi

# log NAME PIECE-SIZE HEAD TAIL: stdin into NAME.aaa, NAME.aab, ... of PIECE-SIZE bytes; prune
# keeps the first HEAD and the last TAIL pieces.
log() { split -a 3 -b $2 - $d/$1.; }
prune() {   # prune NAME HEAD TAIL
    local pieces cnt i=0 p
    pieces=$(ls $d/$1.??? 2>/dev/null)
    cnt=$(echo "$pieces" | grep -c .)
    for p in $pieces; do
        i=$((i + 1))
        [ $i -le $2 ] && continue
        [ $i -gt $((cnt - $3)) ] && break
        rm -f $p && echo "${p##*/}" >> $d/pruned.txt
    done
}
dmesg -w 2>&1 | log dmesg 2m &
(while true; do logcat -b all -v threadtime 2>&1; sleep 2; done) | log logcat 4m &
dump() {
    {
        echo "== uptime"; cat /proc/uptime
        echo "== getprop"; getprop
        echo "== ps"; ps -A -o PID,PPID,USER,STAT,LABEL,NAME
        echo "== lsmod"; lsmod
        echo "== dev"; ls -l /dev/dri /dev/input /dev/snd /dev/video* /dev/block/by-name 2>&1
        echo "== net"; ip addr; ip route show table all 2>&1 | head -50
        echo "== drm"; cat /sys/kernel/debug/dri/*/state 2>&1 | head -80
        echo "== dumpsys SurfaceFlinger"; timeout 10 dumpsys SurfaceFlinger 2>&1 | head -120
        echo "== dumpsys window"; timeout 10 dumpsys window displays 2>&1 | head -60
        echo "== input windows"; timeout 10 dumpsys input 2>&1 | sed -n '/^Input Dispatcher State/,/^ *MonitoringChannels/p' | grep -E '^ *(FocusedApplications|FocusedWindows|TouchStatesByDisplay|Windows|[0-9]+: name=)' | cut -c1-400 | head -80
        echo "== dumpsys ethernet"; timeout 10 dumpsys ethernet 2>&1 | head -60
        echo "== services"; timeout 10 service list 2>&1
        echo "== dumpsys battery"; timeout 10 dumpsys battery 2>&1
        echo "== dumpsys thermalservice"; timeout 10 dumpsys thermalservice 2>&1
        echo "== dumpsys power"; timeout 10 dumpsys power 2>&1 | grep -iE "hal|mode|boost|battery|plugged"
        echo "== sysfs power"; ls -lZ /sys/class/power_supply/ /sys/class/hwmon/ 2>&1
        grep -H . /sys/class/hwmon/hwmon*/name /sys/class/thermal/thermal_zone*/type 2>&1
        grep -H . /sys/devices/system/cpu/intel_pstate/* /sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference /sys/devices/system/cpu/cpufreq/policy0/scaling_driver 2>&1
        ls -lZ /sys/devices/system/cpu/intel_pstate/ /sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference 2>&1
        echo "== selinux"; getenforce
        echo "== avc"; dmesg | grep -c 'avc: denied'
        echo "== avc by scontext"; dmesg | grep 'avc: denied' | grep -o 'scontext=[^ ]*' | sort | uniq -c
        echo "== avc hal_pc"; dmesg | grep 'avc: denied' | grep 'hal_pc_'
        echo "== hal services"; ps -AZ | grep -E 'hal_pc_|hal_health|hal_thermal|hal_power'
        echo "== dumpsys bluetooth_manager"; timeout 10 dumpsys bluetooth_manager 2>&1 | head -150
        echo "== dumpsys wifi"; timeout 10 dumpsys wifi 2>&1 | head -150
        echo "== iw"; ip link show wlan0 2>&1; ip link show wlan1 2>&1
        echo "== dumpsys media.camera"; timeout 10 dumpsys media.camera 2>&1 | head -150
        echo "== dumpsys media.audio_flinger"; timeout 10 dumpsys media.audio_flinger 2>&1 | head -150
        echo "== asound"; cat /proc/asound/cards /proc/asound/pcm 2>&1; grep -H . /proc/asound/card*/pcm*/sub0/status 2>&1
        echo "== tinymix"; timeout 5 tinymix -D 0 2>&1 | head -80
        echo "== hdmi eld"; grep -H -E 'monitor_present|eld_valid|monitor_name|connection_type' /proc/asound/card*/eld#* 2>&1
        for c in /proc/asound/card[1-9]; do
            echo "== tinymix ${c#/proc/asound/card}"; timeout 5 tinymix -D ${c#/proc/asound/card} 2>&1 | head -60
        done
        echo "== audio devices"; timeout 10 dumpsys audio 2>&1 | grep -iE 'hdmi|wired|connected' | head -30
        # The GPU: boot entry, PCI display devices (and the NVIDIA HDMI codec), KMS connectors.
        echo "== gpu boot entry"; getprop ro.boot.android_pc.gpu
        echo "== pci display"; for p in /sys/bus/pci/devices/*; do
            case $(cat $p/class) in 0x03*|0x0403*)
                echo "${p##*/} $(cat $p/vendor):$(cat $p/device) class $(cat $p/class) driver $(basename "$(readlink $p/driver)") power $(cat $p/power/runtime_status 2>/dev/null)" ;;
            esac
        done
        echo "== drm connectors"; for c in /sys/class/drm/card*-*; do
            echo "${c##*/}: status $(cat $c/status) enabled $(cat $c/enabled) dpms $(cat $c/dpms) edid $(wc -c < $c/edid) bytes"
            head -n 5 $c/modes | sed 's/^/    mode /'
        done
        # Phase K1 (docs/nvidia-proprietary.md): NVIDIA's own kernel modules on the NVIDIA entry:
        # what loaded, the driver's dmesg (NVRM, GSP, nvidia-drm), its /proc view, device nodes.
        if [ "$(getprop ro.boot.android_pc.gpu)" = nvidia ]; then
            echo "== nvidia modules"; grep -E '^(nvidia|nvidia_modeset|nvidia_drm|nvidia_uvm|nouveau|nova|nova_core|vkms) ' /proc/modules
            cat /proc/cmdline | tr ' ' '\n' | grep -E '^(cfi|module_blacklist|pcfb)'
            echo "== nvidia dmesg"; dmesg | grep -iE 'nvrm|nvidia|nvkms|gsp|pcnvidia|cfi failure|firmware' | head -n 200
            echo "== /proc/driver/nvidia"; for f in /proc/driver/nvidia/version /proc/driver/nvidia/params \
                /proc/driver/nvidia/registry /proc/driver/nvidia/gpus/*/information /proc/driver/nvidia/gpus/*/power; do
                [ -f "$f" ] && { echo "-- $f"; head -n 60 "$f"; }
            done
            echo "== /dev/nvidia*"; ls -lZ /dev/nvidia* /dev/nvidia-caps 2>&1; ls -lZ /dev/dri 2>&1
            echo "== drm cards"; for c in /sys/class/drm/card? /sys/class/drm/renderD*; do
                echo "${c##*/}: $(basename "$(readlink $c/device/driver)")"
            done
            echo "== modetest -c nvidia-drm"; timeout 10 modetest -M nvidia-drm -c 2>&1 | head -80
        fi
        # Phase L3 (docs/nvidia-native.md): NVIDIA's Vulkan driver in-process (vulkan.nvidia through
        # nvglibc): what the graphics stack chose, the HAL's and its clients' log lines, and the
        # renderer HWUI really uses.
        if [ "$(getprop ro.hardware.vulkan)" = nvidia ]; then
            echo "== vulkan.nvidia"
            getprop | grep -E 'ro.hardware.(vulkan|egl)|ro.vulkan.apex|renderengine|hwui.renderer'
            logcat -d -b all | grep -E 'vulkan.nvidia|nvglibc|RenderEngine|VulkanManager|vulkan  |minigbm' | head -n 150
            echo "== gfxinfo systemui"; timeout 10 dumpsys gfxinfo com.android.systemui 2>&1 | grep -iE 'pipeline|vulkan|driver|janky|total frames' | head -n 20
        fi
        for c in /sys/class/sound/card*; do
            [ "$(cat $c/device/vendor 2>/dev/null):$(cat $c/device/device 2>/dev/null)" = 0x10de:0x22bc ] || continue
            echo "== eld ${c##*/} (10de:22bc)"
            for e in /proc/asound/${c##*/}/eld#*; do echo "-- $e"; cat $e; done
        done
        # Phase G2: which composer runs, and drm_hwcomposer's own view (displays, modes, planes).
        echo "== composer"; getprop ro.boot.vendor.apex.com.android.hardware.graphics.composer
        ps -A -o PID,USER,NAME | grep -i composer
        timeout 10 dumpsys android.hardware.graphics.composer3.IComposer/default 2>&1 | head -150
        echo "== displays"; timeout 10 dumpsys display 2>&1 | grep -E 'DisplayDeviceInfo|mBaseDisplayInfo|mActiveSfDisplayMode|supportedModes' | cut -c1-600 | head -20
        # Phase K4 (docs/nvidia-proprietary.md): drm_hwcomposer on nvidia-drm, the card pcnvdisplay
        # found, KMS state, planes (IN_FORMATS).
        if [ "$(getprop ro.boot.android_pc.gpu)" = nvidia ]; then
            echo "== nvdisplay"; getprop vendor.pc.nvdisplay; getprop vendor.hwc.drm.device
            dmesg | grep -E 'pcnvdisplay|nvidia-drm' | head -40
            echo "== modetest nvidia-drm"; timeout 10 modetest -M nvidia-drm -c -e 2>&1 | head -80
            timeout 10 modetest -M nvidia-drm -p 2>&1 | grep -E '^[0-9]+\s|type:|value: [0-9]$|AB24:' | cut -c1-300 | head -60
        fi
        # Who renders GLES (ANGLE's renderer string names the Vulkan driver), whether any process
        # still maps SwiftShader (pastel), and per-layer frame rates (timestats is switched on at
        # boot below, so a game's FPS is here on every entry).
        echo "== gles"; getprop | grep -E 'hardware\.(egl|vulkan)\]|opengles|vulkan_probe'
        timeout 10 dumpsys SurfaceFlinger 2>&1 | grep -iE 'GLES:|RenderEngine|EGL (vendor|version)' | head -10
        echo "== swiftshader users"; grep -lE 'swiftshader|vulkan\.pastel' /proc/[0-9]*/maps 2>/dev/null |
            while read -r m; do p=${m%/maps}; echo "${p#/proc/} $(cat $p/comm)"; done
        echo "== timestats"; timeout 10 dumpsys SurfaceFlinger --timestats -dump -maxlayers 8 2>&1 |
            grep -E 'layerName|packageName|totalFrames|averageFPS|displayRefreshRate|displayOnTime|missedFrames|clientCompositionFrames|refreshRateSwitches' | head -80
        # GPU temperature as the thermal HAL sees it, GPU faults and hangs over the whole boot
        # (NVRM Xid), suspend/resume.
        echo "== gpu thermal"; timeout 10 dumpsys thermalservice 2>&1 | grep -iE 'GPU|fan' | head -20
        echo "== gpu faults"; dmesg | grep -ciE 'gsp.*(error|fail|timeout)|Xid'
        dmesg | grep -iE 'gsp.*(error|fail|timeout)|Xid' | tail -n 60
        echo "== suspend"; echo "mem_sleep: $(cat /sys/power/mem_sleep)"
        grep -H . /sys/power/suspend_stats/* 2>&1 | grep -vE 'last_failed_(dev|errno|step)$'
        echo "last_resume_reason: $(cat /sys/kernel/wakeup_reasons/last_resume_reason 2>&1)"
        echo "suspend_blocker: $(getprop init.svc.suspend_blocker)"
        dmesg | grep -E 'PM: suspend (entry|exit)|ACPI: PM: (Preparing|Waking)' | tail -n 30
        echo "== media files"; ls -lR /data/media/0/DCIM /data/media/0/Recordings /data/media/0/Music 2>&1 | head -40
    } > $d/state-$1.txt 2>&1
    # Only this boot's tombstones: /data/tombstones keeps the last 32 across boots, and old
    # ones copied into every boot made a fixed crash look like a crash loop (J2).
    mkdir -p $d/tombstones
    stat -c '%n %s %Y' /data/tombstones/tombstone_?? 2>/dev/null > $d/.tombstones-now
    if [ -s $d/.tombstones-now ]; then
        grep -vxF -f $d/.tombstones-before $d/.tombstones-now | while read -r f _; do
            cp $f $d/tombstones/
        done
        cp $d/.tombstones-now $mnt/tombstones.seen
    fi
    # Raw EDID of each DRM connector that has one (decode on the host: edid-decode FILE).
    for c in /sys/class/drm/card*-*; do
        [ -s $c/edid ] && cp $c/edid $d/edid-${c##*/}.bin
    done
    # Unreal games write their own log (Saved/Logs, internal or external storage); logcat does
    # not have it, and it is the only place that says what the game did before a crash. Keep
    # the last 1 MiB of the newest few, as boot-N/applogs/PACKAGE-FILE.
    for f in $(ls -t /data/data/*/files/UE4Game/*/*/Saved/Logs/*.log \
            /data/media/0/Android/data/*/files/UE4Game/*/*/Saved/Logs/*.log 2>/dev/null | head -n 6); do
        p=${f#/data/*/}; p=${p#0/Android/data/}
        mkdir -p $d/applogs && tail -c 1048576 "$f" > "$d/applogs/${p%%/*}-${f##*/}"
    done
}
# dmesg: first 2 MiB + last 8 MiB; logcat: first 8 MiB + last 16 MiB (the newest piece is
# still being written, so it may be smaller). Plus state dumps: about 40 MiB a boot.
(while true; do sleep 3; prune dmesg 1 4; prune logcat 2 4; sync; done) &
# SurfaceFlinger's frame statistics for the `== timestats` section, on every entry, so a game's
# frame rate on the NVIDIA entry can be set against the same game on pcfb.
(for i in $(seq 60); do [ "$(getprop init.svc.surfaceflinger)" = running ] && break; sleep 2; done
 sleep 20; dumpsys SurfaceFlinger --timestats -enable > /dev/null 2>&1) &
# Phase I4 (docs/nvidia.md on branch gpu-alternatives): the audio check (440 Hz tone + mic
# recording), the HDMI beeps and G4's frame-rate benchmark passed on the real PC and are gone; delete what they left behind
# on the pclog partition and in /data (the stick update rewrites neither).
rm -f $mnt/audio-check.dex
(for i in $(seq 120); do [ "$(getprop sys.boot_completed)" = 1 ] && break; sleep 5; done
 rm -rf /data/local/tmp/pcbench /data/local/tmp/audio-check.dex /data/local/tmp/beep.wav \
     /data/local/tmp/beep-period.raw) &
# Phase L2 (docs/nvidia-native.md), NVIDIA entry: nvglibc_vktest loads NVIDIA's glibc Vulkan
# driver into a bionic process through nvglibc and runs a compute dispatch on the GPU, once per
# boot, 30 s after boot completed, into boot-N/nvglibc-vktest.txt (exit status at the end).
if [ "$(getprop ro.boot.android_pc.gpu)" = nvidia ]; then
    (
        for i in $(seq 120); do [ "$(getprop sys.boot_completed)" = 1 ] && break; sleep 5; done
        sleep 30
        f=$d/nvglibc-vktest.txt
        echo "== $(cut -d' ' -f1 /proc/uptime) s: nvglibc_vktest" > $f
        __GL_SHADER_DISK_CACHE=0 NVGLIBC_DEBUG=1 timeout 120 /vendor/bin/nvglibc_vktest >> $f 2>&1
        echo "== exit $? at $(cut -d' ' -f1 /proc/uptime) s" >> $f
        dmesg | grep -E 'NVRM: Xid' | tail -n 20 >> $f
        sync
    ) &
fi
# Screenshot snapshots: when the user takes a screenshot (Print Screen, or power + volume
# down), copy it to boot-N/snap-K/ with the window, input and activity state of that moment,
# so a white screen or a dead click can be matched with what was on screen. At most 10.
(
    shots=/data/media/0/Pictures/Screenshots
    k=0 seen=$(ls $shots 2>/dev/null | wc -l)
    while [ $k -lt 10 ]; do
        sleep 3
        now=$(ls $shots 2>/dev/null | wc -l)
        [ "$now" -gt "$seen" ] || { seen=$now; continue; }
        seen=$now k=$((k + 1)) s=$d/snap-$k
        mkdir -p $s
        cp "$shots/$(ls -t $shots | head -n 1)" $s/ 2>/dev/null
        {
            echo "== uptime"; cat /proc/uptime
            echo "== dumpsys window windows"; timeout 10 dumpsys window windows 2>&1 | grep -E 'Window #|mOwnerUid|mFrame=|isVisible|mHasSurface|mDrawState|Requested w=' | head -200
            echo "== dumpsys window displays"; timeout 10 dumpsys window displays 2>&1 | grep -E 'mCurrentFocus|mFocusedApp|mInputMethodTarget|mObscuringWindow'
            echo "== dumpsys input"; timeout 10 dumpsys input 2>&1 | sed -n '/^Input Dispatcher State/,/^ *MonitoringChannels/p' | cut -c1-500 | head -300
            echo "== dumpsys activity top"; timeout 10 dumpsys activity top 2>&1 | head -600
            echo "== dumpsys webviewupdate"; timeout 10 dumpsys webviewupdate 2>&1 | head -40
            echo "== mouse_as_touch"; getprop persist.sys.pc.mouse_as_touch
        } > $s/state.txt 2>&1
    done
) &
for t in 30 60 120 180 300 600 900 1200 1500 1800 2400 3000 3600; do
    sleep $((t - ${prev:-0}))
    prev=$t
    dump $t &
done
wait
