#!/usr/bin/env bash
# Update the Android OS on an already-flashed android-pc stick (or a stick image) in place,
# keeping installed apps, accounts and all user data (docs/usb-boot-aosp17.md §10).
#
#   tools/update-usb-aosp17.sh [options] NEW.img TARGET
#
# NEW.img: an image made by tools/mkusb-aosp17.sh from the new build (any userdata size,
#          e.g. `tools/mkusb-aosp17.sh --userdata 64 out/aosp17-usb-f9.img`).
# TARGET:  the stick (/dev/sdX, whole disk) or a stick image file.
#
# Run it as yourself, not with sudo: for a stick it asks for the sudo password once, at the
# start, and runs only the commands that read or write /dev/sdX as root (sfdisk, dd, mcopy
# on the stick). The password stays cached while it runs, even through a long write.
#
# Only the OS partitions are rewritten: esp (Limine, kernel, initrd), vbmeta*_a, boot_a,
# init_boot_a, vendor_boot_a and super. userdata (apps, accounts, files), metadata (the
# FBE metadata key), misc, frp and pclog are not touched, and neither is the partition
# table, so the stick must already have the same layout (same partition names, each OS
# partition at least as big as the new one). The script checks that, and that the new
# build can open the old data:
#   - the same AVB key and a rollback index that does not go back (vbmeta),
#   - a security patch level that does not go back (KeyMint refuses a downgrade, and the
#     encrypted userdata would no longer open),
#   - a different build number (ro.build.version.incremental): Android rescans and
#     upgrades the system apps only when the build fingerprint changes. tools/aosp17-build.sh
#     sets a fresh BUILD_NUMBER for each run; --force skips this check.
# Options: --dry-run    check and print the dd commands, write nothing
#          --yes        do not ask before writing to a block device
#          --force      write even if the build number did not change
#          --no-verify  do not read the written partitions back to compare them
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
host=$ROOT/aosp/out/host/linux-x86/bin
dry=0 yes=0 force=0 verify=1 args=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run) dry=1 ;;
        --yes) yes=1 ;;
        --force) force=1 ;;
        --no-verify) verify=0 ;;
        -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
        -*) echo "update-usb-aosp17.sh: unknown option $1 (see --help)" >&2; exit 2 ;;
        *) args+=("$1") ;;
    esac
    shift
