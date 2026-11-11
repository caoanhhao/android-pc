#!/vendor/bin/sh
# Boot log saver for the real PC (init.pc.rc): mounts the stick's FAT partition "pclog" and
# writes boot-N/{dmesg,logcat}.txt plus state dumps, syncing every few seconds.
export PATH=/system/bin:/vendor/bin:$PATH   # logcat, dumpsys: system tools
dev=/dev/block/by-name/pclog
mnt=/mnt/vendor/pclog   # not under /mnt directly: vold unmounts those at start
for i in 1 2 3 4 5 6 7 8 9 10; do [ -e $dev ] && break; sleep 1; done
mkdir -p $mnt
mount -t vfat -o rw,noatime,flush $dev $mnt || exit 1
n=1
while [ -e $mnt/boot-$n ]; do n=$((n + 1)); done
d=$mnt/boot-$n
mkdir $d
echo $n > $mnt/last
dmesg -w > $d/dmesg.txt 2>&1 &
(while true; do logcat -b all -v threadtime >> $d/logcat.txt 2>&1; sleep 2; done) &
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
        echo "== dumpsys ethernet"; timeout 10 dumpsys ethernet 2>&1 | head -60
        echo "== services"; timeout 10 service list 2>&1
        echo "== avc"; dmesg | grep -c 'avc: denied'
    } > $d/state-$1.txt 2>&1
    mkdir -p $d/tombstones && cp /data/tombstones/tombstone_?? $d/tombstones/ 2>/dev/null
}
(while true; do sleep 3; sync; done) &
for t in 30 60 120 180 300 600; do
    sleep $((t - ${prev:-0}))
    prev=$t
    dump $t &
done
wait
