#!/usr/bin/env bash
# Build the H3 guest-hook test app (an arm64 library that PLT-hooks the host libartbase.so,
# like NetEase's libunisec.so) with the AOSP host tools:
#   tools/guest-hook-test/build.sh OUT.apk
# Then: adb install OUT.apk; adb shell am start -W -n pc.test.guesthook/.MainActivity;
#       adb logcat -d -s GuestHookTest pc_native_bridge   (PASS / FAIL, or a tombstone)
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AOSP=$HERE/../../aosp
HOST=$AOSP/out/host/linux-x86/bin
JAR=$AOSP/prebuilts/sdk/current/public/android.jar
CLANG=$AOSP/prebuilts/clang/host/linux-x86/clang-r596125/bin/clang
out=$(realpath -m "$1")
work=$(mktemp -d)
trap '/usr/bin/rm -rf "$work"' EXIT
mkdir -p "$work/lib/arm64-v8a"
"$CLANG" --target=aarch64-linux-android29 -shared -fPIC -O2 -ffreestanding -nostdlib \
    -fno-stack-protector -mno-outline-atomics -fuse-ld=lld -Wl,--hash-style=both \
    -Wl,-soname,libguesthook.so -I "$AOSP/libnativehelper/include_jni" \
    -o "$work/lib/arm64-v8a/libguesthook.so" "$HERE/jni/guesthook.c"
"$AOSP/prebuilts/jdk/jdk21/linux-x86/bin/javac" -source 11 -target 11 -nowarn -cp "$JAR" -d "$work/classes" \
    "$HERE"/src/pc/test/guesthook/*.java
"$HOST/d8" --min-api 29 --lib "$JAR" --output "$work" $(find "$work/classes" -name '*.class')
"$HOST/aapt2" link -I "$JAR" --manifest "$HERE/AndroidManifest.xml" -o "$work/app.apk"
(cd "$work" && zip -q app.apk classes.dex lib/arm64-v8a/libguesthook.so)
"$HOST/zipalign" -f -p 4 "$work/app.apk" "$work/aligned.apk"
"$HOST/apksigner" sign --key "$AOSP/build/make/target/product/security/testkey.pk8" \
    --cert "$AOSP/build/make/target/product/security/testkey.x509.pem" --out "$out" "$work/aligned.apk"
echo "$out"
