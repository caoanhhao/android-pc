# apps/hello — APK mẫu x86_64 build từ mã nguồn

Một `Activity` (không AndroidX) hiển thị chuỗi do thư viện JNI x86_64
(`libhello.so`, biên dịch bằng NDK) trả về. Build bằng công cụ SDK thuần
(`aapt2` → `javac` → `d8` → NDK `clang` → `zipalign` → `apksigner`), không cần
Gradle hay mạng.

```bash
apps/hello/build.sh                       # -> apps/hello/build/hello.apk
apps/hello/run-device.sh -s <ip>:5555     # cài + mở Activity, chờ "HELLO_OK" trong logcat
apps/hello/run-device.sh -s <ip>:5555 --selftest   # không cài: chạy dex+JNI bằng app_process
```

## Yêu cầu

`build.sh` tự dò và báo thiếu gì:

| Thành phần | Mặc định dò | Ghi đè |
|---|---|---|
| Android SDK | `$ANDROID_HOME`, `$ANDROID_SDK_ROOT`, `~/Android/Sdk`, `/opt/android-sdk` | `ANDROID_HOME` |
| build-tools (aapt2, d8, zipalign, apksigner) | bản mới nhất đã cài | `BUILD_TOOLS_VERSION` |
| platform `android-31` (`android.jar`) | | `COMPILE_SDK` |
| NDK r23+ | `$ANDROID_NDK_HOME`, bản mới nhất trong `$ANDROID_HOME/ndk` | `ANDROID_NDK_HOME` |
| JDK 11–21 có `javac` | `/usr/lib/jvm/java-{21,17,11}-openjdk` | `JAVA_HOME` |
| keystore debug | `~/.android/debug.keystore` (tự tạo nếu chưa có) | `KEYSTORE` |

Máy này đã có sẵn: SDK `/opt/android-sdk` (build-tools 30.0.2, platform 31),
NDK r30 (`/opt/android-ndk`), `jdk21-openjdk`. Trên máy khác không có sudo: giải
nén `commandlinetools-linux-*.zip` vào `~/Android/Sdk/cmdline-tools/latest` rồi
`sdkmanager 'build-tools;35.0.0' 'platforms;android-31' 'ndk;27.2.12479018'`.

## Thông số APK

- package `com.androidpc.hello`, minSdk 24, targetSdk 31, ABI duy nhất `x86_64`.
- `lib/x86_64/libhello.so`: liên kết với bionic (`libc.so`, `liblog` không dùng),
  page size 16 KiB, không build-id.
- Build **tái lập**: cùng công cụ + cùng key → APK giống hệt từng byte (timestamp
  zip cố định, không đường dẫn/ build-id trong `.so`). `build.sh` in sha256.

## Kiểm tra trên Waydroid (Android 13 x86_64)

- `--selftest`: **PASS** — `SelfTest.main()` trong APK chạy trên ART qua
  `app_process`, nạp `libhello.so` lấy từ chính APK, in `HELLO_OK`.
  Chế độ này chỉ cần ART + dex + JNI, không cần `system_server`, nên cũng dùng
  được trên kernel Rust trước mốc D3.
- Cài đặt qua `adb install`: Waydroid này có GApps, và **Google Play Protect từ
  chối** (`INSTALL_FAILED_VERIFICATION_FAILURE: Install not allowed`) vì app không
  thuộc developer đã đăng ký. Muốn cài, chủ thiết bị phải cho phép trên thiết bị
  (Play Store → Play Protect → tắt quét app cài qua adb, hoặc
  `adb shell settings put global verifier_verify_adb_installs 0`). Ảnh
  AOSP build từ mã nguồn (A2) không có Play Protect nên không bị chặn.