done
[[ ${#args[@]} == 2 ]] || { echo "usage: update-usb-aosp17.sh [options] NEW.img TARGET (see --help)" >&2; exit 2; }
new=${args[0]} target=${args[1]}
die() { echo "update-usb-aosp17.sh: $*" >&2; exit 1; }
[[ -f $new ]] || die "no image $new"
for f in "$host/mcopy" "$host/avbtool"; do [[ -x $f ]] || die "missing $f (build hosttools)"; done
command -v sfdisk >/dev/null || die "needs sfdisk"

# OS partitions, in write order (the ESP last: until it is written the stick boots the old
# kernel, which still matches the old vendor_boot modules if the copy is interrupted).
OS_PARTS=(super boot_a init_boot_a vendor_boot_a vbmeta_a vbmeta_system_a
          vbmeta_vendor_dlkm_a vbmeta_system_dlkm_a esp)
KEEP_PARTS=(metadata userdata misc frp)
ESP_UUID=a17c0de0-0017-4c0d-9e00-0000000000e5   # tools/mkusb-aosp17.sh

device=0
if [[ -b $target ]]; then
    device=1
    [[ $(lsblk -dno TYPE "$target") == disk ]] || die "$target is not a whole disk"
    [[ $(lsblk -dno TRAN "$target") == usb ]] || die "$target is not a USB disk"
    if lsblk -no MOUNTPOINTS "$target" | grep -q .; then die "$target has mounted partitions (unmount them)"; fi
    (( EUID != 0 )) || die "run it without sudo: it asks for the password itself, only for the commands on $target"
elif [[ ! -f $target ]]; then
    die "no stick or image $target"
fi

work=$(mktemp -d)
keep=""
trap '[[ -n $keep ]] && kill "$keep" 2>/dev/null; /usr/bin/rm -rf "$work"' EXIT

# R: the prefix for commands on TARGET (sudo for a stick, nothing for an image file).
R=() RS=""
if (( device )); then
    R=(sudo --) RS="sudo -- "
    sudo -v -p "[sudo] password for %p (to read and write $target): " || die "no sudo"
    # Keep the sudo ticket fresh: writing super alone can outlast its 5-minute timeout.
    ( while sleep 60; do sudo -n -v 2>/dev/null || exit; done ) &
    keep=$!
fi

# Layout as "name start_MiB size_MiB uuid" lines (sfdisk reads a GPT from a file or a disk).
layout() {   # layout FILE [PREFIX...]
    "${@:2}" sfdisk --json "$1" | python3 -I -c '
import json, sys
t = json.load(sys.stdin)["partitiontable"]
ss = t.get("sectorsize", 512)
for p in t["partitions"]:
    start, size = p["start"] * ss, p["size"] * ss
    if start % 1048576 or size % 1048576:
        sys.exit("partition %s is not MiB-aligned" % p.get("name"))
    print(p.get("name", "-"), start // 1048576, size // 1048576, p.get("uuid", "-").lower())'
}
layout "$new" > "$work/new.layout" || die "$new has no readable GPT"
layout "$target" "${R[@]}" > "$work/target.layout" || die "$target has no readable GPT"
part() { awk -v n="$2" '$1 == n { print $'"$3"' }' "$work/$1.layout"; }   # part new|target NAME FIELD

# Is TARGET an android-pc stick, and is NEW a stick image?
for side in new target; do
    [[ $(part $side esp 4) == "$ESP_UUID" ]] || die "$side image: no android-pc ESP (partition uuid $ESP_UUID)"
done
for p in "${KEEP_PARTS[@]}"; do
    [[ -n $(part target "$p" 1) ]] || die "$target has no $p partition: not an android-pc stick"
done
bad=""
for p in "${OS_PARTS[@]}"; do
    ns=$(part new "$p" 3) ts=$(part target "$p" 3)
    [[ -n $ns ]] || die "$new has no $p partition"
    if [[ -z $ts ]]; then bad+="  $p: missing on the stick\n"
    elif (( ns > ts )); then bad+="  $p: new ${ns} MiB > stick ${ts} MiB\n"; fi
done
if [[ -n $bad ]]; then
    printf "update-usb-aosp17.sh: the stick's layout does not fit the new build:\n$bad" >&2
    echo "This build needs one full flash (tools/mkusb-aosp17.sh --disk), which erases the data." >&2
    exit 1
fi

# --- can the new build open the old data? -------------------------------------------------
extract() {   # extract new|target PART FILE: copy a partition out of NEW or TARGET
    local img=$new run=(); [[ $1 == target ]] && img=$target run=("${R[@]}")
    "${run[@]}" dd if="$img" bs=1M skip="$(part "$1" "$2" 2)" count="$(part "$1" "$2" 3)" status=none > "$3"
}
avb() {   # avb FILE: "key-sha1 rollback-index" of a vbmeta image
    "$host/avbtool" info_image --image "$1" 2>/dev/null | awk '
        /^Public key \(sha1\):/ { k = $4 } /^Rollback Index:/ { r = $3 } END { print k, r }'
}
extract new vbmeta_a "$work/new.vbmeta"
extract target vbmeta_a "$work/target.vbmeta"
read -r new_key new_ri <<<"$(avb "$work/new.vbmeta")"
read -r old_key old_ri <<<"$(avb "$work/target.vbmeta")"
[[ -n $new_key ]] || die "cannot read the new vbmeta"
if [[ -n $old_key ]]; then
    [[ $new_key == "$old_key" ]] || die "the new build is signed with another AVB key ($new_key, stick: $old_key): KeyMint keys and the encrypted data would not open"
    (( ${new_ri:-0} >= ${old_ri:-0} )) || die "the new vbmeta rollback index $new_ri is lower than the stick's $old_ri"
else
    echo "warning: no readable vbmeta on the stick, AVB key not compared" >&2
fi

# Build info: android/build.txt on the ESP (tools/mkusb-aosp17.sh writes it; sticks made
# before it existed have none, and their build number was the default eng.<user>).
buildinfo() {   # buildinfo new|target: the ESP's android/build.txt, or nothing
    local img=$new run=(); [[ $1 == target ]] && img=$target run=("${R[@]}")
    "${run[@]}" env MTOOLS_SKIP_CHECK=1 "$host/mcopy" -n -i "$img@@$(( $(part "$1" esp 2) * 1048576 ))" \
        ::/android/build.txt - 2>/dev/null || true
}
prop() { sed -n "s/^$2=//p" <<<"$1" | head -1; }
new_info=$(buildinfo new) old_info=$(buildinfo target)
[[ -n $new_info ]] || die "$new has no android/build.txt on its ESP: make it with the current tools/mkusb-aosp17.sh"
new_inc=$(prop "$new_info" ro.build.version.incremental)
new_spl=$(prop "$new_info" ro.build.version.security_patch)
if [[ -n $old_info ]]; then
    old_inc=$(prop "$old_info" ro.build.version.incremental)
    old_spl=$(prop "$old_info" ro.build.version.security_patch)
else
    old_inc="(unknown, older stick: probably eng.<user>)" old_spl=""
fi
echo "Stick: $target"
echo "  build now: $old_inc ${old_spl:+(patch level $old_spl)}"
echo "  new build: $new_inc (patch level $new_spl)"
echo "  $(prop "$new_info" ro.vendor.build.fingerprint)"
if [[ -n $old_spl && $new_spl < $old_spl ]]; then
    die "the new security patch level $new_spl is older than the stick's $old_spl: KeyMint would refuse the keys of the encrypted data"
fi
if (( ! force )); then
    if [[ $new_inc == "${old_inc:-}" ]] || [[ -z $old_info && $new_inc =~ ^eng\.[^.]*$ ]]; then
        die "the build number did not change ($new_inc), so Android would not rescan its system apps.
Build with tools/aosp17-build.sh (it sets a fresh BUILD_NUMBER) or pass --force."
    fi
fi

# --- write ---------------------------------------------------------------------------------
# Each partition: the new content, then zeros up to the end of the stick's partition if the
# new one is smaller (no stale AVB footer or super metadata left behind).
cmds=()
for p in "${OS_PARTS[@]}"; do
    ns=$(part new "$p" 2) nz=$(part new "$p" 3) ts=$(part target "$p" 2) tz=$(part target "$p" 3)
    oflag=""; (( device )) && oflag=" oflag=direct"
    cmds+=("${RS}dd if=$new of=$target bs=1M skip=$ns seek=$ts count=$nz conv=notrunc,fsync$oflag status=none")
    (( tz > nz )) && cmds+=("${RS}dd if=/dev/zero of=$target bs=1M seek=$(( ts + nz )) count=$(( tz - nz )) conv=notrunc,fsync$oflag status=none")
done
echo
echo "Partitions written: ${OS_PARTS[*]}"
echo "Kept as they are:   ${KEEP_PARTS[*]} pclog (and the partition table)"
if (( dry )); then
    echo; echo "Commands (dry run, nothing written):"
    for c in "${cmds[@]}"; do echo "  $c"; done
    exit 0
fi
if (( device && ! yes )); then
    lsblk -o NAME,SIZE,MODEL,PARTLABEL "$target"
    read -r -p "Write the new Android to $target? Type yes: " ok
    [[ $ok == yes ]] || die "not written"
fi
for c in "${cmds[@]}"; do echo "+ $c"; eval "$c"; done
sync
if (( verify )); then
    for p in "${OS_PARTS[@]}"; do
        ns=$(part new "$p" 2) nz=$(part new "$p" 3) ts=$(part target "$p" 2)
        cmp -n $(( nz * 1048576 )) <(dd if="$new" bs=1M skip="$ns" count="$nz" status=none) \
            <("${R[@]}" dd if="$target" bs=1M skip="$ts" count="$nz" iflag=$( (( device )) && echo direct || echo fullblock) status=none) \
            >/dev/null || die "$p reads back different from the new image: write it again"
    done
    echo "Read back and compared: OK"
fi
echo "Done. Android starts the new build on the next boot; the first boot takes longer (app upgrade and optimisation)."
