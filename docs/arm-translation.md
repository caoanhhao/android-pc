# ARM64 apps and microG on aosp_pc_x86_64

The image is x86_64-only. Many apps (TikTok, most games) ship native code only for
`arm64-v8a`. Without a translator they crash at load time, for example
`libnxreword.so ... is for EM_AARCH64`. Phase F9 adds:

- an ARM64 → x86_64 native bridge (Google's `libndk_translation`);
- microG in place of Google Play services, and Google's real Play Store, both preinstalled
  and enabled.

## Which translator

| Option | Licence | arm64 on x86_64-only | Verdict |
|---|---|---|---|
| Berberis (`frameworks/libs/binary_translation`, in AOSP 17) | Apache 2.0 | No. Only the riscv64 guest is built. The arm64 directories are scaffolding: no decoder, interpreter or translator, and no `libberberis_arm64` module. | Not usable yet |
| Google `libndk_translation` | Proprietary. It ships in the Android Emulator system images, which are licensed for development use. | Yes. The API 37 "Google APIs" x86_64 image is x86_64-only with `arm64-v8a`, the same layout as ours. | **Chosen** |
| Intel `libhoudini` | Proprietary. Taken from ChromeOS/WSA images. | Yes, but older builds. It crashed (SIGSEGV) in TikTok worker threads in earlier tests. | Not used |

`libndk_translation` may not be redistributed. So:

- it is never committed;
- `tools/fetch-ndk-translation.sh` downloads it on the build machine from Google's own
  server;
- images built with it are for personal use only. Do not publish them.

32-bit `armeabi-v7a` apps cannot run. The product has no 32-bit zygote (x86_64-only, the
same as Google's own x86_64-only desktop/emulator images). That would need the 32-bit
`vsoc_x86_64` board.

## Native bridge: how it is wired

Only the translator itself is proprietary. Everything else is built from AOSP source, the
way Google's own `x86_64_ndk_translation_support.mk` does it.

1. `tools/fetch-ndk-translation.sh` downloads `google_apis/x86_64-37.0_r08.zip` (2.1 GB).
   It checks the SHA-1 that Google publishes and pins in the script. It unpacks
   `system.img` (a GPT disk with `super` in partition 2, then `lpunpack`, then `debugfs`)
   and copies 23 files into `aosp/device/android-pc/ndk_translation/system/`:
   - `lib64/libndk_translation.so`, the translator;
   - `lib64/libproxy_arm64_to_x86_64_*.so` (21), host-side proxies for the NDK libraries;
   - `bin/ndk_translation_program_runner_binfmt_misc_arm64`, which runs arm64
     executables.
2. `aosp-device/ndk_translation.mk` (included by `aosp_pc_x86_64.mk`) does four things:
   - copies those files to `/system`;
   - adds these AOSP modules:
     - `$(NATIVE_BRIDGE_PRODUCT_PACKAGES)` from `native_bridge_support.mk`: the arm64
       guest bionic and NDK libraries in `/system/lib64/arm64` and `bin/arm64/`. They are
       built for `TARGET_NATIVE_BRIDGE_ARCH := arm64`, which the Cuttlefish board already
       sets. The guest libc lives in the Soong namespace
       `frameworks/libs/native_bridge_support/android_api/libc`, so that namespace is added
       too;
     - `arm64_dyn`, `arm64_exe`, `berberis_arm_or_arm64.rc`, `berberis_cpuinfo_arm64`
       and `ld.config.arm64.txt` from `frameworks/libs/binary_translation/prebuilt`;
     - `libberberis_exec_region`. The translator maps its code region from this library at
       a fixed path. Without it every arm64 app aborts with
       `exec_region_elf_backed.cc:48: CHECK failed`.
   - sets these properties:
     - `ro.dalvik.vm.native.bridge=libndk_translation.so`, which overrides the `?=0` from
       `runtime_libart.mk`;
     - `ro.dalvik.vm.isa.arm64=x86_64`;
     - `ro.enable.native.bridge.exec=1`.

     `PC_NATIVE_BRIDGE=off` at build time leaves them out (fallback/debug);
   - lists the new system properties and files in the `generic_system.mk` artifact-path
     allow lists.
3. Cuttlefish's phone builds `/system` from the fixed Soong module
   `aosp_shared_system_image`. That module ignores device packages and `PRODUCT_COPY_FILES`
   on `/system`. `ndk_translation.mk` clears `PRODUCT_SOONG_DEFINED_SYSTEM_IMAGE`, and
   `BoardConfig.mk` clears `USE_SOONG_DEFINED_SYSTEM_IMAGE`. It has to be the board config:
   the product makefiles get reset to `aosp_cf.mk`'s value. With both cleared, Soong
   generates the system image from this product, as for most products. `BoardConfig.mk`
   also sets `BUILD_BROKEN_ELF_PREBUILT_PRODUCT_COPY_FILES`. When nothing has been fetched,
   none of this applies and the image is as before.
4. The Cuttlefish board already declares `TARGET_NATIVE_BRIDGE_ABI := arm64-v8a`. So
   `ro.product.cpu.abilist` was already `x86_64,arm64-v8a` and needs no change. Before F9
   this was exactly why arm64 apps installed and then crashed.
5. binfmt_misc: the GKI kernel has `CONFIG_BINFMT_MISC=y`. When
   `ro.dalvik.vm.isa.arm64=x86_64` is set, `berberis_arm_or_arm64.rc` mounts the
   filesystem and registers the arm64 handlers. Cuttlefish's vendor policy already allows
   `init` to write `binfmt_miscfs`. The new files are plain `system_file`, so no new
   SELinux policy is needed.

## microG and the Play Store, preinstalled and enabled

The user chose microG (open source) over real GMS, which is proprietary and needs an
uncertified-device workaround. The first plan was "installed but disabled". After the
real-PC test on 2026-11-12, the user asked for it enabled, plus the real Google Play
Store. microG's own Play Store "FakeStore" is only a stub, so the real Play Store replaces
it and runs on top of microG.

- `tools/fetch-apps.sh microg` fetches GmsCore (`com.google.android.gms`) from microG's
  GitHub release (`MICROG_VER`, default 0.3.17.252432). It checks the APK against
  microG's signing certificate (SHA-256 `9bd06727…d14165`) and sets it up as a
  `/product/priv-app` prebuilt in `aosp/device/android-pc/apps/GmsCore`.
  `aosp-device/microg/privapp-permissions-microg.xml` grants the privileged permissions it
  requests (the ones marked privileged in AOSP 17's framework-res).
- `tools/fetch-apps.sh playstore` takes the Play Store (Phonesky, `com.android.vending`,
  50.7.34) from the official emulator image `google_apis_playstore/x86_64-37.0_r08.zip`.
  It checks the SHA-1 of the zip and checks Google's certificate on the APK (SHA-256
  `f0fd6c5b…60db83`). The image's `product` partition is EROFS: `fsck.erofs --extract`.
  It generates `privapp-permissions-phonesky.xml` from the image's own
  `privapp-permissions-google-p.xml` block. The Play Store is proprietary, like
  `libndk_translation`: fetched on the build machine, never committed, personal use only.
- microG has to report Google's signature. `tools/patches/aosp17-microg-signature-spoofing.patch`
  (apply in `aosp/frameworks/base` with `git apply`) adds restricted signature spoofing
  to `ComputerEngine.generatePackageInfo`. Only these three packages can use it:
  - `com.google.android.gms`;
  - `com.google.android.gsf`;
  - `com.android.vending`.

  They also have to be preinstalled on a system partition, and they can report only the
  certificate in their own `fake-signature` meta-data. An app installed later can never
  spoof. The real Play Store carries Google's real signature and no such meta-data, so it
  is reported unchanged.

After first boot:

1. Open microG Settings and run its self-check.
2. Grant the location and notification permissions it asks for.
3. Add a Google account in microG.
4. Then open the Play Store.

Free apps install. Paid apps and in-app purchases may not work, because microG has no
billing. Apps that require Play Integrity (banking) refuse this device.

## Verification

See "Results" below. On the device:

```bash
adb shell getprop ro.dalvik.vm.native.bridge
```

```bash
adb shell cat /proc/sys/fs/binfmt_misc/arm64_dyn
```

Then install an arm64-only APK. Termux v0.118.3 `arm64-v8a` from its GitHub release was
used. Check that it starts, and that `logcat` shows `NativeBridge` loading
`libndk_translation.so`.

## Results

Cuttlefish, 2026-11-12, SELinux enforcing:

- Boot completes. Properties:
  - `ro.dalvik.vm.native.bridge` is `libndk_translation.so`;
  - `ro.product.cpu.abilist` is `x86_64,arm64-v8a`;
  - `/proc/sys/fs/binfmt_misc/arm64_dyn` is enabled with the ndk_translation runner as its
    interpreter.
- Termux v0.118.3 `arm64-v8a` (from its GitHub release, SHA-256 checked) installs and
  starts. Logcat shows `berberis: Initialized Berberis`, and its arm64 JNI library
  `libtermux-bootstrap.so` loads. Then an arm64 `bash` runs through binfmt_misc: the
  audit log shows `ndk_translation_program_runner_binfmt_misc_arm64 .../usr/bin/bash`.
  Termux's own bootstrap stops at `Unable to get realpath of id` (its multi-call
  binaries look at their real path). That is an app quirk under translation, not a
  loader failure.
- microG GmsCore 0.3.17 and the Play Store 50.7.34 are installed and enabled, both in
  `/product/priv-app`. The Play Store opens to its "Sign in" screen. No crashes.
- AVC denials: none from the translator, microG or the Play Store. The remaining ones are
  known and not ours: `system_server` reading the sysfs `country` file, and
  mediaprovider on `/data/local/tmp`, from the test's screenshot.
- Gotcha: do not boot Cuttlefish with `MEM_MB` above 4096 on this host. Its ramoops
  region sits at guest-physical 0x150000000 (5.25 GB). With 6 GB of RAM, pstore overwrote
  live memory, and every process crashed at random (system_server SIGSEGV loop,
  `Scudo ERROR: corrupted chunk header`).

Real PC (Z790, `out/aosp17-usb-f9.img`, logs in `out/pclog-f9/boot-2`), 2026-11-12:

- TikTok 47.7.3 (arm64-only, installed with Aurora Store) now starts. The
  `EM_AARCH64` errors are gone: 0, against every launch before F9. Its splash, sign-up
  and web (Spark) screens come up.
- One native crash in TikTok's video player. `libttmplayer.so` memcpy's a frame into a
  gralloc buffer that is mapped read-only (`r--` on `/dev/dri/renderD128`, the
  Cuttlefish minigbm gralloc), giving SIGSEGV `SEGV_ACCERR`. The next launches ran on.
  This is a gralloc mapping issue (follow-up), not a translation failure.
- "Sign in with Google" from TikTok opens microG's sign-in (`SignInHubActivity`), which
  hit an input ANR once.
- microG Settings and the Play Store open. Aurora Store's account request to microG's
  `AskPackageOverrideActivity` is refused (not exported); this is expected for a
  third-party store.
