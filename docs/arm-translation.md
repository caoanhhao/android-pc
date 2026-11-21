# ARM64 apps and microG on aosp_pc_x86_64

The image is x86_64-only. Many apps (TikTok, most games) ship native code only for
`arm64-v8a`. Without a translator they crash at load time, for example
`libnxreword.so ... is for EM_AARCH64`. Phase F9 adds:

- an ARM64 → x86_64 native bridge (Google's `libndk_translation`);
- microG in place of Google Play services, preinstalled, and Google's real Play Store as a
  user-added extra (docs/google-extras.md).

## Which translator

| Option | Licence | arm64 on x86_64-only | Verdict |
|---|---|---|---|
| Berberis (`frameworks/libs/binary_translation`, in AOSP 17) | Apache 2.0 | No. Only the riscv64 guest is built. The arm64 directories are scaffolding: no decoder, interpreter or translator, and no `libberberis_arm64` module. | Not usable yet |
| Google `libndk_translation` | Proprietary. It ships in the Android Emulator system images, which are licensed for development use. | Yes. The API 37 "Google APIs" x86_64 image is x86_64-only with `arm64-v8a`, the same layout as ours. | **Chosen** |
| Intel `libhoudini` | Proprietary. Taken from ChromeOS/WSA images. | Yes, but older builds. It crashed (SIGSEGV) in TikTok worker threads in earlier tests. | Not used |

`libndk_translation` may not be redistributed. So:

- it is never committed and never built into the image;
- the user adds it to the stick with `tools/add-google-extras.sh`, which takes it from
  Google's emulator image on the user's own computer, and Android mounts it over
  placeholders at boot (docs/google-extras.md).

32-bit `armeabi-v7a` apps cannot run. The product has no 32-bit zygote (x86_64-only, the
same as Google's own x86_64-only desktop/emulator images). That would need the 32-bit
`vsoc_x86_64` board.

## Native bridge: how it is wired

Only the translator itself is proprietary. Everything else is built from AOSP source, the
way Google's own `x86_64_ndk_translation_support.mk` does it.

1. `tools/add-google-extras.sh` downloads `google_apis/x86_64-37.0_r08.zip` (2.1 GB).
   It checks the SHA-1 that Google publishes and pins in the script. It unpacks
   `system.img` (a GPT disk with `super` in partition 2, then `lpunpack`, then `debugfs`)
   and puts 23 files on the stick's ESP (`android/extras/system/`); `pcextras.sh` mounts
   them over the image's empty placeholders at boot:
   - `lib64/libndk_translation.so`, the translator;
   - `lib64/libproxy_arm64_to_x86_64_*.so` (21), host-side proxies for the NDK libraries;
   - `bin/ndk_translation_program_runner_binfmt_misc_arm64`, which runs arm64
     executables.
2. `aosp-device/ndk_translation.mk` (included by `aosp_pc_x86_64.mk`) does four things:
   - installs empty placeholders at those 23 paths in `/system`;
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
- `tools/add-google-extras.sh` takes the Play Store (Phonesky, `com.android.vending`,
  50.7.34) from the official emulator image `google_apis_playstore/x86_64-37.0_r08.zip`.
  It checks the SHA-1 of the zip. The image's `product` partition is EROFS:
  `fsck.erofs --extract`. It generates `privapp-permissions-phonesky.xml` from the image's
  own `privapp-permissions-google-p.xml` block. The Play Store is proprietary, like
  `libndk_translation`: never built into the image; the user adds it as an extra and
  Android mounts it at `/product/priv-app/Phonesky` at boot (docs/google-extras.md).
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
- `aosp-device/microg/microg.xml` (installed as `/product/etc/microg.xml`) turns on
  microG's device registration (checkin) and cloud messaging by default. The Play Store
  does not work without them (F9c below). It also turns on SafetyNet/DroidGuard, which
  the Play Store's Play Integrity service needs (F9n below). GmsCore reads this file as the defaults for its
  settings; a value the user sets in microG Settings still wins.

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
  This is a gralloc mapping issue, not a translation failure; fixed in F9b (below).
- "Sign in with Google" from TikTok opens microG's sign-in (`SignInHubActivity`), which
  hit an input ANR once.
- microG Settings and the Play Store open. Aurora Store's account request to microG's
  `AskPackageOverrideActivity` is refused (not exported); this is expected for a
  third-party store.

### F9b: TikTok video player crash (gralloc read-only mapping)

`out/pclog-f9/boot-2/tombstones/tombstone_08`: thread `VOutle2-V0`, guest `memcpy` from
`libttmplayer.so`, write fault in an `r--` mapping of `/dev/dri/renderD128` of size
0xd8000.

- **Which API.** The guest stack holds an `ANativeWindow_Buffer`
  `{576, 1024, stride 576, YV12, bits = fault address}`; 576 x 1024 x 1.5 = 0xd8000. So
  the player locked a Surface with `ANativeWindow_lock`, which is `Surface::lock`: it locks
  the back buffer with `SW_READ_OFTEN | SW_WRITE_OFTEN`. The native-bridge proxies are not
  involved: `ANativeWindow_lock` takes no usage bits, and the lock is made by the host
  (x86_64) `libgui`.
- **Why `r--`.** The Surface's consumer is a `SurfaceTexture` in the same process
  (TextureView), drawn by ANGLE on SwiftShader Vulkan. SwiftShader imports each buffer as
  `VkDeviceMemory` and keeps it locked for the memory's lifetime, with
  `CPU_READ` only for a sampled image (`VkDeviceMemoryExternalAndroid.cpp`). minigbm's
  `cros_gralloc_buffer::lock` hands every later lock of a buffer that is already locked
  the first lock's mapping, whatever its flags. With the virtio-gpu 2D backend
  (`drv_dumb_bo_map`), a read-only lock maps `PROT_READ`. So the producer's write lock got
  SwiftShader's `r--` mapping. The tombstone shows every video buffer mapped twice, once
  `r--` and once `rw-`.
- **Fix.** `tools/patches/aosp17-minigbm-rw-lock.patch` (apply in `aosp/external/minigbm`
  with `git apply`): `cros_gralloc_driver::lock` maps a buffer allocated with a
  `SW_WRITE` usage read-write for every CPU lock. Then the shared mapping is always
  writable. Buffers without CPU write usage are unchanged. It is built into the
  `com.google.cf.gralloc` APEX (vendor image).
- **Verification.** A small x86_64 NDK test locks one `CPU_READ_OFTEN | CPU_WRITE_OFTEN`
  AHardwareBuffer for `CPU_READ`, keeps it locked (as SwiftShader does), locks it again
  for `CPU_WRITE`, and writes. Cuttlefish (`guest_swiftshader`, the same virtio-gpu 2D
  minigbm path as the real PC), 2026-11-13: before the patch, both locks return the same
  address and the write dies with SIGSEGV (exit 139); after it, the write succeeds and
  the read mapping sees the data. The patched boot reaches the launcher with
  SurfaceFlinger up and no new tombstones (only the known `dlkm_loader` abort). The USB
  image with the fix is `out/aosp17-usb-f9b.img`; TikTok on the real PC is not retested yet.

### F9c: Play Store does not open past its first screen (microG checkin off)

Real PC, `out/pclog-f9/boot-2/logcat.txt`: the Play Store shows `AssetBrowserActivity`
several times with no crash and no ANR, but never gets further. A Google account was
already added in microG (`GmsAuthenticator: getAuthToken: Account {...type=com.google}`),
and the Play Store does bind to microG's services. The cause:

- `GmsCheckinTrigger: Ignoring Intent { act=...BOOT_COMPLETED ... }: checkin is disabled`
  and `GmsGcmTrigger: ... gcm is disabled`. microG 0.3.x ships with device registration
  and cloud messaging off.
- With no checkin there is no Android ID. The Play Store logs
  `Unexpected android-id = 0` and `IllegalStateException: Cannot sync with 0 Android ID`
  about a hundred times, cannot load its pages, and GCM registration fails with
  `RuntimeException: Checkin disabled`.

Signature spoofing, permissions and the GSF provider were not the problem.

- **Fix.** microG's `SettingsProvider.getSettingsBoolean` reads its settings from the
  user's preferences first, then from default files, then from the hard-coded default
  (`false` for both). For an APK in `/product` the default files are
  `/product/etc/com.google.android.gms/settings.xml` and `/product/etc/microg.xml`
  (`gatherDefaultPreferences`, found with `dexdump` in GmsCore 0.3.17). So
  `aosp-device/microg/microg.xml`, a SharedPreferences file with `checkin_enable_service`
  and `gcm_enable_mcs_service` set to `true`, goes to `/product/etc/microg.xml`
  (`aosp-device/microg/Android.bp`, `aosp-device/microg.mk`). A device that already ran
  needs no data wipe, as long as the user never switched these options off in microG
  Settings: the default applies on the next boot.
- **Verification.** Cuttlefish, 2026-11-13. Cuttlefish here runs without network
  (`--enable_tap_devices=false`), so for this test a QEMU wrapper added a user-mode NIC
  (`-netdev user,id=pcnet -device virtio-net-pci,netdev=pcnet`). Cuttlefish renames that
  NIC to `buried_eth0`, so it was renamed back to `eth0` with `adb root` + `ip link`.
  Then, with no settings touched:
  - microG checks in (`gservices.db` `android_id` 4333952229198825412) and logs in to
    `mtalk.google.com:5228` (`GmsGcmMcsSvc: Logged in`);
  - the Play Store logs no `android-id = 0` at all;
  - Play Store "Sign in" leads to microG's "An app on your device is trying to sign in"
    dialog, then to Google's own "Sign in / Email or phone" page;
  - no AVC denials from microG or the Play Store.
  
  Signing in needs the user's own Google password, so the step past the sign-in page
  (store pages loading with an account) is left to the user on the real PC.
- **Aurora Store.** Its request to microG's `AskPackageOverrideActivity` is still refused
  (the activity is not exported). Aurora's anonymous login does not need it; this does
  not affect the Play Store.
- **USB image.** `out/aosp17-usb-f9b.img` (built 2026-11-13 06:39 for F9b) already holds
  this `product.img`: its super partition contains the new `microg.xml`. It has both
  fixes.

### F9d: Play Store downloads crash; TikTok stops after login

Real PC, `out/aosp17-usb-f9b.img`, logs in `out/pclog-f9b/boot-1`, 2026-11-13. The user
reported that the Play Store cannot download TikTok, and that TikTok does not open after
login.

**Play Store downloads.** Each download (TikTok: `com.ss.android.ugc.trill`, request 21,
56 MB) crashes `com.android.vending:background` as soon as it starts. This happened 32
times in this boot, plus 3 times in `com.android.vending` (`VpaService`) and 8 times in
`:recovery_mode`:

```
Unable to start service com.google.android.finsky.downloadservice.DownloadService ...
Caused by: java.lang.SecurityException: Starting FGS with type systemExempted
  callerApp=...com.android.vending:background targetSDK=36 requires permissions: all of the
  permissions [FOREGROUND_SERVICE_SYSTEM_EXEMPTED] and any of the permissions
  [SCHEDULE_EXACT_ALARM, USE_EXACT_ALARM, android:activate_vpn] or System exempted
```

- `DownloadService` and `VpaService` are foreground services of type `systemExempted`.
  Phonesky holds `FOREGROUND_SERVICE_SYSTEM_EXEMPTED`, but does not request
  `SCHEDULE_EXACT_ALARM` or `USE_EXACT_ALARM`. So it depends on the "System exempted"
  path, `ActiveServices.SystemExemptedFgsTypePermission`. That path accepts, among
  others, `REASON_SYSTEM_ALLOW_LISTED`: the app is on the system device-idle allowlist
  (`AppRestrictionController.isOnSystemDeviceIdleAllowlist`). This includes the
  except-idle list.
- The emulator image puts the Play Store on that list in
  `/product/etc/sysconfig/google.xml`
  (`<allow-in-power-save-except-idle package="com.android.vending"/>`). F9 took only the
  APK and its privapp permissions, so the Play Store was not on the list.
- **Fix.** `tools/fetch-apps.sh playstore` now also generates `sysconfig-phonesky.xml`.
  It takes every `package="com.android.vending"` line of the image's `google.xml`:
  - the power-save entry;
  - `app-link`;
  - `rollback-whitelisted-app`;
  - `automatic-rollback-denylisted-app`.

  The file goes to `/product/etc/sysconfig`. The script fails if the power-save entry is
  missing. The `update-ownership` lines are not copied: they name `com.google.android.gms`
  (microG here), and the Play Store must not become its update owner.
- In the same spirit, `aosp-device/microg/sysconfig-microg.xml` puts microG on the power
  save and data saver allowlists, as `google.xml` does for real GMS. This keeps GCM and
  checkin working in Doze and Data Saver.
- Run `tools/fetch-apps.sh playstore` again before the next build. `apps/Phonesky` is
  regenerated, and the sysconfig file comes with it.

**TikTok after login.** TikTok 47.7.3 here was installed with Aurora Store, because the
Play Store could not download it. The login itself works:
- `I18nSignUpActivity`;
- `AuthorizeActivity`;
- "Sign in with Google";
- microG's `AuthSignInActivity` returns.

Then TikTok opens `aabplugin...DFInstallBlankActivity` to load a dynamic feature module
through Play Core's `SplitInstallService`. The Play Store answers with
`PlayCoreAcquisitionActivity` ("Showing PlayCore acquisition dialog"), that is, "get this
app from Google Play". The Play Store serves split modules only to apps it installed
itself. After that, only TikTok's web view (`SparkActivity`) comes up, and the user
closed the task 20 s later. TikTok itself did not crash (no FATAL, no tombstone, no ANR).

- **Fix.** None is needed in TikTok or the translator. With the download fix, install
  TikTok from the Play Store. If the Aurora copy is installed, uninstall it first, so that
  `com.android.vending` becomes its installer of record.

**Vulkan crashes (`vulkan.pastel.so`).** Tombstones 01 and 02 are the Play Store's
`RenderThread`, two seconds apart, both while compiling a shader:
- an aborted `std::bad_function_call` in SPIRV-Tools `SSARewriter`;
- a `SEGV_ACCERR` in LLVM's `DominatorTree`, reading past the end of the thread's stack.

F-Droid crashed in the same way (LLVM `SelectionDAG`, NULL+0x38). On this PC,
`vulkan.pastel` (the SwiftShader-based CPU Vulkan from Cuttlefish's
`com.google.cf.vulkan` APEX) is the only Vulkan and the only GLES:
`egl=angle` runs ANGLE on top of it. The display is on the RTX 4070, which has no usable
open driver. The iGPU is not driven (no i915, see `docs/usb-boot-aosp17.md`). Hiding
the driver would leave HWUI with no GLES at all. Mesa is not an option without a GPU
kernel driver. So nothing was changed here.

The same boot also has other crashes:
- Aurora Store: SIGSEGV in JIT code;
- `dex2oat64` compiling TikTok: aborted with `Unexpected state_:-1287570968 for Class
  loader classes`, a Mutex word overwritten with junk.

Earlier PC boots have the same `dex2oat` abort on TikTok (f4f8-2, f4f8 boot-2, f9 boot-2),
a Scudo `corrupted chunk header` in Aurora's `RenderThread`, and launcher3 `RenderThread`
aborts. The Cuttlefish logs kept in `out/` (same host CPU, run as a VM) have none of
these. This is memory corruption in compiler and renderer threads, seen only on bare
metal. Two causes are possible:
- the i7-13700K, which already shows random compiler segfaults and `Bad page state`
  under build load on the host (`docs/aosp-build.md`, "Flaky host");
- the PC-only kernel path (`pcfb` dumb buffers through minigbm).

It is not a ramoops overlap: the stick has no ramoops region and uses `efi_pstore`. The
cause is still open. A `dex2oat` abort only leaves TikTok on JIT, and a crashed Play
Store renderer restarts. To tell the two causes apart on the PC, write 1 to
`/sys/devices/system/cpu/intel_pstate/no_turbo` (as root) and see whether the crashes
stop.

**Other findings, not fixed here.**
- `com.android.phone` is killed for a background ANR every 4 s (Cuttlefish telephony with
  no modem on the PC).
- Chromium's first-run sign-in throws `NullAccountManagerDelegate does not implement
  createAddAccountIntent`.

**Verification (F9d).** Build of 2026-11-13: `tools/fetch-apps.sh playstore` writes the
four `com.android.vending` entries listed above to `sysconfig-phonesky.xml`. `product.img`
(EROFS, `dump.erofs --ls --path=/etc/sysconfig`) contains `sysconfig-phonesky.xml` and
`sysconfig-microg.xml`.

Cuttlefish boot of the same images, 2026-11-13: `dumpsys deviceidle whitelist` shows
`system-excidle,com.android.vending`, plus `system-excidle` and `system` for
`com.google.android.gms`. There is no `Starting FGS with type systemExempted` error and no
AVC denial for either app. If `launch_cvd` fails in `extract-ikconfig` ("Cannot find kernel
config"), check that `/tmp` is not full: the script writes its temp files there.

A Play Store download needs a signed-in account, so it is left for the real PC.

### F9n: Where Winds Meet, Genshin Impact, Play Integrity

Real PC, `out/aosp17-usb-f9f.img`, logs in `out/pclog-f9f/boot-1`, 2026-11-13. The user
reported three games:
- Where Winds Meet (`com.netease.yyvn`) downloads from the Play Store but does not start;
- Genshin Impact does not show up in the Play Store;
- Delta Force (`com.garena.game.df`) fails Play Integrity (`requestIntegrityToken() failed`).

**Where Winds Meet: the install is fine, NetEase's packer crashes under translation.**
- The Play Store delivered every artifact of version 63832 (12:36–12:41): `base.apk`
  (33.5 MB), `.dm`, `config.arm64_v8a`, `config.en`, `config.hdpi`, and the install-time
  asset packs `yysls_it1` (2.59 GB) and `yysls_it2` (424 MB). The PackageInstaller session
  committed at 12:41:51 (`installation completed for package:com.netease.yyvn`). Nothing
  was truncated or missing.
- At launch, `ClassNotFoundException: androidx.core.app.CoreComponentFactory` and the
  five `Failed to find entry 'classes.dex'` lines are expected. The splits and asset packs
  carry no dex. `base.apk` holds only the stub of NetEase's packer: `da1vikvn` /
  `libunisec.so`, project `h72naxx2gb`. The real dex is encrypted in the asset
  `_ntcfgss.dat`. The packer decrypts it to `app_ntp0/3.9_63832/.unzip/classes.dex` and
  swaps the class loader later. The appComponentFactory error is only logged.
- The process dies right after that. It ran three times (12:41:55, 12:41:58, 12:42:18),
  and each time crashed with the same SIGSEGV:
  ```
  signal 11 (SEGV_ACCERR) Cause: trying to execute non-executable memory.
  #00 pc 13147c  .../lib/arm64/libunisec.so (offset 0x62000)
  #01 pc 0278e9  /apex/com.android.art/lib64/libartbase.so (art::ReadMagicAndReset+185)
  #02 libdexfile.so DexFileLoader::InitAndReadMagic ... DexFile_openDexFileNative
  #20 libndk_translation.so DoTrampoline_JNIEnv_CallStaticObjectMethodV
  ```
  `libunisec.so` ("initHookProcs start, running on art runtime") hooks ART by patching
  the host `libartbase.so` with an address in its own arm64 code. The goal is to catch
  `read()` when ART opens the decrypted dex. On an arm64 phone that works. Here,
  `libartbase.so` is x86_64 code run by the host CPU. It jumps into arm64 guest code
  that libndk_translation mapped non-executable, and the process faults at once.
- This cannot be fixed in the ROM. No native bridge supports an arm64 library that
  inline-hooks the host runtime, and libndk_translation is a proprietary binary.
  (H3 revised this: the hook is a GOT/PLT hook, not an inline patch, and
  `libpc_native_bridge` now carries both crossings; see "H3" below.) The
  game ships only an arm64 build on Play. It is not memory corruption on the PC: the
  crash is identical at the same PC (`0x13147c`) three times.
- Other games protected by NetEase's packer (`libunisec.so` / NTP) will fail the same
  way. So will other packers that hook `libart` from native code.

**Genshin Impact: the device profile looks right, and the filtering is on Google's side.**
On Cuttlefish with the same build (2026-11-13), `pm list features` and
`dumpsys SurfaceFlinger` show what the Play Store's device configuration is built from.
These are the values Genshin's published requirements check:

| Check | This ROM | Genshin needs |
|---|---|---|
| ABI | `ro.product.cpu.abilist` = `x86_64,arm64-v8a` | `arm64-v8a` (64-bit only) |
| OpenGL ES | `reqGlEsVersion=0x30001` (GLES 3.1, ANGLE on SwiftShader) | GLES 3.x |
| Vulkan | `vulkan.level=1`, `vulkan.version=4206592` (1.3), `vulkan.compute` (from the `com.google.cf.vulkan` APEX) | optional |
| Texture formats | `GL_KHR_texture_compression_astc_ldr`, ETC1/ETC2/EAC, S3TC/DXT, BPTC, RGTC | ASTC/ETC2 |
| RAM | `android.hardware.ram.normal` (the PC has much more than 4 GB) | 4 GB or more |
| Screen | large/xlarge layout, 240 dpi, landscape and portrait features | — |

- No `uses-feature` that a phone game asks for is missing.
- The extra Cuttlefish features (telephony, NFC, consumer IR) cannot hide an app; they
  only make more apps visible.
- The microG checkin (`GmsCheckinClient` request) sends the real build:
  - fingerprint `Android/aosp_pc_x86_64/pc_x86_64:17/.../userdebug/test-keys`;
  - device `pc_x86_64`, model `android-pc x86_64`;
  - `glEsVersion=196609`.
- What is left is Google Play's own device filtering:
  - **Device catalog.** The device is unknown to Google (no Play-certified build
    fingerprint, test-keys, `ro.product.cpu.abi=x86_64`). A developer can use the Play
    Console device catalog to exclude devices that fail Play Integrity, or whose primary
    ABI is x86.
  - **Same result on other x86 devices.** Genymotion (also x86_64 with ARM translation)
    reports the same thing: Genshin is "not compatible" from the Play Store and installs
    only from an APK site.
- The ROM cannot change this without posing as a certified device:
  - a copied Pixel fingerprint;
  - a microG device profile that claims another phone.

  This is out of scope, so we leave it alone.

  The user can try two things:
  - install Genshin through Aurora Store or from an APK, with all of its splits;
  - in microG Settings → Device registration, pick another device profile, at their own
    risk.

  Even when it installs, Genshin's anti-cheat may refuse translated or x86 devices. That
  was not tested.
- GLES stays at 3.1. Cuttlefish turns off ANGLE's `exposeES32ForTesting`
  (`ro.boot.hardware.angle_feature_overrides_disabled`), because SwiftShader's ES 3.2 is
  not conformant. Claiming 3.2 would let apps use features that then break.

**Play Integrity under microG.**
- The real Play Store serves the Integrity API (`ExpressIntegrityService`, and the
  classic `requestIntegrityToken`). For each verdict it runs a DroidGuard VM through
  `com.google.android.gms`, which is microG here.
- microG 0.3.17 ships with SafetyNet and DroidGuard **off** (`droidguard_enabled`,
  `safetynet_enabled` default `false`). In the f9f logs every DroidGuard handle failed:
  - `java.lang.IllegalAccessException: DroidGuard should not be available locally`
    (`NetworkHandleProxyFactory.createHandle`);
  - then `DGFallback: kotlin.NotImplementedError` (12:32:22).

  The Play Store then gave up:
  - `ExpressIntegrityException: Last device integrity refresh attempt failed`;
  - `StandardIntegrityException: -100`.

  The same failure hit the account sign-in page's DroidGuard step (`minute_maid`).
- **Fix.** `aosp-device/microg/microg.xml` now also sets `safetynet_enabled` and
  `droidguard_enabled` to `true`, the same as turning on "Google SafetyNet" in microG
  Settings. DroidGuard then runs Google's attestation VM locally in
  `com.google.android.gms.unstable`.
- microG keeps blocking hardware key attestation (`droidguard_block_hw_attestation=1`).
  This is microG's default; we did not change it.
- What to expect:
  - Integrity requests should now return a verdict instead of error -100.
  - The verdict will at best be `MEETS_BASIC_INTEGRITY`. `MEETS_DEVICE_INTEGRITY` and
    `MEETS_STRONG_INTEGRITY` need a Google-certified build fingerprint and a hardware-backed
    attestation key chained to Google's roots. This build has neither: test-keys, an
    unlocked userdebug build, and a Cuttlefish software KeyMint.
  - Delta Force and most online games that check integrity ask for device integrity, so
    they will still refuse.
  - Getting further would mean faking a certified device (fingerprint/keybox spoofing),
    which breaks Google's terms. We do not do it.
- Verification on Cuttlefish (2026-11-13; the file was pushed to `/product/etc` over
  `adb remount`, so no image was rebuilt). Before the push,
  `content query --uri content://com.google.android.gms.microg.settings/droidguard` showed
  `droidguard_enabled=0`, and the `safety-net` URI showed `safetynet_enabled=0`. After it,
  both show `1`, `droidguard_force_local_disabled=false`. The Cuttlefish VM has no
  Internet (DNS fails), so a real verdict needs the PC with a signed-in account. The change
  reaches the PC with the next `product.img`.

### F9p: TikTok white page after Google sign-in (WebView `Stream closed`)

Real PC, logs in `out/pclog-f9g/boot-2` (also `out/pclog-f9/boot-2` and
`out/pclog-f9b/boot-1`), 2026-11-13. After "Sign in with Google", TikTok opens
`com.bytedance.hybrid.spark.page.SparkActivity` and the page stays white.

**What the page is.** The Spark URL is
`https://inapp.tiktokv.com/ucenter_web/idv_inapp/verification?...&enter_from=suspicious_login&page=suspicious_login`:
TikTok's server flags the sign-in as suspicious and asks for an identity check. It does
this in all three PC boots that logged a sign-in (f9, f9b, f9g) and for both TikTok
packages (`com.zhiliaoapp.musically`, `com.ss.android.ugc.trill`).

**Why it is white.** The page's CSS/JS (`verification.*.js`, `lib-tux`, `lib-stable`,
`builder-runtime`, `convert-rem`, `hybrid-monitor-bootstrap`) come from TikTok's
`shouldInterceptRequest` (its Forest resource loader, Gecko channel
`tiktok_account_idv`). Each of them fails: WebView logs 16 `InputStreamUtil` errors per
boot (8 resources, `available()` then `read()`), all `IOException: Stream closed` from
`GZIPInputStream.ensureOpen`, under TikTok's own wrappers (`X.0Dal`/`X.1Hq3` in 47.x,
`X.0Fup`/`X.1So0` in the f9 build). The resources that come from Gecko's offline
package (`tiktok_webview_js_inject_manage`, `exist:true`) load fine; every
`tiktok_account_idv` resource is `exist:false` and fails 30-50 ms later, on the same
thread, before WebView has read a byte. No `Forest_TTNetDepender` line shows a network
fetch for them.

