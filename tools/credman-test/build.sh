#!/usr/bin/env bash
# Build the Credential Manager passkey test app (H2) with the AOSP host tools:
#   tools/credman-test/build.sh OUT.apk
# Then: adb install OUT.apk; adb shell am start -n pc.test.credman/.MainActivity;
#       adb logcat -s CredManTest CredentialManager
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AOSP=$HERE/../../aosp
HOST=$AOSP/out/host/linux-x86/bin
JAR=$AOSP/prebuilts/sdk/current/public/android.jar
out=$(realpath -m "$1")
work=$(mktemp -d)
trap '/usr/bin/rm -rf "$work"' EXIT
"$AOSP/prebuilts/jdk/jdk21/linux-x86/bin/javac" -source 11 -target 11 -nowarn -cp "$JAR" -d "$work/classes" \
    "$HERE"/src/pc/test/credman/*.java
"$HOST/d8" --min-api 34 --lib "$JAR" --output "$work" $(find "$work/classes" -name '*.class')
"$HOST/aapt2" compile --dir "$HERE/res" -o "$work/res.zip"
"$HOST/aapt2" link -I "$JAR" --manifest "$HERE/AndroidManifest.xml" -o "$work/app.apk" "$work/res.zip"
(cd "$work" && zip -q app.apk classes.dex)
"$HOST/zipalign" -f 4 "$work/app.apk" "$work/aligned.apk"
"$HOST/apksigner" sign --key "$AOSP/build/make/target/product/security/testkey.pk8" \
    --cert "$AOSP/build/make/target/product/security/testkey.x509.pem" --out "$out" "$work/aligned.apk"
echo "$out"
