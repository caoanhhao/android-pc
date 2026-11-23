#!/bin/bash
# Device firmware for gts7l (SM-T875), docs/gts7l.md §13: lays out the co-processor firmware of
# this tablet under the names the mainline kernel requests. Every file comes from the tablet's
# own stock data and is Samsung/Qualcomm/Cirrus-signed: the output is never committed and never
# leaves this host (out/ is gitignored).
#
#   tools/gts7l-firmware.sh
#
# Inputs (read-only; environment overrides in brackets):
#   gts7l-backup/<latest>/      [GTS7L_BACKUP] the S5 partition backup: apnhlos.img (vfat,
#                               DSP/Venus/GPU-zap/IPA), sec_efs.img (amp calibration); modem.img
#                               and dsp.img are only listed (no kernel consumer yet)
#   gts7l-stock/ap/super/vendor.img   [GTS7L_STOCK_VENDOR] stock vendor (ext4) from the Odin
#                               package: GPU microcode, QCA6390 Wi-Fi/BT, CS35L41 protection
#   out/gts7l/sm8250-samsung-gts7l.dtb [GTS7L_DTB] the DTB of tools/gts7l-kernel.sh: its
#                               firmware-name properties decide the remoteproc/Venus/zap names
#
# Output in out/gts7l/firmware/ of the main checkout [GTS7L_FW_OUT]:
#   tree/          every file under the requested name, real copies (bring-up initramfs:
#                  tools/gts7l-bringup.sh --firmware puts it at /lib/firmware)
#   gts7l_fw.img   ext4 for the gts7l_fw logical partition in super (tools/gts7l-image.sh),
#                  mounted at /mnt/vendor/firmware: files that exist only in the stock vendor
#                  or are generated here are real files, apnhlos files are symlinks into
#                  /mnt/vendor/apnhlos/image, so Android still reads those from the partition
#   MANIFEST.tsv   path, size, sha256, source, kernel driver, form in gts7l_fw.img
#   SHA256SUMS     of tree/; SOURCES: the input images and their sha256
#   partitions/    file lists of modem and dsp, which stay on their partitions
set -euo pipefail
WT="$(cd "$(dirname "$0")/.." && pwd)"
ROOT=$WT
[[ -d $ROOT/aosp ]] || ROOT="$(cd "$(git -C "$WT" rev-parse --path-format=absolute --git-common-dir)/.." && pwd)"
OUT=${GTS7L_FW_OUT:-$ROOT/out/gts7l/firmware}
BACKUP=${GTS7L_BACKUP:-$(ls -d "$ROOT"/gts7l-backup/*/ 2>/dev/null | sort | tail -1)}
BACKUP=${BACKUP%/}
VENDOR=${GTS7L_STOCK_VENDOR:-}
if [[ -z $VENDOR ]]; then
    for v in "$ROOT/gts7l-stock/ap/super/vendor.img" "$(dirname "$ROOT")/android-pc-gts7l/gts7l-stock/ap/super/vendor.img"; do
        [[ -f $v ]] && { VENDOR=$v; break; }
    done
fi
DTB=${GTS7L_DTB:-$ROOT/out/gts7l/sm8250-samsung-gts7l.dtb}
# I2C bus of the four CS35L41 amps (qupv3_se11, i2c-11 in the S6 survey): the calibration file
# name is cirrus/cs35l41-cal-<dev_name>.bin.
AMP_BUS=${GTS7L_AMP_BUS:-11}
# Where Android mounts apnhlos (fstab.gts7l); the symlinks in gts7l_fw.img point there.
APNHLOS_MNT=/mnt/vendor/apnhlos/image

die() { echo "gts7l-firmware: $*" >&2; exit 1; }
for t in mcopy mdir debugfs mke2fs e2fsck dtc sha256sum; do
    command -v $t >/dev/null || die "$t not found"
done
[[ -n $BACKUP && -f $BACKUP/apnhlos.img ]] || die "no apnhlos.img in '$BACKUP' (set GTS7L_BACKUP)"
[[ -n $VENDOR && -f $VENDOR ]] || die "stock vendor.img not found (set GTS7L_STOCK_VENDOR)"
[[ -f $DTB ]] || die "$DTB missing: run tools/gts7l-kernel.sh first (or set GTS7L_DTB)"

# The backup is checksummed (tools/gts7l-backup.sh): use it only if it still matches.
(cd "$BACKUP" && grep -E ' (apnhlos|sec_efs|modem|dsp)\.img$' SHA256SUMS | sha256sum -c --quiet) ||
    die "$BACKUP does not match its SHA256SUMS"

work=$OUT/.work
rm -rf "$OUT/tree" "$OUT/partitions" "$work"
mkdir -p "$OUT/tree" "$OUT/partitions" "$work/img"
trap 'rm -rf "$work"' EXIT

mcopy -s -n -i "$BACKUP/apnhlos.img" ::/image "$work/apnhlos"
debugfs -R "rdump /firmware $work" "$VENDOR" 2>/dev/null
V=$work/firmware
A=$work/apnhlos
[[ -f $V/a650_sqe.fw && -d $V/qca6390 ]] || die "$VENDOR has no /firmware with the GPU and QCA6390 files"
[[ -f $A/adsp.mdt ]] || die "apnhlos.img has no image/adsp.mdt"

manifest=$work/manifest
: > "$manifest"
# put DEST KIND SRC DRIVER: KIND is apnhlos (SRC in apnhlos image/, a symlink in gts7l_fw.img),
# vendor (SRC under the stock /vendor/firmware) or file (SRC a file generated here); the last
# two are real files in gts7l_fw.img.
put() {
    local dest=$1 kind=$2 src=$3 driver=$4 from form
    case $kind in
        apnhlos) from=$A/$src; form=link ;;
        vendor) from=$V/$src; form=file ;;
        file) from=$src; form=file ;;
    esac
    [[ -f $from ]] || die "missing $kind file $src"
    install -D -m 0644 "$from" "$OUT/tree/$dest"
    mkdir -p "$(dirname "$work/img/$dest")"
    if [[ $form == link ]]; then
        ln -s "$APNHLOS_MNT/$src" "$work/img/$dest"
    else
        install -m 0644 "$from" "$work/img/$dest"
    fi
    case $kind in
        apnhlos) src="apnhlos:image/$src" ;;
        vendor) src="vendor:/firmware/$src" ;;
        file) src=$5 ;;
    esac
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$dest" "$(stat -c %s "$from")" \
        "$(sha256sum "$from" | cut -d' ' -f1)" "$src" "$driver" "$form" >> "$manifest"
}
# Split PIL image: NAME (the .mdt under the requested name) and its .bNN segments, which the
# mdt loader requests next to it.
put_pil() {
    local dest=$1 stem=$2 driver=$3 seg
    put "$dest" apnhlos "$stem.mdt" "$driver"
    for seg in "$A/$stem".b[0-9][0-9]; do
        put "$(dirname "$dest")/${seg##*/}" apnhlos "${seg##*/}" "$driver"
    done
}
shopt -s nullglob

# 1. DT-named firmware (remoteprocs, Venus, GPU zap), all from apnhlos.
mapfile -t dtnames < <(dtc -I dtb -O dts -q "$DTB" | sed -n 's/.*firmware-name = "\([^"]*\)".*/\1/p' | sort -u)
(( ${#dtnames[@]} )) || die "no firmware-name in $DTB"
for name in "${dtnames[@]}"; do
    stem=${name##*/}; stem=${stem%.*}
    case $stem in
        adsp|cdsp|slpi) driver="qcom_q6v5_pas (remoteproc $stem)" ;;
        venus) driver="qcom_iris (Venus video codec)" ;;
        a650_zap) driver="msm (Adreno 650 zap shader)" ;;
        *) die "firmware-name $name: no known source for $stem" ;;
    esac
    put_pil "$name" "$stem" "$driver"
    # Protection-domain maps of the DSP, read by pd-mapper (userspace) next to the firmware.
    for j in "$A/$stem"*.jsn; do
        put "$(dirname "$name")/${j##*/}" apnhlos "${j##*/}" "pd-mapper (userspace, $stem protection domains)"
    done
done

# 2. IPA (LTE data path; the ipa node is not enabled in the DTS yet). The apnhlos copies: TZ
# rejects the differently signed ones of the stock vendor (gts7xl-mainline docs/suspend-drain.md).
put_pil ipa_fws.mdt ipa_fws "ipa (GSI firmware, default name; node not enabled yet)"
put_pil ipa_uc.mdt ipa_uc "ipa microcontroller (no mainline loader yet)"

# 3. Files only the stock vendor has. Driver names: adreno a6xx_catalog.c (qcom/ prefix first),
# ath11k hw.h (QCA6390/hw2.0: amss.bin, m3.bin, board.bin, regdb.bin), btqca.c (rom 0x20).
put qcom/a650_sqe.fw vendor a650_sqe.fw "msm (Adreno 650 SQE microcode)"
put qcom/a650_gmu.bin vendor a650_gmu.bin "msm (Adreno 650 GMU firmware)"
W=ath11k/QCA6390/hw2.0
put $W/amss.bin vendor qca6390/amss20.bin "ath11k_pci (QCA6390 Wi-Fi firmware)"
put $W/m3.bin vendor qca6390/m3.bin "ath11k_pci (QCA6390 M3 firmware)"
# Stock bdwlan.elf is the same file as bdwlan.e05; ath11k takes board.bin when there is no
# board-2.bin container.
put $W/board.bin vendor qca6390/bdwlan.elf "ath11k_pci (QCA6390 board data)"
put $W/regdb.bin vendor qca6390/regdb.bin "ath11k_pci (regulatory database)"
put qca/htbtfw20.tlv vendor htbtfw20.tlv "btqca/hci_uart (QCA6390 Bluetooth rampatch)"
put qca/htnv20.bin vendor htnv20.bin "btqca/hci_uart (QCA6390 Bluetooth NVM)"
# wm_adsp asks for cirrus/<part>-dsp1-<fw>.*; the stock files carry the cs35l40 part name.
put cirrus/cs35l41-dsp1-spk-prot.wmfw vendor cs35l40-spk-dsp1-spk-prot.wmfw "snd-soc-cs35l41 (wm_adsp speaker protection)"
put cirrus/cs35l41-dsp1-spk-prot.bin vendor cs35l40-spk-dsp1-spk-prot.bin "snd-soc-cs35l41 (wm_adsp protection tuning)"

# 4. Factory speaker calibration from sec_efs:/cirrus, as the gts7l kernel's cs35l41 patch
# reads it: rdc, ambient temperature, vsc, isc as 4 x u32 LE. The amp suffixes per address
# are the stock DT's cirrus,mfd-suffix (dtbo entry 04): 0x40 _r, 0x41 none, 0x42 _br, 0x43 _b.
efs_num() {
    local v
    v=$(debugfs -R "cat /cirrus/$1" "$BACKUP/sec_efs.img" 2>/dev/null | tr -d '\0\n\r ')
    [[ $v =~ ^[0-9]+$ ]] || die "sec_efs:/cirrus/$1 is not a number: '$v'"
    echo "$v"
}
u32le() { printf "\\x$(printf %02x $(($1 & 255)))\\x$(printf %02x $(($1 >> 8 & 255)))\\x$(printf %02x $(($1 >> 16 & 255)))\\x$(printf %02x $(($1 >> 24 & 255)))"; }
temp=$(efs_num temp_cal)
for amp in 0040:_r 0041: 0042:_br 0043:_b; do
    addr=${amp%%:*} s=${amp#*:}
    rdc=$(efs_num rdc_cal$s) vsc=$(efs_num vsc_cal$s) isc=$(efs_num isc_cal$s)
    (( rdc > 5000 && rdc < 15000 )) || die "implausible speaker resistance $rdc for amp $addr"
    f=$work/cal-$addr.bin
    { u32le "$rdc"; u32le "$temp"; u32le "$vsc"; u32le "$isc"; } > "$f"
    put "cirrus/cs35l41-cal-$AMP_BUS-$addr.bin" file "$f" "snd-soc-cs35l41 (factory calibration)" \
        "sec_efs:/cirrus/{rdc,vsc,isc}_cal$s,temp_cal"
done

# Partitions read in place, not laid out here.
mdir -/ -b -i "$BACKUP/modem.img" ::/image | sed 's|^::/image/||' | sort > "$OUT/partitions/modem.txt"
dsp_ls() {
    local e
    debugfs -R "ls -p $1" "$BACKUP/dsp.img" 2>/dev/null | awk -F/ '$6!="" && $6!="." && $6!=".." && $6!="lost+found" {print $3, $6}' |
    while read -r mode e; do
        [[ $mode == 04* ]] && dsp_ls "$1$e/" || echo "${1#/}$e"
    done
}
dsp_ls / | sort > "$OUT/partitions/dsp.txt"

{
    echo "# gts7l device firmware, made by tools/gts7l-firmware.sh on $(date -u +%Y-%m-%dT%H:%MZ)."
    echo "# Not redistributable: never commit, never publish. Sources: SOURCES."
    echo "# form: file = real file in gts7l_fw.img; link = symlink to $APNHLOS_MNT/<file> (apnhlos)."
    echo "# Not laid out: modem (X55 over MHI, T7) and dsp (FastRPC shared objects, userspace) stay"
    echo "# on their partitions (partitions/*.txt). Stock vendor files left out: ipa_* (TZ rejects"
    echo "# them, apnhlos copies used), a6xx files of other GPUs, cs40l* (haptics, not on gts7l),"
    echo "# cs35l40-...-calib.bin (stock calibration run), CAMERA_ICP.elf (cameras, T7),"
    echo "# wlan/qca_cld/WCNSS_qcom_cfg.ini (Qualcomm's cnss driver only), dax_param.bin, Data.msc."
    printf '#path\tsize\tsha256\tsource\tdriver\tform\n'
    sort "$manifest"
} > "$OUT/MANIFEST.tsv"
(cd "$OUT/tree" && find . -type f | sed 's|^\./||' | sort | xargs sha256sum) > "$OUT/SHA256SUMS"
{
    for f in "$BACKUP"/{apnhlos,sec_efs,modem,dsp}.img "$VENDOR" "$DTB"; do
        echo "$(sha256sum "$f" | cut -d' ' -f1)  $f"
    done
} > "$OUT/SOURCES"

# gts7l_fw.img. The old link names of firmware/init.gts7l.firmware.rc stay valid inside it.
mkdir -p "$work/img/gts7l"
ln -s /mnt/vendor/apnhlos/image "$work/img/gts7l/apnhlos"
ln -s /mnt/vendor/modem/image "$work/img/gts7l/modem"
ln -s /vendor/firmware "$work/img/gts7l/vendor"
install -m 0644 "$OUT/MANIFEST.tsv" "$work/img/MANIFEST.tsv"
find "$work/img" -exec touch -h -d @1700000000 {} +
used=$(du -s --block-size=4096 "$work/img" | cut -f1)
blocks=$(( (used * 5 / 4 + 1024 + 255) / 256 * 256 ))  # 25% + 4 MiB headroom, whole MiB
rm -f "$OUT/gts7l_fw.img"
E2FSPROGS_FAKE_TIME=1700000000 mke2fs -q -t ext4 -b 4096 -m 0 -O ^has_journal -L gts7l_fw \
    -U 6774736c-5f66-7700-0000-000000000001 -E root_owner=0:0,hash_seed=6774736c-5f66-7700-0000-000000000002 \
    -d "$work/img" "$OUT/gts7l_fw.img" "$blocks"
e2fsck -fn "$OUT/gts7l_fw.img" >/dev/null 2>&1 || die "gts7l_fw.img does not pass e2fsck"

# Check: every manifest entry is in the image, as a file with the right content or as a link
# to the apnhlos file of the same content.
mkdir "$work/check"
debugfs -R "rdump / $work/check" "$OUT/gts7l_fw.img" 2>/dev/null
n=0
while IFS=$'\t' read -r path size sum src driver form; do
    [[ $path == \#* ]] && continue
    if [[ $form == link ]]; then
        t=$(readlink "$work/check/$path") || die "image: $path is not a link"
        [[ $(sha256sum < "$A/${t#$APNHLOS_MNT/}" | cut -d' ' -f1) == "$sum" ]] || die "image: $path -> $t differs"
    else
        [[ $(sha256sum < "$work/check/$path" | cut -d' ' -f1) == "$sum" ]] || die "image: $path differs"
    fi
    n=$((n + 1))
done < "$OUT/MANIFEST.tsv"
(cd "$OUT/tree" && sha256sum -c --quiet ../SHA256SUMS)

echo "gts7l-firmware: $n files in $OUT/tree ($(du -sh "$OUT/tree" | cut -f1)), gts7l_fw.img $(( $(stat -c %s "$OUT/gts7l_fw.img") >> 20 )) MiB"
cut -f5 "$OUT/MANIFEST.tsv" | grep -v '^#\|^driver$' | sed 's/ (.*//' | sort | uniq -c