**Root cause: the stream is closed by TikTok before WebView gets it.** A `Stream closed`
from `GZIPInputStream.ensureOpen` means `close()` was called on that `GZIPInputStream`.
WebView calls `available()` first, on the thread that just ran `shouldInterceptRequest`,
so the close happened inside TikTok's code. WebView only reports it. Checked:
- Our WebView is not old or odd: `com.android.webview` 145.0.7632.218 (AOSP's prebuilt
  from `external/chromium-webview`), the same Chromium release as Google's WebView in the
  API 37 emulator image (`com.google.android.webview` 145.0.7632.218, code 763221809).
- The translation layer matches Google's: the arm64 guest libraries
  (`/system/lib64/arm64`, 56 files) and the `libproxy_arm64_to_x86_64_*` set are the same
  as the emulator's. TikTok's Java code runs on ART, not translated.
- `tools/webview-intercept-test/` serves a page whose scripts are `GZIPInputStream`s in
  `FilterInputStream` wrappers, as TikTok does, plus one stream closed before it is
  returned. On Cuttlefish (build `eng.builder.20261113.2206`) both providers give
  `RESULT gz=ok big=ok closed=failed`: open streams of any size load, and only the
  pre-closed one fails, with the same `available()` stack as TikTok's.

So no WebView provider can load these resources: the bytes are gone before WebView
reads. Why TikTok's `exist:false` path closes its stream could not be traced. That needs
TikTok's code, and no TikTok APK was available here (no copy on this host; the Aurora
anonymous download was not allowed in this session). The likely trigger is device
state: the same flow is reached only because the sign-in is "suspicious" (uncertified,
x86_64 device, microG), and TikTok picks the idv package path from server config.

