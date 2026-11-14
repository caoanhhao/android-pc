#!/vendor/bin/sh
# Phase G5 (docs/nvidia.md), started by init.pc.rc on the nouveau boot entries once the boot
# completes, after it stopped Cuttlefish's suspend_blocker: Android now suspends the PC when
# the screen is off and nothing holds a wakelock (system_suspend writes "mem" to
# /sys/power/state: S3 if the firmware offers it, see /sys/power/mem_sleep). pcfb entries
# keep suspend_blocker: the UEFI GOP framebuffer is not restored after a resume.
#
# This script:
# - lets USB keyboards wake the PC (the keyboard, its hubs and the xHCI controller); the
#   power button wakes it anyway. Mice stay off, so a nudged desk does not wake it;
# - turns the screen on after a wake that did not come from an alarm: a USB keyboard's key or
#   the ACPI power button wake the kernel, but Android gets no key that wakes the screen (the
#   ACPI button driver swallows the press that woke it), so it would suspend again;
# - holds a wakelock while pcvktest.sh or pcgfxbench.sh (G3/G4 tests, first boot of a build)
#   run unattended, so a screen timeout does not suspend them halfway.
export PATH=/system/bin:/vendor/bin:$PATH
k() { echo "pcwake: $*" > /dev/kmsg; }

k "mem_sleep: $(cat /sys/power/mem_sleep)"
for i in /sys/bus/usb/devices/*:*; do
    [ "$(cat $i/bInterfaceClass 2>/dev/null)" = 03 ] &&
        [ "$(cat $i/bInterfaceProtocol 2>/dev/null)" = 01 ] || continue
    p=$(readlink -f $i)
    p=${p%/*}
    while [ -n "$p" ] && [ "$p" != /sys/devices ]; do
        [ -f $p/power/wakeup ] && echo enabled > $p/power/wakeup
        p=${p%/*}
    done
    k "keyboard ${i##*/} may wake the PC"
done

# Wakeup sources an alarm wakes through: the kernel's alarmtimer and the RTC, whose wakeup
# source is named after its device (rtc_cmos: the PNP device "00:04"), which has an rtc/.
alarms=
for w in /sys/class/wakeup/wakeup*; do
    case "$(cat $w/name)" in
        *rtc*|*alarmtimer*) alarms="$alarms $w/event_count" ;;
        *) [ -d $w/device/rtc ] && alarms="$alarms $w/event_count" ;;
    esac
done
k "alarm wakeup sources:$(for f in $alarms; do printf ' %s' "$(cat ${f%event_count}name)"; done)"
sum() { local s=0 f; for f in "$@"; do s=$((s + $(cat $f 2>/dev/null || echo 0))); done; echo $s; }

ok=$(cat /sys/power/suspend_stats/success) alarm=$(sum $alarms) held= i=0
while sleep 1; do
    i=$((i + 1))
    if [ $((i % 5)) = 0 ]; then
        if pgrep -f 'pcvktest.sh|pcgfxbench.sh' > /dev/null; then
            [ -n "$held" ] || { echo pcwake > /sys/power/wake_lock; held=1; k "tests running, wakelock held"; }
        elif [ -n "$held" ]; then
            echo pcwake > /sys/power/wake_unlock; held=; k "tests done, wakelock released"
        fi
    fi
    now=$(cat /sys/power/suspend_stats/success)
    if [ "$now" = "$ok" ]; then
        alarm=$(sum $alarms)
        continue
    fi
    a=$(sum $alarms)
    k "resumed (suspend $now): $(tr '\n' ' ' < /sys/kernel/wakeup_reasons/last_resume_reason 2>/dev/null)"
    if [ "$a" = "$alarm" ]; then
        k "not an alarm, turning the screen on"
        input keyevent KEYCODE_WAKEUP
    fi
    ok=$now alarm=$a
done
