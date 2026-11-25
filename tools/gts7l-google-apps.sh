#!/usr/bin/env bash
# microG GmsCore and Aurora Store on the Galaxy Tab S7 LTE as ordinary user apps, in userdata
# (docs/gts7l.md §22). The aosp_gts7l ROM carries nothing of Google's, unlike the PC product;
# these are installed over adb and survive `gts7l-android-flash.sh --keep-data`.
#
#   tools/gts7l-google-apps.sh [ADB_SERIAL]
#
# - microG: the APK tools/fetch-apps.sh microg fetched and checked (one APK for every ABI).
#   As a user app it gets no signature spoofing (the frameworks/base patch accepts only
#   preinstalled system apps) and no privileged permissions; its runtime permissions are
#   granted here.
# - Aurora Store (com.aurora.store), an open-source client of Google Play's catalogue that
#   needs no privileges: F-Droid's build, pinned by version code and SHA-256, signer checked.
#   Google's own Play Store does not run as a user app: it calls UserManager.getUsers(), which
#   needs the privileged MANAGE_USERS, and crashes at start.
set -euo pipefail
WT="$(cd "$(dirname "$0")/.." && pwd)"
ROOT=$WT
[[ -d $ROOT/aosp ]] || ROOT="$(cd "$(git -C "$WT" rev-parse --path-format=absolute --git-common-dir)/.." && pwd)"
adb=(adb ${1:+-s "$1"})
die() { echo "gts7l-google-apps: $*" >&2; exit 1; }

gms=$ROOT/aosp/device/android-pc/apps/GmsCore/GmsCore.apk
[[ -f $gms ]] || die "no $gms, run tools/fetch-apps.sh microg"

aurora_apk=com.aurora.store_76.apk   # 4.8.4
aurora_sha=fd9c75d90d0f4a7c132b9b4a5a2cf1992a45e03b8d8ff988b7dcfbc0db2c4d11
aurora_cert=5c83c7672b929955dc0a1db89a5e6ae4389e2eae7ec939956041694e5815f532
dl=$ROOT/out/downloads/aurora
mkdir -p "$dl"
if [[ ! -f $dl/$aurora_apk ]]; then
    curl -sSfL -o "$dl/$aurora_apk.part" "https://f-droid.org/repo/$aurora_apk"
    echo "$aurora_sha  $dl/$aurora_apk.part" | sha256sum -c --quiet
    mv "$dl/$aurora_apk.part" "$dl/$aurora_apk"
fi
echo "$aurora_sha  $dl/$aurora_apk" | sha256sum -c --quiet
"$ROOT/aosp/out/host/linux-x86/bin/apksigner" verify --print-certs "$dl/$aurora_apk" 2>/dev/null |
    grep -q "SHA-256 digest: $aurora_cert" || die "$aurora_apk is not signed with Aurora's certificate"

"${adb[@]}" install -r "$gms"
"${adb[@]}" install -r "$dl/$aurora_apk"
for p in ACCESS_FINE_LOCATION ACCESS_COARSE_LOCATION ACCESS_BACKGROUND_LOCATION READ_PHONE_STATE \
         GET_ACCOUNTS POST_NOTIFICATIONS BLUETOOTH_SCAN BLUETOOTH_CONNECT BLUETOOTH_ADVERTISE \
         NEARBY_WIFI_DEVICES; do
    "${adb[@]}" shell pm grant com.google.android.gms android.permission.$p 2>/dev/null || true
done
"${adb[@]}" shell cmd deviceidle whitelist +com.google.android.gms >/dev/null
"${adb[@]}" shell pm list packages --show-versioncode | grep -E 'com.google.android.gms|com.aurora.store'