**What changed in the ROM.** Google's Android System WebView is now the default
provider, as on a Google device, with AOSP's kept as a choice:
- `tools/fetch-apps.sh webview` takes `WebViewGoogle.apk` (standalone, 192 MB, x86_64
  only, no Trichrome library needed) from the same pinned emulator image as the Play
  Store, checks Google's WebView certificate (SHA-256 `6faf3c41...3d72ba6fe5`) and sets
  it up as a `/product` app. The emulator ships it gzipped behind a stub; here the full
  APK is installed. Like Phonesky, it is never committed.
- `aosp-device/webview/webview.mk` adds a `config_webview_packages` overlay only when
  the APK was fetched: Google's WebView first, `com.android.webview` second.
  WebViewUpdateService uses the first `availableByDefault` provider and does not fall
  back, which is why the overlay depends on the fetch. Both are system apps, so no
  `<signature>` is needed, and Play Store updates of `com.google.android.webview` keep
  working.
- To switch: Developer options > WebView implementation, or
  `cmd webviewupdate set-webview-implementation com.android.webview`.

Checked on Cuttlefish: `dumpsys webviewupdate` shows `com.google.android.webview` as
current and both packages valid; the intercept test passes on both; Chromium and the
WebView shell start (`cr_WVCFactoryProvider: version=145.0.7632.218 (763221809)`).
`product.img` grows by 192 MB (the system group still has about 4 GB free).

