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
  inline-hooks the host runtime, and libndk_translation is a proprietary binary. The
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
