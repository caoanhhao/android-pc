#!/vendor/bin/sh
# Phase G1 (docs/nvidia.md), started by init.pc.rc on the nouveau boot entry only: keeps the
# preferred mode set on every connected connector of the nouveau card, so the monitor's
# HDMI/DP link is up and the NVIDIA HD Audio codec gets its ELD. modetest -r stays DRM master
# and keeps the mode until its stdin closes; it is restarted if it exits (no monitor yet,
# unplugged). Its output goes to /data/vendor/pcgpu/ (pclog.sh copies it).
export PATH=/system/bin:/vendor/bin:$PATH
out=/data/vendor/pcgpu
mkdir -p $out
k() { echo "pcgpu: $*" > /dev/kmsg; }
for i in $(seq 30); do
    [ "$(basename "$(readlink /sys/class/drm/card0/device/driver)")" = nouveau ] && break
    sleep 1
done
k "card0 driver: $(basename "$(readlink /sys/class/drm/card0/device/driver)")"
n=0
while true; do
    n=$((n + 1))
    k "modetest -r, try $n"
    # stdin is a FIFO held open by a sleep, killed once modetest exits (a "sleep | modetest"
    # pipeline would wait for the sleep and never retry).
    rm -f $out/hold && mkfifo $out/hold
    sleep 2147483 > $out/hold &
    hold=$!
    modetest -M nouveau -r < $out/hold > $out/modetest-$n.txt 2>&1
    rc=$?
    kill $hold
    k "modetest exited ($rc), see $out/modetest-$n.txt"
    [ $n -ge 20 ] && n=0
    sleep 10
done
