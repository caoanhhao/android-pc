#!/usr/bin/env bash
# microG GmsCore with the mobile network the device checks in with set to MCCMNC (default
# 45204, Viettel, Vietnam), re-signed with AOSP's testkey (docs/gts7l.md §24).
#
#   tools/gts7l-microg-patch.sh [OUT.apk]
#
# microG's org.microg.gms.common.PhoneInfo hard-codes cellOperator and simOperator 26207 (O2,
# Germany) for every checkin, so Google registers the device as device_country=de and Play
# hides apps that are not sold in Germany ("This item isn't available in your country", Genshin
# Impact) although the account's Play country is the user's. Stock GMS sends the SIM's operator.
# The string is replaced in the one dex that has it (baksmali, sed, smali), and the APK is
# signed again: tools/patches/aosp17-microg-signature-spoofing.patch accepts testkey besides
# microG's release key. The output replaces microG's own APK in tools/gts7l-google-apps.sh.
set -euo pipefail
WT="$(cd "$(dirname "$0")/.." && pwd)"
ROOT=$WT
[[ -d $ROOT/aosp ]] || ROOT="$(cd "$(git -C "$WT" rev-parse --path-format=absolute --git-common-dir)/.." && pwd)"
die() { echo "gts7l-microg-patch: $*" >&2; exit 1; }
mccmnc=${MCCMNC:-45204}
[[ $mccmnc =~ ^[0-9]{5,6}$ ]] || die "MCCMNC must be 5 or 6 digits, not $mccmnc"
aosp=$ROOT/aosp
host=$aosp/out/host/linux-x86
gms=$aosp/device/android-pc/apps/GmsCore/GmsCore.apk
out=${1:-$ROOT/out/gts7l/microg/GmsCore-$mccmnc.apk}
[[ -f $gms ]] || die "no $gms, run tools/fetch-apps.sh microg"
"$host/bin/apksigner" verify --print-certs "$gms" 2>/dev/null |
    grep -q 'SHA-256 digest: 9bd06727e62796c0130eb6dab39b73157451582cbd138e86c468acc395d14165' ||
    die "$gms is not signed with microG's release key"
for jar in smali-baksmali android-smali; do
    [[ -f $host/framework/$jar.jar ]] ||
        die "no $host/framework/$jar.jar: m $jar (through tools/limited-build.sh)"
done

work=$(mktemp -d "${TMPDIR:-/tmp}/microg-patch.XXXXXX")
trap '/usr/bin/rm -rf "$work"' EXIT
unzip -q "$gms" 'classes*.dex' -d "$work/dex"
dex=$(cd "$work/dex" && grep -l 'Lorg/microg/gms/common/PhoneInfo;' classes*.dex) ||
    die "no dex with org.microg.gms.common.PhoneInfo"
[[ $(wc -w <<<"$dex") == 1 ]] || die "PhoneInfo in several dex files: $dex"
java -jar "$host/framework/smali-baksmali.jar" d -o "$work/smali" "$work/dex/$dex"
src=$work/smali/org/microg/gms/common/PhoneInfo.smali
[[ $(grep -c 'const-string v[0-9]*, "26207"' "$src") == 1 ]] ||
    die "PhoneInfo no longer sets 26207 once; check the microG version"
sed -i "s/\(const-string v[0-9]*, \)\"26207\"/\1\"$mccmnc\"/" "$src"
java -jar "$host/framework/android-smali.jar" a -o "$work/$dex" "$work/smali"

cp "$gms" "$work/unsigned.apk"
(cd "$work" && zip -q -X "unsigned.apk" "$dex")
"$host/bin/zipalign" -f -p 4 "$work/unsigned.apk" "$work/aligned.apk"
mkdir -p "$(dirname "$out")"
key=$aosp/build/make/target/product/security/testkey
"$host/bin/apksigner" sign --key "$key.pk8" --cert "$key.x509.pem" --out "$out" "$work/aligned.apk"
"$host/bin/apksigner" verify "$out"
echo "gts7l-microg-patch: $out (checkin operator $mccmnc)"