**Next on the PC.** After the update, sign in to TikTok again. If the page is still
white, the WebView is ruled out for good: compare with "AOSP WebView" in Developer
options, and look at what the idv page asks for in a browser
(`https://inapp.tiktokv.com/ucenter_web/idv_inapp/verification` needs the
`passport_ticket` from the log). Signing in by email or phone code instead of Google
avoids the Spark page only if TikTok does not flag that sign-in too.

**Build note.** The first build of this change failed in `system.img` with a path that
does not exist, `stdarch-verify/mip\x04-msa.h` (the file is `mips-msa.h`). Soong had
written the bad byte into `out/soong/build.aosp_pc_x86_64.7.ninja` and its incremental
cache `out/soong/ninja.db`, another sign of the flaky host (`docs/aosp-build.md`).
Deleting the ninja files is not enough: soong_ui then stops on the missing shards, and
an incremental run regenerates nothing. What worked: move `out/soong/*.db` (all five
incremental caches) away, change any `Android.bp`, and build again; that is one full
analysis (about 21 GB peak), after which the caches are consistent again.

### F9q: Delta Force dies 22 s after launch (guest jump to address 0)

Real PC, `out/aosp17-usb-f9g.img` installed with `update-usb-aosp17.sh` (user data kept),
logs in `out/pclog-f9g/boot-2` (joined logcat `out/pclog-f9g/b2-logcat.txt`), 2026-11-13.
Delta Force (`com.garena.game.df`, version 1.203.37119, Unreal Engine 4, Garena/Tencent
SDKs) started at 14:21:19 and died at 14:21:41.

