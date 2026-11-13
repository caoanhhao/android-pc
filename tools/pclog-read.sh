#!/usr/bin/env bash
# Copy the boot logs that pclog.sh (aosp-device/pc) wrote to the "pclog" partition of a
# tools/mkusb-aosp17.sh image or stick, without mounting it, and joins the dmesg/logcat
# pieces of each boot into boot-N/dmesg.txt and boot-N/logcat.txt.
#   tools/pclog-read.sh IMAGE-or-/dev/sdX [DEST]     default DEST: out/pclog
# On a stick plugged into the build host, udisks usually mounts the partition (label
# PCLOG) under /run/media/$USER/PCLOG: copying from there works as well.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
src=$1 dest=${2:-$ROOT/out/pclog}
start=$(sfdisk --json "$src" | python3 -I -c "
import json,sys
for p in json.load(sys.stdin)['partitiontable']['partitions']:
    if p['name'] == 'pclog': print(p['start'] * 512)")
[[ -n $start ]] || { echo "pclog-read.sh: no pclog partition on $src" >&2; exit 1; }
mkdir -p "$dest"
MTOOLS_SKIP_CHECK=1 "$ROOT/aosp/out/host/linux-x86/bin/mcopy" -s -n -o -i "$src@@$start" '::/*' "$dest/"
# pclog.sh writes dmesg and logcat in pieces (NAME.aaa, NAME.aab, ...; pruned.txt lists the
# ones it dropped): join them into NAME.txt.
for b in "$dest"/boot-*/; do
    for n in dmesg logcat; do
        pieces=("$b$n".???)
        [[ -e ${pieces[0]} ]] && cat "${pieces[@]}" > "$b$n.txt"
    done
done
ls -l "$dest"
