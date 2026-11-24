#!/bin/bash
# Flash the aosp_gts7l Android images (tools/gts7l-image.sh) from the running bring-up shell
# (docs/gts7l.md §12, §15) over the USB NCM link, check every write, and reboot into Android.
#
#   tools/gts7l-android-flash.sh [--yes] [--no-reboot] [IMAGES_DIR]
#
# IMAGES_DIR defaults to out/gts7l/images/ of the main checkout (boot.img, recovery.img,
# super.img, SHA256SUMS). The tablet must run the bring-up image (telnet on 172.16.42.1),
# from boot or from recovery.
#
# What it writes (partition names and sizes are read from sysfs and checked first):
#   super     sda29  the sparse super.img expanded on the host while it streams, gzip over
#                    nc and written with dd; SHA-256 of the whole partition compared
#   boot      sda23  zeroed, written, SHA-256 of the image's range compared
#   recovery  sda24  the same; the bring-up image, so an Android init failure lands in this shell
#   userdata  sda37  first 16 MiB zeroed: Android formats it at first boot (fstab.gts7l
#                    formattable). THIS WIPES /data: Samsung's encrypted userdata is unusable
#                    for us, and it holds nothing since the unlock wipe.
#   metadata  sda28  zeroed (32 MiB): Samsung's encryption metadata of that userdata; formatted
#                    at first boot the same way
#   dtbo      sda22  first 4 KiB zeroed (ABL then applies no stock overlay)
#   param     sda10  byte 0 cleared (Samsung's boot flag, as in docs/gts7l.md §12)
# Then `reboot -f` (not sysrq: the kernel's reboot notifiers write the normal reboot reason).
# Never written: efs, sec_efs, persist, modem*, apnhlos, dsp, firmware and bootloader
# partitions, vbmeta (ours since S5; only read and checked).
set -euo pipefail
WT="$(cd "$(dirname "$0")/.." && pwd)"
ROOT=$WT
[[ -d $ROOT/aosp ]] || ROOT="$(cd "$(git -C "$WT" rev-parse --path-format=absolute --git-common-dir)/.." && pwd)"
export PATH=$ROOT/aosp/out/host/linux-x86/bin:$PATH
YES= REBOOT=1
while [[ ${1:-} == --* ]]; do
    case $1 in --yes) YES=1 ;; --no-reboot) REBOOT= ;; *) echo "usage: $0 [--yes] [--no-reboot] [IMAGES_DIR]" >&2; exit 2 ;; esac
    shift
done
DIR=${1:-$ROOT/out/gts7l/images}
IP=172.16.42.1
die() { echo "gts7l-android-flash: $*" >&2; exit 1; }
log() { echo "gts7l-android-flash: $*"; }

# Partition, block device, size in bytes (docs/gts7l-inventory.md, gts7l-backup/*/partitions.txt).
declare -A DEV=([boot]=sda23 [recovery]=sda24 [super]=sda29 [userdata]=sda37 [metadata]=sda28
                [dtbo]=sda22 [param]=sda10 [vbmeta]=sdd25)
declare -A SIZE=([boot]=71303168 [recovery]=86888448 [super]=10171187200 [userdata]=114864140288
                 [metadata]=33554432 [dtbo]=10485760 [param]=10485760 [vbmeta]=65536)
WRITABLE=" boot recovery super userdata metadata dtbo param "

# One shell command over telnet; prints its output (busybox telnetd, no options negotiated).
# Same as tools/gts7l-netflash.sh.
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

# Sends stdin to the tablet's port $1.
send() {
    python3 -I -c "
import socket, sys
s = socket.create_connection(('$IP', int(sys.argv[1])))
while True:
    b = sys.stdin.buffer.read(1 << 20)
    if not b: break
    s.sendall(b)
s.shutdown(socket.SHUT_WR); s.close()" "$1"
}

# Runs $1 in the background on the tablet, then waits up to $2 s for it; prints its output.
bg() {
    local id=$RANDOM out i
    tsh "rm -f /tmp/bg.$id.*; ( ( { $1; } > /tmp/bg.$id.out 2>&1; echo \$? > /tmp/bg.$id.rc ) </dev/null >/dev/null 2>&1 & )" >/dev/null
    for (( i = 0; i < $2; i += 5 )); do
        sleep 5
        out=$(tsh "[ -f /tmp/bg.$id.rc ] && { cat /tmp/bg.$id.out; echo RC=\$(cat /tmp/bg.$id.rc); }")
        [[ $out == *RC=* ]] && { echo "$out"; return; }
    done
    echo "RC=timeout"
}

