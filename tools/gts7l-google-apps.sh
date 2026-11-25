#!/usr/bin/env bash
# microG GmsCore, Google Play Store and Aurora Store on the Galaxy Tab S7 LTE as ordinary user
# apps, in userdata (docs/gts7l.md §24). The aosp_gts7l ROM carries nothing of Google's, unlike
# the PC product; these are installed over adb and survive `gts7l-android-flash.sh --keep-data`.
#
#   tools/gts7l-google-apps.sh [--accept-license] [ADB_SERIAL]
#
# - microG: the APK tools/fetch-apps.sh microg fetched and checked (one APK for every ABI),
#   0.3.18 or later: 0.3.16.1 and 0.3.17 break AccountManager sign-in from other apps
#   (Aurora Store's "microG" login, microg/GmsCore#3853).
#   tools/gts7l-microg-patch.sh makes it check in with a Vietnamese mobile network instead
#   of microG's German one and signs it with AOSP's testkey; it replaces a microG signed
#   with microG's key only after `adb uninstall com.google.android.gms` (sign in again).
#   tools/patches/aosp17-microg-signature-spoofing.patch lets it report Google's signature
#   although it is not a system app, because it is signed with testkey or microG's release
#   key. Its runtime permissions are granted here.
# - Google Play Store (com.android.vending): Google's APK from the Android 17 arm64 emulator
#   image (Android SDK License Agreement; asks unless --accept-license), pinned by SHA-256 and
#   signer. tools/patches/aosp17-play-store-user-app.patch gives it, as an ordinary app, the
#   signature permissions a preinstalled Play Store has (without them it crashes at start:
#   UserManager.getUsers needs MANAGE_USERS), makes it force-queryable and approves its app
#   links. Personal use only: never publish the APK.
# - Aurora Store (com.aurora.store), an open-source client of Google Play's catalogue that
#   needs no privileges: F-Droid's build, pinned by version code and SHA-256, signer checked.
set -euo pipefail
WT="$(cd "$(dirname "$0")/.." && pwd)"
ROOT=$WT
[[ -d $ROOT/aosp ]] || ROOT="$(cd "$(git -C "$WT" rev-parse --path-format=absolute --git-common-dir)/.." && pwd)"
accept=0
[[ ${1:-} == --accept-license ]] && { accept=1; shift; }
adb=(adb ${1:+-s "$1"})
die() { echo "gts7l-google-apps: $*" >&2; exit 1; }
host=$ROOT/aosp/out/host/linux-x86/bin
signed_by() {   # APK, certificate SHA-256
    "$host/apksigner" verify --print-certs "$1" 2>/dev/null | grep -q "SHA-256 digest: $2"
}

gms=$ROOT/aosp/device/android-pc/apps/GmsCore/GmsCore.apk
[[ -f $gms ]] || die "no $gms, run tools/fetch-apps.sh microg"
signed_by "$gms" 9bd06727e62796c0130eb6dab39b73157451582cbd138e86c468acc395d14165 ||
    die "$gms is not signed with microG's release key"

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
signed_by "$dl/$aurora_apk" $aurora_cert || die "$aurora_apk is not signed with Aurora's certificate"