**The tombstone (`tombstone_04`).**
- `signal 11 (SEGV_ACCERR), fault addr 0x0` in tid 4405 `GameThread`. The host stack is
  only the translator: `libc syscall` ← `berberis_HandleNoExec` ← `ExecuteGuest` ←
  `RunGuestThread`. libndk_translation found that the next guest PC is not executable
  guest code and raised the SIGSEGV itself; `SEGV_ACCERR` is its choice, not a real
  access fault.
- The guest (arm64) state of that thread, which the tombstone prints under "Guest thread
  information":
  - `pc 0`, `lr 0`, `x29 (fp) 0`. A call through a null function pointer (`blr xN`)
    would leave `lr` pointing back into the caller. Here the return address itself was 0:
    the code ran `ret` after loading `x29`/`x30` from a frame record that held zeros. The
    64 bytes around the guest `sp` (`0x735610af29c0`–`29ff`) are all zero.
  - The caller frames above it are intact: `x20` points at a frame record
    (`fp 0x735610af2c90`, `lr libUE4.so+0x10bc7e38`). `x0` and `x10` point at two
    function entries in `libUE4.so` (`+0x11333b04`, `+0x11333b40`), `x16`/`x17` (the
    linker's call scratch registers) into `libUE4.so` too. The thread was in the game's
    own code.
  - So one stack frame of `GameThread` was zeroed (or never set up) and the function
    returned through it.
- Other threads:
  - three anti-cheat threads are alive and idle in raw-syscall stubs: tid 4030 (`lr` in
    `libanogs.so`), 4013 and 4017 (`lr` in `libanort.so`); `pc` is in one-page anonymous
    code pages (`x8` = 101 `nanosleep`, 72 `pselect6`). `libanogs.so`/`libanort.so` are
    Tencent's ACE (the `TSS` GCloud plugin, loaded at 14:21:21);
  - `RenderThread`, the UE4 task-graph workers and a `vulkan` thread are idle.
  The anti-cheat runs under translation; its own code works.
- `libUE4.so`'s text is still one mapping (`r--`, 0–0x1be57000): nothing split it, so
  there is no sign that someone patched the engine's code at run time.
- `14:21:41.372 linker: libcrashlytics-trampoline.so is for EM_AARCH64` comes 1 ms
  *before* the fatal-signal line. It is a result, not the cause: Firebase Crashlytics'
  native handler catches the SIGSEGV first and runs its arm64 helper through the x86_64
  `linker64`, which cannot load it. Crashlytics crash reports from translated apps never
  work; that is harmless.

**What ran before (logcat, pid 3993).**
- 14:21:19 start, 14:21:21 `SplashActivity`, ANGLE on SwiftShader Vulkan (renderer init
  is fine), GCloud plugins (GPM, HTTPDNS, `TSS`, GCloud, TGPA), 14:21:25 INTL SDK init
  (remote config, `getsid` → `ret = 6`, a report-session error the game ignores),
  14:21:26 `GameActivity` drawn.
- 14:21:39 GVoice plugin, 14:21:41.14 Play asset-pack sync (`AssetPackService
  getSessionStates`, answered by the Play Store), 14:21:41.37 crash on `GameThread`.
- No anti-cheat, emulator, root or integrity message in between. The game did not ask for
  a Play Integrity token in this run. The INTL device info sent `"root_info":"test-keys"`
  and the INTL config has `"emulator": 2`, as in earlier runs.
- The previous run (f9f, `out/pclog-f9f/boot-1`, 12:35, same game build, same translator)
  went through the same sequence. After the asset-pack sync, `GameThread` went on to
  `GCloudDns init` (1 s later), GPM's VM checks (`/sys/bus/virtio exist`,
  `/proc/kallsyms contains vm info goldfish/qemu/kvm`), the GCloud version check and a
  resource download. It ran 6 minutes until the user closed it.
  What differs in f9g:
  - it was the second launch, with the half-finished download from the first one on disk;
  - the clicks now reach the game as touches (F9m; 14:21:38.9 and 14:21:39.6 on
    `GameActivity`, 1.8 s before the crash);
  - DroidGuard is on (F9n), which does not touch the game process.

**Cause: which of the suspects fits.**

| Suspect | Verdict |
|---|---|
| Translator limitation (unsupported instruction, self-modifying code) | Unlikely. An unknown instruction gives SIGILL with the guest PC in the library, not a return to 0. `libUE4.so`'s text was never remapped. The same code path ran fine in the f9f run. |
| Vulkan/GLES (renderer init on the software GPU) | No. ANGLE/SwiftShader came up at 14:21:21, `GameActivity` drew at 14:21:26, and the crash is on `GameThread`, not `RenderThread`/RHI. In f9f the same renderer ran for 6 minutes. |
| Play Integrity | No. This run made no integrity request; Integrity is checked by the server at login, which the game never reached. |
| Anti-cheat (ACE/TSS) kills the game on purpose | Possible, not proven. A return through a zeroed frame is a pattern anti-cheats use to crash without a usable backtrace, and ACE was loaded and running. Against it: the f9f run had the same root/emulator signs and was not killed; nothing logged a detection. |
| Game bug on this path (second launch with partly downloaded data, or touch input during loading) | Possible, not proven. A buffer cleared with the wrong size gives the same picture. |