dev() { echo "/dev/${DEV[$1]}"; }
zeros_sha() { head -c "$1" /dev/zero | sha256sum | cut -d' ' -f1; }

# --- Host side: the images.
for f in boot.img recovery.img super.img SHA256SUMS; do [[ -f $DIR/$f ]] || die "$DIR/$f missing (tools/gts7l-image.sh)"; done
log "checking $DIR/SHA256SUMS"
(cd "$DIR" && grep -E ' (boot|recovery|super)\.img$' SHA256SUMS | sha256sum -c --quiet) || die "image checksums do not match"
for f in boot recovery; do
    [[ $(head -c 8 "$DIR/$f.img") == ANDROID! ]] || die "$f.img is not a boot image"
    (( $(stat -c %s "$DIR/$f.img") <= SIZE[$f] )) || die "$f.img is larger than $f"
done
[[ $(od -An -tx4 -N4 "$DIR/super.img" | tr -d ' ') == ed26ff3a ]] || die "super.img is not sparse"
BUILD=$(cat "$DIR/BUILD" 2>/dev/null || echo unknown)

# --- Tablet side: the bring-up shell and the partition table.
ping -c1 -W2 "$IP" >/dev/null 2>&1 || die "no answer on $IP: is the tablet in the bring-up shell (gts7l-bringup connection up)?"
kver=$(tsh 'cat /proc/version')
[[ $kver == *-gts7l* ]] || die "$IP does not run our gts7l kernel: $kver"
for p in "${!DEV[@]}"; do
    d=${DEV[$p]}
    got=$(tsh "echo \$(cat /sys/class/block/$d/size) \$(sed -n 's/^PARTNAME=//p' /sys/class/block/$d/uevent)")
    [[ $got == "$((SIZE[$p] / 512)) $p" ]] ||
        die "/dev/$d on the tablet is '$got', expected '$((SIZE[$p] / 512)) $p': partition table differs, nothing written"
done
log "partitions checked: $(for p in boot recovery super userdata metadata dtbo param vbmeta; do printf '%s=%s ' $p ${DEV[$p]}; done)"
vb=$(tsh "dd if=$(dev vbmeta) bs=4 skip=30 count=1 2>/dev/null | od -An -tx1 | tr -d ' '; dd if=$(dev vbmeta) bs=4 count=1 2>/dev/null")
[[ $vb == $'00000003\nAVB0' ]] && log "vbmeta: ours (AVB0, flags 3)" ||
    log "WARNING: vbmeta does not look like ours (flags 3): '$vb'; Android may not boot (docs/gts7l.md §11)"

guard() { [[ $WRITABLE == *" $1 "* ]] || die "refusing to write $1"; }

if [[ -z $YES ]]; then
    echo "About to write build $BUILD to the tablet: super, boot, recovery; and to WIPE userdata and"
    echo "metadata (Android formats them at first boot), clear the dtbo header and param byte 0."
    read -r -p "Type 'flash' to go on: " a; [[ $a == flash ]] || die "aborted"
fi