# Play Store 50.7.34-31, product/priv-app/Phonesky of the emulator image's super partition.
ps_zip=google_apis_playstore-arm64-v8a-37.0_r08.zip ps_zip_sha1=ac251e27178d78cffb0b7ee577d79643491a89a6
ps_sha=82f47f6d3b069f90bd3498ac71767f9eac2b674c606ddd8cf8eea2a65fff68b1
ps_cert=f0fd6c5b410f25cb25c3b53346c8972fae30f8ee7411df910480ad6b2d60db83
cache=${XDG_CACHE_HOME:-$HOME/.cache}/android-pc-extras
ps=$ROOT/out/downloads/playstore/Phonesky.apk
if [[ ! -f $ps ]]; then
    if [[ ! -f $cache/$ps_zip ]]; then
        if (( ! accept )); then
            echo "Google Play Store comes from Google's Android emulator system image, licensed to"
            echo "you under the Android Software Development Kit License Agreement"
            echo "(https://developer.android.com/studio/terms). Personal use only."
            read -r -p "Accept the license and continue? Type yes: " ok
            [[ $ok == yes ]] || die "not accepted"
        fi
        mkdir -p "$cache"
        curl -fL -o "$cache/$ps_zip.part" \
            "https://dl.google.com/android/repository/sys-img/google_apis_playstore/arm64-v8a-37.0_r08.zip"
        mv "$cache/$ps_zip.part" "$cache/$ps_zip"
    fi
    echo "$ps_zip_sha1  $cache/$ps_zip" | sha1sum -c --quiet
    work=$(dirname "$ps")/work
    /usr/bin/rm -rf "$work" && mkdir -p "$work"
    unzip -q -o -d "$work" "$cache/$ps_zip" arm64-v8a/system.img
    # arm64-v8a/system.img is a GPT disk whose partition 2 is the dynamic-partition "super".
    start=$(sfdisk -d "$work/arm64-v8a/system.img" | sed -n 's/.*: start= *\([0-9]*\),.*/\1/p' | sed -n 2p)
    dd if="$work/arm64-v8a/system.img" of="$work/super.img" bs=512 skip="$start" status=none
    /usr/bin/rm -f "$work/arm64-v8a/system.img"
    lpunpack -p product "$work/super.img" "$work" >/dev/null
    /usr/bin/rm -f "$work/super.img"
    "$host/fsck.erofs" --extract="$work/product" "$work/product.img" >/dev/null
    cp "$work/product/priv-app/Phonesky/Phonesky.apk" "$ps.part"
    /usr/bin/rm -rf "$work"
    mv "$ps.part" "$ps"
fi
echo "$ps_sha  $ps" | sha256sum -c --quiet
signed_by "$ps" $ps_cert || die "$ps is not signed with Google Play's certificate"

gms_vn=$ROOT/out/gts7l/microg/GmsCore-vn.apk
"$WT/tools/gts7l-microg-patch.sh" "$gms_vn"
"${adb[@]}" install -r "$gms_vn" ||
    die "microG not updated: if it is still signed with microG's key, adb uninstall com.google.android.gms first"
"${adb[@]}" install -r "$ps"
"${adb[@]}" install -r "$dl/$aurora_apk"
for p in ACCESS_FINE_LOCATION ACCESS_COARSE_LOCATION ACCESS_BACKGROUND_LOCATION READ_PHONE_STATE \
         GET_ACCOUNTS POST_NOTIFICATIONS BLUETOOTH_SCAN BLUETOOTH_CONNECT BLUETOOTH_ADVERTISE \
         NEARBY_WIFI_DEVICES; do
    "${adb[@]}" shell pm grant com.google.android.gms android.permission.$p 2>/dev/null || true
done
# A preinstalled Play Store is allow-in-power-save-except-idle (google.xml); without it downloads
# crash com.android.vending:background (docs/arm-translation.md, F9d). The user allowlist is
# kept in /data/system/deviceidle.xml across reboots and self-updates (not across uninstall).
for p in com.google.android.gms com.android.vending; do
    "${adb[@]}" shell cmd deviceidle whitelist +$p >/dev/null
done
# A preinstalled microG is also allow-in-data-usage-save (aosp-device/microg/sysconfig-microg.xml):
# with Data Saver on, push messaging and checkin keep their background network access.
gms_uid=$("${adb[@]}" shell pm list packages -U com.google.android.gms |
          sed -n 's/^package:com\.google\.android\.gms uid:\([0-9]*\).*/\1/p')
[[ -n $gms_uid ]] || die "no uid for com.google.android.gms"
"${adb[@]}" shell cmd netpolicy add restrict-background-whitelist "$gms_uid"
"${adb[@]}" shell pm list packages --show-versioncode |
    grep -E 'com.google.android.gms|com.android.vending|com.aurora.store'