The logs cannot tell the last two apart: the crashing function is in the stripped
`libUE4.so` (the APK is not on the build host), and the game's own log is not in logcat.
The pc-0 crash is not one of the "fixable in the ROM" cases:
- **libndk_translation settings.** The binary reads `ro.berberis.flags` (e.g.
  `accurate-sigsegv`) and a translation mode (`interpret-only`,
  `lite-translate-or-interpret`). None of them changes what guest code returns to. An
  interpreter-only run would make UE4 unusably slow. Nothing points at the translator, so
  no setting was changed.
- **Hiding userdebug/root signs.** The game reports `test-keys` and finds the VM (kallsyms
  symbols, virtio) in both runs, and the f9f run was not killed for it. Changing
  `ro.build.tags`/`ro.build.type` changes the build fingerprint. The Waydroid attempt on
  2026-11-07 showed that this breaks Android's own integrity check, and Garena then still
  said "avoid using emulators" at login. Not done.
- **GLES/Vulkan exposure.** Not involved (see the table).

**Fix made: `pclog` keeps the game's own log.**
`pclog.sh` copies, at each state dump, the last 1 MiB of the newest six Unreal logs
(`/data/data/*/files/UE4Game/*/*/Saved/Logs/*.log` and the same under
`/data/media/0/Android/data`) to `boot-N/applogs/PACKAGE-FILE`. The next time Delta Force
dies, its log should say what `GameThread` was doing (UE4 logs the loading steps, and a
detected cheat is usually logged before the kill). This needs a new `vendor.img` (f9-usb4
builds the next stick image).

**What the user can try on the PC (no rebuild needed).**
1. Launch Delta Force and do not click until the game's own UI is up. If it survives,
   touches during loading are the trigger. Then turn the conversion off for the game only
   over adb (`setprop persist.sys.pc.mouse_as_touch -com.garena.game.df`) and compare.
2. If it still dies at the same point, clear the game's data (Settings → Apps → Delta
   Force → Storage → Clear storage) so it runs the first-launch path that worked in f9f.
3. If it gets past loading, the next stop is login. Garena's login refused emulators in
   the 2026-11-07 Waydroid test, and the game asks for Play device integrity, which this
   build cannot give (F9n). Expect Delta Force to stay unplayable on this PC even after
   the crash is gone.

**Untrusted-touch drops in this boot (950).** All 950 are `Dropping untrusted touch event
due to /1000`, in bursts:

| Time | Drops | What was on screen |
|---|---|---|
| 14:21:20–25 | 255 | Delta Force starting: a held-button drag while only its starting window and the `SplashActivity` were up |
| 14:22:06, 14:22:47 | 4 | single clicks while the Play Store task was relaunching and a dim layer was fading out |
| 14:23:13–19 | 429 | two Play Store `market://` links opening `ResolverActivity` (each click landed during the 0.4 s open animation) |
| 14:24:03–14:25:07 | 262 | TikTok's activities switching (`NewUserJourneyActivity`, sign-in, `SparkActivity`) and desktop mode exiting at 14:24:49 |

In each burst `input_interaction` names no app window, only `PointerEventDispatcher0`:
the click fell on a window that was not taking input yet, as in F9m. There is no drop
between 14:21:26 and 14:21:41, while `GameActivity` was up. `input_interaction` shows
the clicks at 14:21:26.2, 14:21:38.9 and 14:21:39.6 going to
`com.garena.game.df/com.epicgames.ue4.GameActivity`, which is on F9m's list, so they
reached the game as one-finger touches. The drops are Android's tapjacking protection, and
each is logged once per `ACTION_MOVE` of a held drag. That is noise and does not keep
clicks from reaching the game. Nothing to fix.

### H2: TikTok's passkey button does nothing after Google sign-in (Credential Manager hang)

Real PC, build `eng.builder.20261114.2338`, logs in `out/pclog-g6/boot-4/logcat.txt`
(2026-11-14 17:35 and 17:44). After "Sign in with Google", TikTok opens the same identity
check page as in F9p (`inapp.tiktokv.com/ucenter_web/idv_inapp/verification`, Spark
channel `tiktok_account_idv`). With Google's WebView (F9p) the page now loads, offers a
passkey, and its button does nothing when clicked with the mouse.

**What the passkey UI is.** Not a WebView prompt and not Google Play services' own UI:
when the page loads, TikTok calls Credential Manager (`executeGetCredential` with
`callingPackage: com.ss.android.ugc.trill`) for an `androidx.credentials.TYPE_PUBLIC_KEY_CREDENTIAL`.
The button waits for that request. The system selector
(`com.android.credentialmanager`) is never started, and no activity of it appears in the log.

**Why nothing happens.** The providers are microG's:
- `RemoteService` (a system provider, there from boot) answers with only a *remote entry*,
  the "passkey from another device" (hybrid/QR) entry. `ProviderSession` keeps a remote
  entry only from the service named in `config_defaultCredentialManagerHybridService`,
  which is empty in AOSP, so it logs `Remote entry being dropped as it is not from the
  service configured by the OEM` and drops it. (`Could not parse hybrid service while
  filtering options` is the same empty setting seen earlier, in option filtering; it is a
  warning only.)
- `PasswordAndPasskeyService` (after the user turned it on in Settings at 17:43) has no
  passkey for `tiktok.com` and returns `EMPTY_RESPONSE`.

`GetRequestSession` then sees a provider in `CREDENTIALS_RECEIVED` and decides that the
UI is needed (`Provider status changed - ui invocation is needed`). But
`RequestSession.getProviderDataAndInitiateUi()` gets an empty list, since the only entry
was dropped, and in that case it does nothing: no UI, no error to the client. The
request stays open until TikTok cancels it (`Cancellation invoked from the client` when
the user leaves the page, `Client binder died` when TikTok is killed). The click goes to
TikTok's window (`input_interaction ... SparkActivity`), so input is fine; the page is
just waiting for an answer that never comes.

This is an AOSP bug that a Google device does not hit, because there Google Play
services is both the provider and the configured hybrid service, so its remote entry is
kept and the selector opens.

**Fix.** `tools/patches/aosp17-credman-empty-ui.patch` (apply in `aosp/frameworks/base`):
when every entry is dropped while the UI data is prepared, the session answers through
`onUiSelectorInvocationFailure()`, as the UI does when it cannot start. A get request
ends with `GetCredentialException.TYPE_NO_CREDENTIAL` ("No credentials available."), a
create request with `TYPE_NO_CREATE_OPTIONS`. TikTok then gets
`NoCredentialException`, as on a phone with no passkey, and the page can offer its
other verification methods.

Not done, on purpose: setting `config_defaultCredentialManagerHybridService` to
microG's `RemoteService`. That would keep the remote entry and open the selector with
"Use a passkey from another device", a QR code plus Bluetooth (caBLE) flow with a phone.
Whether microG's `RemoteService` can complete that flow was not checked, and a selector
whose only entry fails would be no better than the error. It can be tried later with a
one-line overlay if a passkey from a phone is wanted.

**Check without the PC.** `tools/credman-test/` is a small app built with the AOSP host
tools (`tools/credman-test/build.sh OUT.apk`). It has a provider that, like microG's
`RemoteService`, answers every get request with only a remote entry, and an activity
that asks for a passkey and logs how the request ends (`RESULT error=...`,
`RESULT response`, or `RESULT hang` after 15 s):

