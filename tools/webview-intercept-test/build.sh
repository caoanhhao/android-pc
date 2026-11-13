#!/usr/bin/env bash
# Build the WebView shouldInterceptRequest test app (F9p) with the AOSP host tools:
#   tools/webview-intercept-test/build.sh OUT.apk
# Then: adb install OUT.apk; adb shell am start -n pc.test.wvintercept/.MainActivity;
#       adb logcat -s WVIntercept InputStreamUtil
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AOSP=$HERE/../../aosp
HOST=$AOSP/out/host/linux-x86/bin
JAR=$AOSP/prebuilts/sdk/current/public/android.jar
out=$(realpath -m "$1")
work=$(mktemp -d)
trap '/usr/bin/rm -rf "$work"' EXIT
"$AOSP/prebuilts/jdk/jdk21/linux-x86/bin/javac" -source 11 -target 11 -nowarn -cp "$JAR" -d "$work/classes" \
    "$HERE"/src/pc/test/wvintercept/*.java
"$HOST/d8" --min-api 29 --lib "$JAR" --output "$work" $(find "$work/classes" -name '*.class')
"$HOST/aapt2" link -I "$JAR" --manifest "$HERE/AndroidManifest.xml" -o "$work/app.apk"
(cd "$work" && zip -q app.apk classes.dex)
"$HOST/zipalign" -f 4 "$work/app.apk" "$work/aligned.apk"
"$HOST/apksigner" sign --key "$AOSP/build/make/target/product/security/testkey.pk8" \
    --cert "$AOSP/build/make/target/product/security/testkey.x509.pem" --out "$out" "$work/aligned.apk"
echo "$out"
