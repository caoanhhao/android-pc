#!/bin/bash
# Write a boot image to the tablet's boot partition from the running bring-up initramfs
# (docs/gts7l.md, S6) over the USB NCM link, check it, and reboot into it.
#
#   tools/gts7l-netflash.sh [IMAGE]     default: out/gts7l/bringup/boot.img of the main checkout
#
# The bring-up shell listens on 172.16.42.1 (telnet 23); boot is sda23 (71303168 bytes).
set -euo pipefail
WT="$(cd "$(dirname "$0")/.." && pwd)"
ROOT=$WT
[[ -d $ROOT/aosp ]] || ROOT="$(cd "$(git -C "$WT" rev-parse --path-format=absolute --git-common-dir)/.." && pwd)"
IMG=${1:-$ROOT/out/gts7l/bringup/boot.img}
IP=172.16.42.1 BOOT=/dev/sda23 BOOT_SIZE=71303168
N=$(stat -c %s "$IMG"); H=$(sha256sum "$IMG" | cut -d' ' -f1)
(( N <= BOOT_SIZE )) || { echo "gts7l-netflash: $IMG is larger than boot" >&2; exit 1; }
[[ $(head -c 8 "$IMG") == ANDROID! ]] || { echo "gts7l-netflash: $IMG is not a boot image" >&2; exit 1; }

# One shell command over telnet; prints its output (busybox telnetd, no options negotiated).
tsh() {
    python3 -I - "$IP" "$1" "${2:-30}" <<'PY'
import socket, sys, time
ip, cmd, tmo = sys.argv[1], sys.argv[2], float(sys.argv[3])
s = socket.create_connection((ip, 23), timeout=5)
def clean(d):
    out, i = bytearray(), 0
    while i < len(d):
        if d[i] == 255 and i + 2 < len(d):
            if d[i + 1] in (251, 252, 253, 254):
                s.send(bytes([255, 254 if d[i + 1] in (251, 252) else 252, d[i + 2]])); i += 3; continue
            i += 2; continue
        out.append(d[i]); i += 1
    return bytes(out)
time.sleep(0.5)
try: clean(s.recv(65536))
except socket.timeout: pass
s.send((cmd + '; echo __END__\n').encode()); s.settimeout(1)
buf, end = b'', time.time() + tmo
while time.time() < end and buf.count(b'__END__') < 2:
    try:
        d = s.recv(65536)
        if not d: break
        buf += clean(d)
    except socket.timeout: pass
t = buf.decode(errors='replace').replace('\r', '')
print(t.split('__END__')[1].strip() if t.count('__END__') >= 2 else t.strip())
PY
}

[[ $(tsh "cat /sys/class/block/${BOOT#/dev/}/size") == $((BOOT_SIZE / 512)) ]] ||
    { echo "gts7l-netflash: $BOOT on the tablet is not the boot partition" >&2; exit 1; }
tsh 'rm -f /tmp/boot.img; (nc -l -p 9000 > /tmp/boot.img </dev/null &); sleep 1' >/dev/null
python3 -I -c "
import socket, sys
s = socket.create_connection(('$IP', 9000)); s.sendall(open(sys.argv[1], 'rb').read())
s.shutdown(socket.SHUT_WR); s.close()" "$IMG"
out=$(tsh "sleep 2; [ \"\$(sha256sum /tmp/boot.img | cut -d' ' -f1)\" = $H ] || { echo PUSH-MISMATCH; exit; }
dd if=/dev/zero of=$BOOT bs=1M count=68 2>/dev/null; dd if=/tmp/boot.img of=$BOOT bs=1M conv=fsync 2>/dev/null; sync
[ \"\$(head -c $N $BOOT | sha256sum | cut -d' ' -f1)\" = $H ] && echo READBACK-OK || echo READBACK-MISMATCH" 60)
echo "gts7l-netflash: $out"
[[ $out == *READBACK-OK* ]] || exit 1
[[ ${NO_REBOOT:-} ]] && exit 0
tsh 'sync; reboot -f' 4 >/dev/null 2>&1 || true
for _ in $(seq 30); do ping -c1 -W1 "$IP" >/dev/null 2>&1 || break; done
for _ in $(seq 90); do ping -c1 -W1 "$IP" >/dev/null 2>&1 && { echo "gts7l-netflash: tablet is back"; exit 0; }; done
echo "gts7l-netflash: tablet did not come back on $IP" >&2; exit 1
