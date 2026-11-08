#!/usr/bin/env bash
# Build apps/hello into an x86_64 APK from source with the plain SDK tools
# (aapt2, javac, d8, NDK clang, zipalign, apksigner) — no Gradle.
#
#   apps/hello/build.sh            -> apps/hello/build/hello.apk
#
# Environment overrides:
#   ANDROID_HOME         SDK root (default: $ANDROID_SDK_ROOT, ~/Android/Sdk, /opt/android-sdk)
#   ANDROID_NDK_HOME     NDK root (default: newest $ANDROID_HOME/ndk/*)
#   BUILD_TOOLS_VERSION  (default: newest installed)
#   COMPILE_SDK          platform to compile against (default: 31)
#   JAVA_HOME            JDK with javac, 11..21 (default: probed in /usr/lib/jvm)
#   KEYSTORE             signing keystore (default: ~/.android/debug.keystore,
#                        created like Android Studio does if missing)
#
# The output is reproducible: building twice with the same tools and key
# gives a byte-identical APK (fixed zip timestamps, no build ids/paths).
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
out="$here/build"
MIN_SDK=24
TARGET_SDK=31
COMPILE_SDK="${COMPILE_SDK:-31}"
ABI=x86_64

missing=()
need() { missing+=("$1"); }

# --- locate tools -----------------------------------------------------------
sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
if [ -z "$sdk" ]; then
    for d in "$HOME/Android/Sdk" /opt/android-sdk; do [ -d "$d" ] && { sdk="$d"; break; }; done
fi
[ -d "${sdk:-/nonexistent}" ] || need "Android SDK (set ANDROID_HOME)"

bt=""
if [ -d "$sdk/build-tools" ]; then
    v="${BUILD_TOOLS_VERSION:-$(ls "$sdk/build-tools" | sort -V | tail -n1)}"
    bt="$sdk/build-tools/$v"
fi
for t in aapt2 d8 zipalign apksigner; do
    [ -x "$bt/$t" ] || need "build-tools ($t): sdkmanager 'build-tools;35.0.0'"
done

android_jar="$sdk/platforms/android-$COMPILE_SDK/android.jar"
[ -f "$android_jar" ] || need "platform android-$COMPILE_SDK: sdkmanager 'platforms;android-$COMPILE_SDK'"

ndk="${ANDROID_NDK_HOME:-}"
if [ -z "$ndk" ] && [ -d "$sdk/ndk" ]; then
    ndk="$(ls -d "$sdk"/ndk/*/ 2>/dev/null | sort -V | tail -n1)"
fi
ndk="${ndk%/}"
cc="$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android$MIN_SDK-clang"
strip="$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip"
[ -x "$cc" ] || need "NDK (r23+): sdkmanager 'ndk;27.2.12479018' or set ANDROID_NDK_HOME"

if [ -z "${JAVA_HOME:-}" ]; then
    for d in /usr/lib/jvm/java-21-openjdk /usr/lib/jvm/java-17-openjdk /usr/lib/jvm/java-11-openjdk; do
        [ -x "$d/bin/javac" ] && { JAVA_HOME="$d"; break; }
    done
fi
[ -x "${JAVA_HOME:-/nonexistent}/bin/javac" ] || need "JDK 11-21 with javac (e.g. pacman -S jdk21-openjdk), set JAVA_HOME"

if [ ${#missing[@]} -gt 0 ]; then
    echo "Missing build requirements:" >&2
    printf '  - %s\n' "${missing[@]}" >&2
    echo "Without root, the SDK can live in ~/Android/Sdk: unpack Google's" >&2
    echo "commandlinetools-linux-*.zip to ~/Android/Sdk/cmdline-tools/latest and run" >&2
    echo "its bin/sdkmanager with the packages above." >&2
    exit 1
fi
export JAVA_HOME PATH="$JAVA_HOME/bin:$PATH"

keystore="${KEYSTORE:-$HOME/.android/debug.keystore}"
if [ ! -f "$keystore" ]; then
    mkdir -p "$(dirname "$keystore")"
    keytool -genkeypair -keystore "$keystore" -storepass android -keypass android \
        -alias androiddebugkey -dname "CN=Android Debug,O=Android,C=US" \
        -keyalg RSA -keysize 2048 -validity 10000 >/dev/null
fi

echo "SDK $sdk | build-tools $(basename "$bt") | android-$COMPILE_SDK | NDK $(sed -n "s/^Pkg.Revision *= *//p" "$ndk/source.properties") | $("$JAVA_HOME/bin/javac" -version 2>&1)"

# --- build -------------------------------------------------------------------
rm -rf "$out"
mkdir -p "$out/gen" "$out/classes" "$out/dex" "$out/lib/$ABI"

# Resources + manifest -> base APK and R.java.
"$bt/aapt2" compile --dir "$here/res" -o "$out/res.zip"
"$bt/aapt2" link -o "$out/base.apk" -I "$android_jar" \
    --manifest "$here/AndroidManifest.xml" --java "$out/gen" \
    --min-sdk-version $MIN_SDK --target-sdk-version $TARGET_SDK \
    "$out/res.zip"

# Java -> classes -> dex.
find "$here/src" "$out/gen" -name '*.java' | LC_ALL=C sort >"$out/sources.txt"
javac --release 8 -Xlint:-options -encoding UTF-8 -implicit:none \
    -classpath "$android_jar" -d "$out/classes" @"$out/sources.txt"
find "$out/classes" -name '*.class' | LC_ALL=C sort >"$out/classes.txt"
"$bt/d8" --release --min-api $MIN_SDK --lib "$android_jar" --output "$out/dex" @"$out/classes.txt"

# JNI library.
"$cc" -shared -fPIC -O2 -Wall -Wextra -Werror \
    -ffile-prefix-map="$here"=. -Wl,--build-id=none -Wl,-z,max-page-size=16384 \
    -o "$out/lib/$ABI/libhello.so" "$here/jni/hello.c"
"$strip" --strip-unneeded "$out/lib/$ABI/libhello.so"

# Assemble with fixed timestamps and order; resources.arsc stays stored.
python3 - "$out/base.apk" "$out/unaligned.apk" "$out/dex/classes.dex" "$out/lib/$ABI/libhello.so" "$ABI" <<'PY'
import sys, zipfile
base, dst, dex, so, abi = sys.argv[1:]
FIXED = (1981, 1, 1, 1, 1, 2)
def add(z, name, data, compress):
    zi = zipfile.ZipInfo(name, FIXED)
    zi.compress_type = compress
    zi.external_attr = 0o644 << 16
    zi.create_system = 0
    z.writestr(zi, data, compresslevel=9 if compress == zipfile.ZIP_DEFLATED else None)
with zipfile.ZipFile(base) as src, zipfile.ZipFile(dst, "w") as z:
    for info in src.infolist():
        add(z, info.filename, src.read(info), info.compress_type)
    add(z, "classes.dex", open(dex, "rb").read(), zipfile.ZIP_DEFLATED)
    add(z, f"lib/{abi}/libhello.so", open(so, "rb").read(), zipfile.ZIP_DEFLATED)
PY
"$bt/zipalign" -p -f 4 "$out/unaligned.apk" "$out/aligned.apk"
"$bt/apksigner" sign --ks "$keystore" --ks-pass pass:android --key-pass pass:android \
    --ks-key-alias androiddebugkey --out "$out/hello.apk" "$out/aligned.apk"
"$bt/apksigner" verify "$out/hello.apk"
rm -f "$out/hello.apk.idsig"

echo "$out/hello.apk  sha256 $(sha256sum "$out/hello.apk" | cut -d' ' -f1)"
