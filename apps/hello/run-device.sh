#!/usr/bin/env bash
# Run build/hello.apk on a device reachable by adb (Waydroid, an emulator,
# or later the Rust kernel's adbd).
#
#   apps/hello/run-device.sh [-s SERIAL] [--selftest]
#
# Default: install, launch MainActivity, wait for the "HELLO_OK" line the app
# logs after its JNI call succeeded.
# --selftest: no install and no framework — run SelfTest.main() from the APK
# with app_process (ART + dex + JNI only) and expect "HELLO_OK" on stdout.
#
# For Waydroid: `waydroid session start`, then `adb connect <ip>:5555`
# (IP from `waydroid status`), or pass -s <ip>:5555.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
apk="$here/build/hello.apk"
adb=(adb)
selftest=0
while [ $# -gt 0 ]; do
    case "$1" in
        -s) adb+=(-s "$2"); shift 2 ;;
        --selftest) selftest=1; shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
[ -f "$apk" ] || { echo "build the APK first: apps/hello/build.sh" >&2; exit 1; }

"${adb[@]}" wait-for-device
abi="$("${adb[@]}" shell getprop ro.product.cpu.abi | tr -d '\r')"
echo "device: $("${adb[@]}" shell getprop ro.product.model | tr -d '\r') / Android $("${adb[@]}" shell getprop ro.build.version.release | tr -d '\r') / $abi"

if [ $selftest -eq 1 ]; then
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    # Use the library exactly as packaged in the APK.
    unzip -q -o "$apk" "lib/$abi/libhello.so" -d "$tmp"
    dir=/data/local/tmp/androidpc-hello
    "${adb[@]}" shell "rm -rf $dir && mkdir -p $dir"
    "${adb[@]}" push -q "$apk" "$tmp/lib/$abi/libhello.so" "$dir/"
    out="$("${adb[@]}" shell "CLASSPATH=$dir/hello.apk app_process -Djava.library.path=$dir /system/bin com.androidpc.hello.SelfTest" | tr -d '\r')"
    "${adb[@]}" shell "rm -rf $dir"
    echo "$out"
    if grep -qx HELLO_OK <<<"$out"; then
        echo "PASS: APK dex + x86_64 JNI ran on ART (app_process)"
        exit 0
    fi
    echo "FAIL: SelfTest did not print HELLO_OK" >&2
    exit 1
fi

if ! inst="$("${adb[@]}" install -r -t "$apk" 2>&1)"; then
    echo "$inst" >&2
    if grep -q VERIFICATION_FAILURE <<<"$inst"; then
        echo "The device's package verifier (Google Play Protect) rejected the adb install." >&2
        echo "Allow it on the device (Play Store > Play Protect > settings, or" >&2
        echo "'adb shell settings put global verifier_verify_adb_installs 0'), or use --selftest." >&2
    fi
    exit 1
fi
echo "$inst"
"${adb[@]}" logcat -c
"${adb[@]}" shell am force-stop com.androidpc.hello
"${adb[@]}" shell am start -W -n com.androidpc.hello/.MainActivity

for _ in $(seq 1 30); do
    line="$("${adb[@]}" logcat -d -s AndroidPcHello:I | grep HELLO_OK || true)"
    if [ -n "$line" ]; then
        echo "$line"
        echo "PASS: app ran and its x86_64 JNI library answered"
        exit 0
    fi
    sleep 1
done
echo "FAIL: no HELLO_OK in logcat; recent errors:" >&2
"${adb[@]}" logcat -d '*:E' | tail -n 30 >&2
exit 1
