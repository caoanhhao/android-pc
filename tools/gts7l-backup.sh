#!/bin/bash
# Back up the SM-T875's partitions to the host (step S5 of docs/gts7l.md), from a recovery with
# a root adb shell. Every partition except userdata, super and cache (super and the firmware
# images are in the stock Odin package, userdata is the user's data); each one is read with dd,
# and its SHA-256 on the tablet and on the host must match.
#
#   tools/gts7l-backup.sh [DIR]     default DIR: gts7l-backup/<date> at the main checkout root
set -euo pipefail
SERIAL=${SERIAL:?set SERIAL to the adb serial of the tablet}
WT="$(cd "$(dirname "$0")/.." && pwd)"
ROOT=$WT
[[ -d $ROOT/aosp ]] || ROOT="$(cd "$(git -C "$WT" rev-parse --path-format=absolute --git-common-dir)/.." && pwd)"
DIR=${1:-$ROOT/gts7l-backup/$(date +%Y%m%d-%H%M)}
SKIP=" userdata super cache "
# </dev/null: adb would otherwise eat the partition list the loop reads.
a() { adb -s "$SERIAL" "$@" </dev/null; }

[[ $(a get-state 2>/dev/null) == recovery ]] || { echo "gts7l-backup: $SERIAL is not in recovery with adb" >&2; exit 1; }
[[ $(a shell id -u | tr -d '\r') == 0 ]] || { echo "gts7l-backup: adb shell is not root" >&2; exit 1; }
model=$(a shell getprop ro.product.model | tr -d '\r')
[[ $model == SM-T875 ]] || { echo "gts7l-backup: model is '$model', not SM-T875" >&2; exit 1; }

mkdir -p "$DIR"
a shell 'cd /dev/block/by-name && for n in *; do echo "$n $(readlink "$n")"; done' | tr -d '\r' > "$DIR/by-name.txt"
a shell cat /proc/partitions | tr -d '\r' > "$DIR/partitions.txt"

fail=0
while read -r name dev; do
    [[ $name =~ ^sd[a-z]$ || $SKIP == *" $name "* ]] && continue
    a exec-out "dd if=/dev/block/by-name/$name bs=1M 2>/dev/null" > "$DIR/$name.img"
    want=$(a shell "sha256sum /dev/block/by-name/$name" | tr -d '\r' | cut -d' ' -f1)
    got=$(sha256sum "$DIR/$name.img" | cut -d' ' -f1)
    size=$(stat -c %s "$DIR/$name.img")
    if [[ $want == "$got" ]]; then
        printf '%-16s %-6s %12d OK\n' "$name" "${dev##*/}" "$size"
    else
        printf '%-16s %-6s %12d MISMATCH\n' "$name" "${dev##*/}" "$size"; fail=1
    fi
done < "$DIR/by-name.txt"
(cd "$DIR" && sha256sum *.img > SHA256SUMS)
chmod -R a-w "$DIR"
[[ $fail == 0 ]] && echo "gts7l-backup: all partitions read back and compared: OK ($DIR)" ||
    { echo "gts7l-backup: some partitions differ, see above" >&2; exit 1; }
