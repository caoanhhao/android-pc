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
        echo "== media files"; ls -lR /data/media/0/DCIM /data/media/0/Recordings /data/media/0/Music 2>&1 | head -40
    } > $d/state-$1.txt 2>&1
    mkdir -p $d/tombstones && cp /data/tombstones/tombstone_?? $d/tombstones/ 2>/dev/null
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
# Audio check (tools/audio-check/AudioCheck.java, copied to the pclog partition as
# audio-check.dex by tools/mkusb-aosp17.sh): a 440 Hz tone, then a mic recording.
if [ -f $mnt/audio-check.dex ]; then
    (
        for t in 240 480; do
            sleep $((t - ${prev:-0}))
            prev=$t
            cp $mnt/audio-check.dex /data/local/tmp/audio-check.dex
            # This service starts on "init", before /data is mounted and before init
            # exports BOOTCLASSPATH & co. (derive_classpath writes them at post-fs-data);
            # app_process aborts without them.
            while read -r cmd name value; do
                [ "$cmd" = export ] && export "$name=$value"
            done < /data/system/environ/classpath
            {
                echo "== $t s: play 5 s 440 Hz"
                CLASSPATH=/data/local/tmp/audio-check.dex timeout 30 app_process /system/bin AudioCheck play 5 440
                echo "== $t s: record 10 s"
                CLASSPATH=/data/local/tmp/audio-check.dex timeout 30 app_process /system/bin AudioCheck record 10
            } >> $d/audio-check.txt 2>&1
        done
    ) &
fi
# HDMI check: a 3 s, 375 Hz beep on each HDMI PCM in turn (4 s apart), straight through
# ALSA, so the log says which PCM (if any) the monitor plays.
(
    sleep 150
    w=/data/local/tmp/beep.wav p=/data/local/tmp/beep-period.raw
    : > $p
    for i in $(seq 64); do printf '\000\020\000\020' >> $p; done
    for i in $(seq 64); do printf '\000\360\000\360' >> $p; done
    # RIFF header: 48 kHz, stereo, 16 bit, 576000 data bytes (3 s).
    printf 'RIFF\044\312\010\000WAVEfmt \020\000\000\000\001\000\002\000' > $w
    printf '\200\273\000\000\000\356\002\000\004\000\020\000data\000\312\010\000' >> $w
    for i in $(seq 1125); do cat $p; done >> $w
    grep HDMI /proc/asound/pcm | while IFS=- read -r card rest; do
        dev=${rest%%:*}
        echo "== $(cut -d' ' -f1 /proc/uptime) s: beep on card $((10#$card)) device $((10#$dev))"
        timeout 10 tinyplay $w -D $((10#$card)) -d $((10#$dev)) 2>&1
        echo "exit $?"
        sleep 4
    done >> $d/hdmi-check.txt 2>&1
) &
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