```
adb install credman-test.apk
adb shell settings put secure credential_service pc.test.credman/pc.test.credman.RemoteOnlyProvider
adb shell am start -n pc.test.credman/.MainActivity
adb logcat -s CredManTest CredentialManager
```

Result on Cuttlefish (`aosp_pc_x86_64`, build `eng.builder.20261115.0135`, which has the
patch), 2026-11-15. With only microG's providers, as on the PC, the log is the PC's
sequence up to `For ui, provider data size: 1`, then the patch's
`No provider data left for ui - responding with failure`, and the app logs
`RESULT error=android.credentials.GetCredentialException.TYPE_NO_CREDENTIAL` 1.2 s after
the request. With the test provider turned on, both remote entries are dropped and the
answer is the same. Without the patch the PC log stops at `For ui, provider data size`
and the request is answered only when the client cancels it.

Release image `out/aosp17-usb-h2.img`, build `eng.builder.20261115.0346` (main with H1, H2
and H4): in QEMU it boots to `sys.boot_completed` and the same test, with microG's providers
only, ends with `TYPE_NO_CREDENTIAL` 50 ms after the request. Update the stick in place with
`sudo tools/update-usb-aosp17.sh out/aosp17-usb-h2.img /dev/sdX`.

**Next on the PC.** Sign in to TikTok with Google again. The passkey step should now
fail at once instead of waiting, and the page should offer another way to verify (code
by email or SMS). If it still shows only a passkey, the page itself does not handle
`NotAllowedError`/`NoCredentialException`; then the way out is a passkey provider with a
real passkey, or signing in by email or phone instead of Google.

### H3: Where Winds Meet — arm64 code that PLT-hooks host libraries

Real PC, G6 stick (`eng.builder.20261114.2338`), logs in `out/pclog-g6/boot-4`, 2026-11-14.
Where Winds Meet (`com.netease.yyvn`) still crashed at start, 8 times, always the same way
as in F9n: `SIGSEGV (SEGV_ACCERR)`, "trying to execute non-executable memory",
`#00 pc 0x13147c libunisec.so`, `#01 art::ReadMagicAndReset+185` in the x86_64
`libartbase.so`. F9n concluded that this could not be fixed in the ROM. H3 looked closer
and found that it can.

**What libunisec.so does.**
- `libartbase.so`'s text is still one `r-x` mapping in the tombstone, so it was not patched
  in place. `ReadMagicAndReset+0xb5` is `callq read@plt`, and the faulting registers are
  `rdi = 0x62` (fd), `rsi` = a stack buffer, `rdx = 4`: ART was reading a dex file's magic.
  The PLT jumps through the `R_X86_64_JUMP_SLOT` of `read` (`.got.plt`, offset `0x5dd40`).
  libunisec found `libartbase.so` in `/proc/self/maps` (the translator shows host libraries
  there too), walked its dynamic section and wrote the address of its own arm64 function
  into that slot: a classic PLT/GOT hook (xhook/bhook style). Its purpose is to see the
  decrypted dex that the packer hands to ART.
- The bytes at the fault address, disassembled as arm64, are the hook itself:
  `read_hook(fd, buf, count)` saves its arguments, loads a function pointer from its own
  data (`libunisec.so+0x5e7cc8`) and calls it with the same arguments (`blr x8`). That
  pointer is the old slot value, the host libc `read`.
- So two calls cross between the host (x86_64) and the guest (arm64) without going through
  the bridge:
  1. host → guest: x86_64 code calls the arm64 hook. Guest code is mapped non-executable
     for the host CPU, so it faults at once (the crash we see);
  2. guest → host: the hook calls the host `read`. libndk_translation finds no guest code
     there and raises SIGSEGV (`berberis_HandleNoExec`); this would have been the next
     crash.

**Fix: `libpc_native_bridge.so` (`aosp-device/nbshim/`).**
libndk_translation stays untouched (it is a proprietary binary), but it exports the
berberis functions its own proxy libraries use. ART now loads a small native bridge of
ours (`ro.dalvik.vm.native.bridge=libpc_native_bridge.so`) that copies libndk_translation's
`NativeBridgeItf` (version 8), forwards every callback to it and adds a SIGSEGV handler
through `getSignalHandler`. ART installs that handler as a sigchain special handler, so it
runs after ART's own fault handler and before any app or guest handler.
- host → guest: the fault address equals `rip` and `GuestMapShadow::IsExecutable` says
  it is guest code. The handler asks `WrapGuestFunctionImpl` (with `RunGuestCall`) for a
  host-callable wrapper of that guest address, the same kind of wrapper the bridge builds
  for JNI functions, and resumes at the wrapper. The `call` already pushed its return
  address, so the wrapper runs as if it had been called directly. Wrappers are cached by
  the translator; the hook stays in the GOT (rewriting it could trip the packer's own
  checks), so each hooked call costs one handled SIGSEGV (a few microseconds).
- guest → host: the fault address is not `rip`, is not guest code and is the exact start of
  an exported host function (`dladdr`, `dli_saddr ==` address). The handler drops the
  translator's "no exec" cache entry for that address (`InvalidateGuestRange`) and
  registers a guest-callable trampoline there (`MakeTrampolineCallable`, as the bridge does
  for host `JNIEnv` functions). The translator then retries the jump and calls the host
  function. Each host function is wrapped once; a second fault at the same address is
  passed on (so a real bug still crashes instead of looping).
- Both directions use one generic signature: 8 integer and 8 floating-point arguments
  (x0–x7/d0–d7 ↔ rdi…r9 + stack/xmm0–7), integer result. That covers the libc-style
  functions such hooks wrap (`read`, `open`, `mmap`, `fstat`, …). Not carried over:
  floating-point results, structs passed or returned by value, more than 8 integer
  arguments, function-pointer arguments (they are not wrapped), and `errno` (a host call
  sets the host `errno`, which the guest does not see).
- `persist.sys.pc.nb_hooks=0` (over adb, then restart the app) turns the handler off; the
  bridge then behaves exactly as libndk_translation alone. `PC_NATIVE_BRIDGE=off` at build
  time still switches the whole bridge off.
- The handler logs the first 64 crossings per process under the tag `pc_native_bridge`.

**Test: `tools/guest-hook-test/`.** The game's APK is not on the build host and the real PC
has no adb, so the test app reproduces the hook: an arm64-only, libc-free JNI library
finds `libartbase.so` in `/proc/self/maps`, swaps the `read` JUMP_SLOT for its own arm64
function, which counts the call and calls the saved host `read`. The activity then opens
a `DexClassLoader` on its own APK, which goes through `ReadMagicAndReset` like the game's
packer does. `build.sh` builds it with the AOSP host tools.

**Results (build `eng.builder.20261115.0441`, 2026-11-15).**
- Cuttlefish, `persist.sys.pc.nb_hooks=0` (handler off): the test app crashes exactly like
  the game, `SIGSEGV (SEGV_ACCERR)`, "trying to execute non-executable memory",
  `#00 libguesthook.so`, `#01 art::ReadMagicAndReset+185` in `libartbase.so`.
- Cuttlefish, default (handler on): `PASS`, two `read` calls of the dex load go through the
  arm64 hook. `pc_native_bridge` logs one host → guest wrapper for the hook and one
  guest → host trampoline for libc `read`; the second call reuses both.
- VLC (arm64-only, `org.videolan.vlc` 13070206) starts to its onboarding screen with
  `libvlc.so` loaded through `libpc_native_bridge.so` + `libndk_translation.so`; no crash.