# --- super: made raw on the host while it streams (simg2img needs a seekable output, so the
# sparse format is expanded here, ~10 GB never touch the disk), gzip over nc, dd on the tablet.
guard super
# Writes the raw image of sparse $1 to stdout, "<bytes> <sha256>" of it to $2.
unsparse() {
    python3 -I -c '
import hashlib, struct, sys
f, out, h, n = open(sys.argv[1], "rb"), sys.stdout.buffer, hashlib.sha256(), 0
magic, _, _, fhs, chs, bs, _, chunks, _ = struct.unpack("<IHHHHIIII", f.read(28))
assert magic == 0xed26ff3a
f.read(fhs - 28)
def put(b):
    global n
    out.write(b); h.update(b); n += len(b)
for _ in range(chunks):
    typ, _, blocks, total = struct.unpack("<HHII", f.read(12)); f.read(chs - 12)
    size = blocks * bs
    if typ == 0xcac1:
        while size:
            b = f.read(min(size, 1 << 22)); put(b); size -= len(b)
    elif typ in (0xcac2, 0xcac3):
        fill = f.read(4) if typ == 0xcac2 else bytes(4)
        b = fill * ((1 << 20) // 4)
        while size:
            put(b[:min(size, len(b))]); size -= min(size, len(b))
    elif typ == 0xcac4:
        f.read(4)
    else:
        sys.exit("unknown chunk type %x" % typ)
out.flush()
open(sys.argv[2], "w").write("%d %s\n" % (n, h.hexdigest()))
' "$1" "$2"
}
sums=$(mktemp); trap '/usr/bin/rm -f "$sums"' EXIT
log "super: streaming the raw image (gzip -1)"
tsh "rm -f /tmp/super.*; ( ( ( set -o pipefail; nc -l -p 9001 </dev/null | gunzip -c | dd of=$(dev super) bs=1M 2>/tmp/super.dd ); echo \$? > /tmp/super.rc ) </dev/null >/dev/null 2>&1 & ); sleep 1" >/dev/null
t0=$SECONDS
unsparse "$DIR/super.img" "$sums" | gzip -1 -c | send 9001
read -r N H < "$sums"
(( N == SIZE[super] )) || die "raw super is $N bytes, the partition ${SIZE[super]}"
rc=
for (( i = 0; i < 600; i += 5 )); do
    rc=$(tsh 'cat /tmp/super.rc 2>/dev/null')
    [[ -n $rc ]] && break; sleep 5
done
[[ $rc == 0 ]] || die "super: write failed (rc '$rc'): $(tsh 'cat /tmp/super.dd')"
log "super: $N bytes written in $((SECONDS - t0)) s, sha256 $H; reading back"
out=$(bg "sync; echo 3 > /proc/sys/vm/drop_caches; head -c $N $(dev super) | sha256sum" 900)
[[ $out == *"$H"*RC=0* ]] || die "super: READBACK MISMATCH: $out"
log "super: readback OK"

# --- boot and recovery: pushed to /tmp, checked, the partition zeroed, written, read back.
flash_img() {
    local p=$1 img=$DIR/$1.img n h d out
    guard "$p"; d=$(dev "$p")
    n=$(stat -c %s "$img"); h=$(sha256sum "$img" | cut -d' ' -f1)
    tsh "rm -f /tmp/$p.img; (nc -l -p 9000 > /tmp/$p.img </dev/null &); sleep 1" >/dev/null
    send 9000 < "$img"
    out=$(tsh "sleep 2; [ \"\$(sha256sum /tmp/$p.img | cut -d' ' -f1)\" = $h ] || { echo PUSH-MISMATCH; exit; }
dd if=/dev/zero of=$d bs=4096 count=$((SIZE[$p] / 4096)) 2>/dev/null; dd if=/tmp/$p.img of=$d bs=1M conv=fsync 2>/dev/null; sync
echo 3 > /proc/sys/vm/drop_caches
[ \"\$(head -c $n $d | sha256sum | cut -d' ' -f1)\" = $h ] && echo READBACK-OK || echo READBACK-MISMATCH; rm -f /tmp/$p.img" 120)
    [[ $out == *READBACK-OK* ]] || die "$p: $out"
    log "$p: $n bytes, readback OK"
}
flash_img boot
flash_img recovery

# --- Zeroed ranges: userdata and metadata headers, dtbo header, param byte 0.
zero() {
    local p=$1 n=$2 d out
    guard "$p"; d=$(dev "$p")
    out=$(tsh "dd if=/dev/zero of=$d bs=$((n < 4096 ? n : 4096)) count=$((n < 4096 ? 1 : n / 4096)) conv=fsync 2>/dev/null; sync
echo 3 > /proc/sys/vm/drop_caches; head -c $n $d | sha256sum" 60)
    [[ $out == *"$(zeros_sha "$n")"* ]] || die "$p: first $n bytes not zero after the write: $out"
    log "$p: first $n bytes zeroed"
}
zero userdata $((16 << 20))
zero metadata "${SIZE[metadata]}"
zero dtbo 4096
zero param 1

log "build $BUILD written"
[[ -n $REBOOT ]] || exit 0
tsh 'sync; reboot -f' 4 >/dev/null 2>&1 || true
for _ in $(seq 30); do ping -c1 -W1 "$IP" >/dev/null 2>&1 || break; done
log "rebooting into Android; the first boot formats userdata. Waiting up to 10 min for adb"
for (( i = 0; i < 600; i += 5 )); do
    if lsusb -d 18d1:4ee7 >/dev/null 2>&1; then
        log "Android's gadget is up (18d1:4ee7): adb devices: $(adb devices | sed -n '2,$p' | tr '\n' ' ')"
        exit 0
    fi
    if lsusb -d 1d6b:0104 >/dev/null 2>&1 && ping -c1 -W1 "$IP" >/dev/null 2>&1; then
        log "the bring-up shell is back on $IP: Android failed and init rebooted into recovery;"
        log "previous boot's logs: telnet $IP, ls /sys/fs/pstore (console-ramoops-0, pmsg-ramoops-0)"
        exit 1
    fi
    sleep 5
done
die "neither Android's adb gadget nor the bring-up shell after 10 min"
