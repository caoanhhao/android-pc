#!/bin/bash
# Builds libhyprlua_jni for the host (external/lua + jni/, with the system C compiler) and
# runs hyprlua's unit tests against real Lua, with the AOSP tree's kotlinc and JDK, without
# Soong. Under Soong the same tests are `hyprlua-tests` (java_test_host, Android.bp).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
AOSP=${AOSP:-$HERE/../../../aosp}
export JAVA_HOME=$AOSP/prebuilts/jdk/jdk21/linux-x86
KOTLINC=$AOSP/external/kotlinc/bin/kotlinc
JUNIT=$AOSP/prebuilts/devtools/tools/lib/junit-4.12.jar
HAMCREST=$AOSP/prebuilts/devtools/tools/lib/hamcrest-core-1.3.jar
STDLIB=$AOSP/external/kotlinc/lib/kotlin-stdlib.jar
LUA=$AOSP/external/lua/src
OUT=$(mktemp -d)
trap '/usr/bin/rm -rf "$OUT"' EXIT
mkdir -p "$OUT/lib" "$OUT/classes"
taskset -c 8-15 cc -O1 -g -shared -fPIC -o "$OUT/lib/libhyprlua_jni.so" \
    -DLUA_COMPAT_5_3 -I"$LUA" -I"$JAVA_HOME/include" -I"$JAVA_HOME/include/linux" \
    "$HERE/jni/hyprlua_jni.c" $(ls "$LUA"/*.c | grep -v -e '/lua\.c$' -e '/luac\.c$') -lm
taskset -c 8-15 "$KOTLINC" -nowarn -jvm-target 17 -cp "$JUNIT" -d "$OUT/classes" \
    $(find "$HERE/src" "$HERE/../hyprtile/src" "$HERE/tests/src" -name '*.kt') 2>&1 \
    | grep -v '^warning: unable to find kotlin-stdlib' || true
[ -n "$(find "$OUT/classes" -name 'HyprLuaTest.class')" ] || { echo "compile failed"; exit 1; }
classes=$(cd "$OUT/classes" && find . -name '*Test.class' | sed 's|^\./||; s|\.class$||; s|/|.|g')
"$JAVA_HOME/bin/java" -Djava.library.path="$OUT/lib" \
    -cp "$OUT/classes:$HERE/res:$JUNIT:$HAMCREST:$STDLIB" org.junit.runner.JUnitCore $classes