- The release image `out/aosp17-usb-h3.img` (H1+H2+H3; same AVB key, rollback index and
  security patch level as H2's) boots in QEMU (`tools/run-qemu-aosp17-usb.sh`, a copy made
  with the default userdata size: a 64 MiB userdata is too small for a first boot and
  system_server loops on SettingsProvider) to `sys.boot_completed` in 40 s, and the test
  app prints `PASS` there too.
- Build notes: `libnativebridge-headers` is visible to `//art` only and Soong forbids
  `include_dirs` into `art/`, so `pc_native_bridge.cpp` spells out the
  `NativeBridgeCallbacks` table itself. libndk_translation's table is 168 bytes, one entry
  longer than AOSP 17's header (20 function pointers); the shim copies all of it and
  checks `version == 8` before using it.

**What is still open.**
- The fix makes the crash path work; it does not prove the game runs to its menu. Next
  stops on the real PC are the packer's further checks (it may refuse an emulator), the
  game's Play Integrity request (F9n: no device integrity on this build) and rendering.
- Delta Force (`com.garena.game.df`) in the same boot is not this bug and the handler
  leaves it alone. All three of its tombstones come from `berberis_HandleNoExec` on a
  guest thread, with the guest jumping to guest memory that is not executable, never to a
  host function: `tombstone_04` to address 0 (as in F9q), `tombstone_08` to an anonymous
  `rw-` page (`+0x40`), `tombstone_12` into `libanogs.so` (Tencent ACE anti-cheat) at
  `+0x470434`, which the translator's executable map no longer lists as code. In `_08` and
  `_12` the signal is `SI_TKILL`: the game's own SIGSEGV handler (ACE) caught the first
  fault and raised it again. That points at the anti-cheat (code it generates or
  re-protects at run time) and stays open as in F9q.

### I3: Genshin Impact still missing from the Play Store

Real PC, image `out/aosp17-usb-h3.img` (build `eng.builder.20261115.0441`), logs in
`out/pclog-h3/boot-7` … `boot-11`, 2026-11-15. Where Winds Meet and Delta Force are listed
and install from the Play Store; Genshin Impact (`com.miHoYo.GenshinImpact`) does not show
up in search.

**Cause: Genshin requires `android.hardware.opengles.aep`, and no entry declares it.**
The Play build of version 7.2.0 (`versionCode 1258`, fetched from Google Play with
`apkeep` on 2026-11-15, see below) has, from `aapt2 dump badging`:

```
uses-feature: name='android.hardware.opengles.aep'                  (required)
uses-feature-not-required: android.hardware.touchscreen, .multitouch, .multitouch.distinct,
  bluetooth, camera, camera.autofocus, microphone, sensor.accelerometer
```

The OpenGL ES 3.1 Android Extension Pack (AEP) is a required feature, and Play hides an
app from search and refuses its install when the device lacks a required feature. This
ROM declares no `android.hardware.opengles.aep` (`pm list features` source files:
`handheld_core_hardware.xml` and the other `/vendor/etc/permissions` files of
`aosp_pc_x86_64`). Where Winds Meet and Delta Force do not ask for AEP, which is why they
are listed with the same device profile.

An earlier version of this section (first I3 commit) blamed Play Console device-catalog
filtering, before the manifest was available; that was wrong.

What else Play sees from this device (`state-300.txt` of boot-9 and the built permission
files) and does match:

| Item | Value on the PC | Matches Genshin? |
|---|---|---|
| ABIs | `ro.product.cpu.abilist=x86_64,arm64-v8a` (libndk_translation) | Yes: the arm64 split is served |
| GLES | `ro.opengles.version=196609` (3.1) on every entry | Yes, but AEP is missing |
| Touch | touchscreen features declared; Genshin marks them not required anyway | Yes |
| Screen | 2560x1080, 160 dpi (H4), `xlrg` | Yes: Genshin was already hidden at 240 dpi (F9n) |
| Device type | `ro.build.characteristics=default` | Yes |
| RAM | `android.hardware.ram.normal` | Yes |

**Why the ROM does not simply declare AEP.** The feature is a promise that every AEP
extension works: `GL_ANDROID_extension_pack_es31a`, which includes geometry and
tessellation shaders, ASTC LDR, sample shading, texture buffers, and the rest.
- On the SwiftShader entries (pcfb, nouveau KMS) ANGLE runs on SwiftShader Vulkan, which
  has no `geometryShader` or `tessellationShader`. ANGLE cannot expose AEP there, so
  declaring it would be a lie that breaks any game that relies on it.
- On the NVIDIA GPU entry (ANGLE on NVK) the hardware has all of it and ANGLE can expose
  ES 3.2, which contains AEP. `ro.opengles.version` there still says 3.1 on purpose
  (`docs/nvidia.md` on branch `gpu-alternatives`, G4: raise it per entry once
  dEQP-GLES31/32 passes on NVK). The NVK entry is on that branch now; on `main` the GPU entry
  is the only one, "Android 17" (ANGLE on vulkan.nvidia, `docs/nvidia-native.md`, L4).

So the device-side fix is AEP and ES 3.2 on the GPU entry only: a per-entry feature file
(for example bind-mounted by `init.pc.rc` on that entry before `system_server` starts) and
`ro.opengles.version=196610` there, after dEQP-GLES31 passes on NVK on the real PC. This
is not done in I3. Play also has to pick up the new feature list (a new device-config
upload; clearing the Play Store's cache after the update may be needed).

**Install without Play (the route taken for now).** `PackageManager` does not enforce
`uses-feature` at install time, so the Play files install on any entry. They were fetched
from Google Play's own servers (Google-signed delivery of HoYoverse's APKs; no mirror
site):
- tool: `apkeep` 1.1.0 from EFF's GitHub release, GPG signature checked (key
  `1073E74EB38BD6D19476CBF8EA9DBF9FB761A677`, William Budington, EFF);
- token: Aurora Store's anonymous dispenser (`auroraoss.com/api/auth`), not the user's
  account; Google Play ToS accepted by the user;
- files in a local download directory on the build host:
  `com.miHoYo.GenshinImpact.apk` (33 MB, base),
  `com.miHoYo.GenshinImpact.config.arm64_v8a.apk` (145 MB),
  `com.miHoYo.GenshinImpact.AssetBundles.apk` (514 MB, install-time asset pack); about
  30 GB more are downloaded by the game itself.

The three files are one app and must be installed in one session, from a machine with adb
connected to the PC (Wireless debugging, `docs/mouse-input.md`):

```
adb install-multiple com.miHoYo.GenshinImpact*.apk
```

or copy them to the PC (`adb push`, or a second USB drive as portable storage, F9i) and
install them together with a split installer such as SAI. The user's data partition on
the stick is never written from the build host (it is file-encrypted, and CLAUDE.md keeps
it out of reach).

Expected results:
- Play keeps the game hidden, and it is not updated from Play: the next version must be
  fetched the same way, or Aurora Store used on the PC.
- Use the NVIDIA GPU entry. Genshin asks for AEP features that SwiftShader lacks, so on
  the other entries it is likely to fail at its first GL context or shader compile.
- The Play build may refuse uncertified devices at login (community reports of error
  `31-4302` on Waydroid), and its anti-cheat may refuse translated or x86 devices. Not
  tested here: nothing was installed from this host.
- If it starts but mouse clicks do nothing, add it to `persist.sys.pc.mouse_as_touch`
  (`docs/mouse-input.md`).
