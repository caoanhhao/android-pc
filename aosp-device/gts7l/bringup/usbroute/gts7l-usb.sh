#!/vendor/bin/sh
# USB reachability of aosp_gts7l without the screen (docs/gts7l.md §15), started by
# init.gts7l.usb.rc at "on init".
#
#   gts7l-usb.sh console    a root shell on the gadget's ACM tty (/dev/ttyACM0 on the host)
#   gts7l-usb.sh            the rest:
#  1. route the MAX77705 USB data lines to the SoC (usbroute), retrying while the PMIC's I2C
#     client probes;
#  2. if adbd has not bound the gadget after 90 s (no sys.usb.ffs.ready), bind it with the NCM
#     function alone, so adbd's TCP port (service.adb.tcp.port) is reachable on the USB link;
#  3. keep 172.16.42.1/24 on the NCM interface (the host's "gts7l-bringup" connection has
#     172.16.42.2), as the bring-up shell does: `adb connect 172.16.42.1:5555`. Under Android,
#     netd's policy routing never looks up the main table, so the interface also goes into
#     netd's local network, whose table routes the replies;
#  4. stop once the USB HAL has taken the gadget over (sys.usb.configfs=2): it removes the
#     bring-up NCM function, and tethering configures its own interface.
G=/config/usb_gadget/g1
log() { echo "gts7l-usb: $*" > /dev/kmsg; }

if [ "$1" = console ]; then
    stty -F /dev/ttyGS0 sane -echo 2>/dev/null
    exec /system/bin/sh < /dev/ttyGS0 > /dev/ttyGS0 2>&1
fi

i=0
until /vendor/bin/usbroute; do
    i=$((i + 1))
    [ $i -ge 30 ] && { log "usbroute failed 30 times"; break; }
    sleep 1
done

i=0
while [ -z "$(cat $G/UDC 2>/dev/null)" ]; do
    [ "$(getprop sys.usb.configfs)" = 2 ] && break
    i=$((i + 1))
    if [ $i -ge 90 ]; then
        log "no adb gadget after 90 s (sys.usb.state=$(getprop sys.usb.state)), binding NCM alone"
        echo "$(getprop sys.usb.controller)" > $G/UDC
        break
    fi
    sleep 1
done
log "gadget on $(cat $G/UDC 2>/dev/null) ($(ls $G/configs/b.1 | grep '^f' | tr '\n' ' '))"

while [ "$(getprop sys.usb.configfs)" != 2 ]; do
    if [ -e /sys/class/net/usb0 ] && ! ifconfig usb0 | grep -q 172.16.42.1; then
        ifconfig usb0 172.16.42.1 netmask 255.255.255.0 up && log "usb0 172.16.42.1"
        if [ -n "$(getprop init.svc.netd)" ]; then
            ndc network interface add local usb0 >/dev/null 2>&1
            ndc network route add local usb0 172.16.42.0/24 >/dev/null 2>&1
        fi
    fi
    sleep 5
done
if [ -e /sys/class/net/usb0 ]; then
    ndc network interface remove local usb0 >/dev/null 2>&1
    ifconfig usb0 0.0.0.0 2>/dev/null
fi
log "the USB HAL has the gadget (functions: $(ls $G/configs/b.1 | grep '^f' | tr '\n' ' '))"
