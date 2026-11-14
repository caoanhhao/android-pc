# android-pc — AOSP 17 on a real x86_64 PC, with HALs in Rust

## Goal

Run **AOSP 17 built from source** on a desktop PC (Gigabyte Z790, i7-13700K, see
`docs/usb-boot-aosp17.md`), booted from a USB stick on the stock **GKI android17-6.18**
kernel plus vendor modules built from source, with the PC's hardware (power, thermal,
Bluetooth, audio, camera, Wi-Fi) served by **vendor HALs written in Rust**. Cuttlefish
runs the same product for development and VTS.

## History: the Rust kernel (branch `aosp-16`)

Phases A–E wrote a Linux-ABI-compatible kernel in Rust (`kernel/`, booted by Limine in
QEMU/KVM and on the Z790) that ran unmodified **AOSP 16** userspace up to the launcher and
`adb install`, with drivers for virtio, NVMe/AHCI, xHCI, RTL8125, HD Audio, AX211 Wi-Fi and
Bluetooth, UVC and ACPI. That work, its test suites (`tools/run-qemu.sh`,
`tools/aosp-*-test.py`, `tests/`), the sample APK and the AOSP 16 notes were removed
from `main` on 2026-11-12 and live on the `aosp-16` branch. From AOSP 17 on, `main` only
covers the stock kernel.

## Repo layout

```
aosp-device/   device tree for aosp_pc_x86_64: product, libpchal, hals/<name>, kernel/ (vendor modules)
tools/         build, USB image and QEMU scripts (tools/limited-build.sh for every heavy build)
docs/          aosp-build.md, usb-boot-aosp17.md, rust-hals.md
aosp/          AOSP checkout (gitignored, very large)
gki/           GKI kernel checkout (gitignored)
```

## Phase F — AOSP 17 from source + Rust HALs

Goal: move the AOSP tree to **android-17.0.0_r1**, built from source, and add
our own **vendor HALs written in Rust** (AIDL, `rust_binary` in Soong) for the
PC hardware. Decisions (2026-11-10): upgrade `aosp/` **in
place** (only ~52 GB free on `/`, so no second tree); the Rust drivers are
**Android HALs in userspace**, not kernel modules.

Every step runs in its own sub-session. Builds stay **sequential** (one at a
time, `tools/limited-build.sh`, see CLAUDE.md), so the HAL steps after F2 can
be coded in parallel but must take turns building.

- **F1. AOSP 17 in place.** Delete `aosp/out/`, `repo init -b
  android-17.0.0_r1` with the same partial-clone flags, `repo sync -j4`,
  re-check the local fixes from `docs/aosp-build.md` (`RELEASE_USE_DEX_V41`,
  `trim.xml`), build `aosp_cf_x86_64_only_phone` image by image, boot it on
  Cuttlefish with the android17 GKI. Done when: `sys.boot_completed=1`, `adb
  shell` works, `docs/aosp-build.md` describes AOSP 17.
  **Done (2026-11-11):** `aosp_cf_x86_64_only_phone-cp2a-userdebug`
  (CP2A.260605.016) with GKI `6.18.16-android17` boots in ~40 s, built by
  `tools/aosp17-build.sh`. The DEX v41 fix is gone; `trim.xml` keeps
  `device/generic/car`; Soong needs a `GOMEMLIMIT` patch and siso takes no
  `-jN`; guest RAM must be 4 GB (ramoops at 0x150000000).
- **F2. Our product + Rust HAL scaffolding.** A device tree kept in this repo
  (`aosp-device/`, linked into `aosp/device/android-pc/pc_x86_64` by a local
  manifest or symlink) for `aosp_pc_x86_64` inheriting the Cuttlefish
  x86_64_only phone; a shared Rust crate (`libpchal`: logging, binder service
  registration, sysfs/devfs helpers); VINTF manifest fragments, init `.rc`,
  SELinux policy for HAL services; one trivial Rust HAL as proof. Done when:
  the product builds and the proof HAL is registered (`lshal`/`service list`).
  **Done (2026-11-11):** `aosp-device/` (copied into `aosp/device/android-pc/pc_x86_64`
  by `tools/aosp-device-link.sh`; symlinks leak into the image) defines
  `aosp_pc_x86_64-cp2a-userdebug` (`PRODUCT_DEVICE pc_x86_64`, own BoardConfig for
  vendor sepolicy), `libpchal`, and the `hals/<name>/` pattern (hal.mk, rc, VINTF
  fragment, sepolicy via `pc_hal_service`). Proof HAL: Rust `android.hardware.light`
  over `/sys/class/leds` + `/sys/class/backlight`, replacing Cuttlefish's lights HAL;
  boots, registered, VINTF clean. `tools/aosp17-build.sh` takes `PRODUCT`; see
  `docs/rust-hals.md`.
- **F3. Kernel for AOSP 17 on the real PC** (decision 2026-11-11: the stock
  kernel, F3a; the Rust-kernel option F3b was dropped on 2026-11-12 with the
  move of the Rust kernel to the `aosp-16` branch).
  - **F3a. Stock Linux kernel (GKI android17-6.18).** Build the GKI kernel
    and vendor modules from source for the PC hardware (NVMe,
    AHCI, xHCI/HID/usb-storage, simpledrm/i915, r8169/RTL8125, HD Audio,
    iwlwifi + btusb AX211, uvcvideo, ACPI/thermal/RTC), pack them into
    `aosp_pc_x86_64` (`vendor_dlkm`/`vendor_boot`), and boot AOSP 17 from a
    USB stick on the real PC. Done when: AOSP 17 reaches the launcher on the
    Z790 PC from USB with display, input, network; Cuttlefish still boots.
    *Done 2026-11-11:* on the Z790 PC from USB: launcher at 2560x1080, USB
    keyboard/mouse, Ethernet, Wi-Fi (AX211, Cuttlefish's Wi-Fi HAL), Bluetooth
    (AOSP's default HAL over an HCI user channel), HD Audio as card 0, webcam
    node, Chromium + F-Droid preinstalled; Cuttlefish still boots. Kernel 6.18.21
    GKI + vendor modules from source (`tools/gki-build.sh`), `pcfb` (UEFI
    framebuffer as DRM `virtio_gpu`), `pchvc` (placeholder Cuttlefish ports),
    stick `tools/mkusb-aosp17.sh`. SELinux enforcing and the webcam were checked
    in the combined F4–F8 test; still open: real sound use (F6). See
    `docs/usb-boot-aosp17.md`.
- **F4. Health + power + thermal HALs (Rust)** — `android.hardware.health`,
  `power`, `thermal` over the AC supply, thermal zones and cpufreq from E9.
  *Done on Cuttlefish 2026-11-11:* `hals/{health,thermal,power}` (health V5, thermal V3,
  power V7) replace Cuttlefish's HALs; SELinux enforcing with no AVC denials; VTS health
  25/25, thermal 9 + 2 skipped, power 10 + 13 skipped (no hint sessions); `libpchal` gained
  `Callbacks`, `uevent`, `procstat`. Kernel: `ACPI_AC`/`ACPI_BATTERY` modules. A first boot
  on the Z790 PC showed battery-less AC, CPU0/skin 74 °C, EPP switching; the two bugs it
  found (intel_pstate limits not writable, ACPI zones loaded after the HAL) are fixed but
  not yet re-checked there. The real-PC check moves to the combined F4–F8 USB test. See
  `docs/rust-hals.md`.
- **F5. Bluetooth HAL (Rust)** — `android.hardware.bluetooth` H4 over
  `/dev/hvc5` (E7), replacing the C++ default HAL.
  *Done on Cuttlefish/QEMU 2026-11-11:* `hals/bluetooth` replaces the interim C++ HAL
  (built from `aosp-device-vendor/`, now removed; that folder keeps only `fmq`): `IBluetoothHci/default` over an HCI user channel to the
  kernel's `hci0` on the PC, or H4 on a UART (`--h4 /dev/hvc5`, Cuttlefish's rootcanal) for
  VTS. Cuttlefish enforcing: `VtsHalBluetoothTargetTest` 16/16, the stack turns ON on it,
  no AVC denials; QEMU USB boot (no controller): the HAL reports `UNABLE_TO_OPEN_INTERFACE`
  after 3 s, the stack gives up after 6 restarts (state OFF). `libpchal` gained `poll`. The
  real-PC check (AX211 scan/pairing) is part of the combined F4–F8 USB test. See
  `docs/rust-hals.md`.
- **F6. Audio HAL (Rust)** — AIDL `android.hardware.audio.core` over the ALSA
  PCM/control ABI (E5, E8 microphone).
  *Done on Cuttlefish/QEMU 2026-11-12:* `hals/audio` serves the primary module
  (`IModule/default`, core V4) over the built-in codec's ALSA PCMs (FMQ streams through
  `libfmq_rust`), unmutes the codec's mixer at start; AOSP's default HAL APEX keeps IConfig,
  r_submix, bluetooth and the effects. SELinux enforcing, no AVC denials;
  `VtsHalAudioCoreTargetTest` (`IModule/default`) 148 passed + 73 skipped, 0 failed on
  Cuttlefish (`snd-aloop` card) and on QEMU (`intel-hda` + `hda-duplex`); playback and
  capture through AudioFlinger checked with `tools/audio-check.sh`. HDMI and USB audio are
  not routed yet. The real-PC check (ALC897 line out/mic) is part of the combined F4–F8 USB
  test. See `docs/rust-hals.md`.
- **F7. Camera HAL (Rust)** — AIDL camera provider over V4L2 UVC
  `/dev/video0` (E8), so the stock Camera app works.
  *Done on Cuttlefish 2026-11-12:* `hals/camera` serves `ICameraProvider/pc/0`
  (provider V4): each V4L2 capture node with YUYV/MJPEG becomes an `EXTERNAL` camera `100+N`
  with YUV and JPEG outputs, hotplug through uevents; metadata codec, V4L2, image conversion
  and gralloc (`AIMapper`) in Rust, JPEG through a small C layer over libjpeg. Cuttlefish
  enforcing (`vivid` webcam node): `VtsAidlHalCameraProvider_TargetTest` 45/45 after two
  fixes (DRM render node access for the minigbm mapper; YUV before BLOB in the stream
  configurations), no AVC denials, cameraserver lists the camera; device unit test 4/4,
  host tests 26/26. Cuttlefish's emulated/C++ provider APEX stays as the fallback. The
  Camera app photo on the real PC is part of the combined F4–F8 USB test. See
  `docs/rust-hals.md`.
- **F8. Wi-Fi HAL (Rust)** — minimal `android.hardware.wifi` for nl80211
  (E6), next to wpa_supplicant.
  *Done on Cuttlefish 2026-11-12:* `hals/wifi` serves `IWifi/default` (V4)
  for the chip behind `wlan0` over nl80211: chip modes from the wiphy's interface
  combinations, AP interfaces created/deleted with `NL80211_CMD_NEW/DEL_INTERFACE`, MAC
  addresses and driver info through ioctls, country code, usable channels; the optional
  vendor features are `ERROR_NOT_SUPPORTED`. The AIDL Rust bindings are generated in
  `hals/wifi` (AOSP has no Rust backend for `android.hardware.wifi`). Builds with
  clippy/rustfmt clean, `check-vintf-all` passes, host tests 25/25 (one read-only against
  the PC's own AX211). Fallback: `PC_WIFI_HAL=legacy` installs Cuttlefish's Wi-Fi APEX
  (AOSP's C++ HAL) instead. Cuttlefish enforcing (hwsim): `VtsHalWifi*` (IWifi, chip,
  STA, AP) 52 passed + 38 skipped, 0 failed, no AVC denials; Wi-Fi turns on, `wlan0` joins a
  framework SoftAP on `wlan1` (WPA2, DHCP). The real-PC check (AX211) is part of the
  combined F4–F8 USB test. See `docs/rust-hals.md`.
- **F9. ARM64 apps + Google services** — the real-PC test found that ARM64-only apps
  (TikTok) crash, because there is no translator. Users also wanted Play services and the
  Play Store.
  *Done on Cuttlefish 2026-11-12:* Google's `libndk_translation` native bridge. Berberis in
  AOSP 17 has no arm64 guest yet, and libhoudini is older and crashed TikTok. Only the
  translator, its proxies and the binfmt runner are fetched from the API 37 emulator
  image by `tools/fetch-ndk-translation.sh`; they are proprietary, never committed, and
  for personal use only. The arm64 guest libraries and config files are built from AOSP.
  `/system` is now generated by Soong instead of Cuttlefish's fixed shared image.
  microG GmsCore (enabled, with a restricted signature-spoofing patch to
  `frameworks/base`) and Google's Play Store, taken from the emulator image, are
  privileged apps on `/product`. arm64-only Termux starts and its arm64 JNI and
  executables run; the Play Store opens. Enforcing, no new denials. 32-bit ARM is not
  possible (x86_64-only). TikTok on the real PC is still to be checked. See
  `docs/arm-translation.md`.
  *F9d (2026-11-13, real-PC f9b logs):* Play Store downloads crashed: `DownloadService` is a
  `systemExempted` foreground service, allowed only for apps on the system power-save
  allowlist. `fetch-apps.sh playstore` now ships the Play Store's `google.xml` sysconfig
  entries, and microG gets its own. TikTok "not opening after login" was the Play Store
  refusing a dynamic feature module to an app Aurora installed. With working downloads,
  install TikTok from the Play Store. Open: crashes in shader and `dex2oat` compiler
  threads seen only on bare metal (SwiftShader `vulkan.pastel`, which is the only GLES/Vulkan
  on this PC). The cause is the CPU or the `pcfb` path, not settled yet.
  *F9e (2026-11-13, real-PC f9b logs):* Wi-Fi did not connect while Ethernet was plugged
  in: ConnectivityService ranks Ethernet above Wi-Fi, so Wi-Fi got no network request and
  `ClientModeImpl` dropped every connection. A SettingsProvider overlay turns on
  `wifi_always_requested` (a background Wi-Fi request), so Wi-Fi connects next to Ethernet.
  The Rust HAL's usable channels are now frequencies in MHz (the framework had found no band).
  Checked on Cuttlefish with Ethernet + hwsim. See `docs/usb-boot-aosp17.md` §7.
  *F9f (2026-11-13):* desktop mode and a fixed 2560x1080 landscape display. AOSP 17's own
  desktop windowing on the primary display (`aosp-device/desktop/`): freeform feature,
  `config_isDesktopModeSupported`/`config_canInternalDisplayHostDesktops` overlay, a
  `/vendor/etc/display_settings.xml` that starts the display freeform with rotation locked at
  0, and `persist.wm.debug.force_desktop_first_on_default_display_for_testing`, because WM
  Shell otherwise keeps a display without a touchpad touch-first (fullscreen). Apps open as
  windows with caption bars over Launcher3 and its taskbar; no third-party launcher.
  `tools/mkusb-aosp17.sh` now sets the GOP mode to 2560x1080 by default (`--resolution
  auto` for the EDID mode). Checked on Cuttlefish at 2560x1080; the real PC is part of the
  next USB test. See `docs/desktop-mode.md`.
  *F9i (2026-11-13):* storage. Large games did not install because the F9d image had the
  4 GiB default `userdata`; release images are built with `tools/mkusb-aosp17.sh --disk
  /dev/sda`, which makes `userdata` fill the stick (48.6 GiB), and `--disk` now refuses a
  non-USB or mounted disk. Extra SSDs on USB, SATA or NVMe can be set up as portable or
  adopted ("internal") storage through Android's own wizard: `voldmanaged` entries in an
  `/odm/etc` fstab, and a vold patch whose PC disk guard ignores `nvme0n1`, the boot stick
  and any disk that is not blank or holds partitions Android does not make. The patch
  also fixes an AOSP vold bug that took the wrong partition on NVMe (adopting a second NVMe
  disk formatted the first disk's partition). Checked in QEMU with NVMe, SATA and USB test
  disks; the real PC is part of the next USB test. See `docs/usb-boot-aosp17.md` §9.
  *USB image `out/aosp17-usb-f9d.img` (2026-11-13, 12.7 GiB, `tools/mkusb-aosp17.sh --menu`,
  2560x1080):* holds F9b–F9f (main at 0cabdba). Checked in the image's super partition:
  `sysconfig-phonesky.xml`/`sysconfig-microg.xml` on `/product`, the desktop-mode
  `display_settings.xml`, freeform feature and framework RRO, the Wi-Fi SettingsProvider
  overlay and the rebuilt Rust Wi-Fi HAL on `/vendor`, the `force_desktop_first` product
  prop; Limine entries at 2560x1080. QEMU smoke boot (`tools/run-qemu-aosp17-usb.sh`):
  boot completes, the display is freeform with rotation locked, `wifi_always_requested=1`,
  the Play Store and microG are on the deviceidle allowlist. Next: the real-PC test.
  *F9j (2026-11-13): Termux preinstalled.* `fetch-apps.sh termux` ships F-Droid's Termux
  0.118.3 (x86_64 bootstrap, no translation; F9's arm64 test build failed its bootstrap under
  translation) with Termux:API and Termux:Boot, pinned by SHA-256 and checked against the
  certificate F-Droid signs Termux with, so F-Droid updates install over them. They are
  plain (non-privileged) `/product` apps: updatable, can be disabled, not uninstalled.
  Termux's JNI libraries are compressed in its APK and a `/product` app's are never
  extracted, so `pc.mk` installs them beside the APK. Checked on Cuttlefish (enforcing):
  the bootstrap installs, `pkg install` fetches and runs x86_64 packages, no AVC denials.
  See `docs/usb-boot-aosp17.md` (Apps).
  *F9h (2026-11-13, real-PC f9d logs): HDMI audio.* No sound from the monitor (RTX 4070
  HDMI), onboard audio fine. The Rust audio HAL listed the HDMI PCMs but only exposed
  `Speaker`, and nothing reported a monitor to the framework. Now the HAL has an external
  `HDMI Out` port (connection `hdmi`, own `hdmi output` mix port) with
  `connectExternalDevice`, and `config_useDevInputEventForAudioJack` makes
  WiredAccessoryManager turn the HD Audio HDMI/DP jack switches into `DEVICE_OUT_HDMI`, so
  media moves to the monitor while one with audio is connected. The HAL picks the PCM by
  the jack kcontrols, logs the ELD state and switches the card's IEC958 outputs on;
  `persist.vendor.audio.hdmi` (`off`, or a PCM) overrides it. Caveat: the NVIDIA codec only
  attaches a PCM to a pin with a valid ELD, which the GPU driver writes; with the GOP
  framebuffer and no NVIDIA/nouveau driver the jacks likely stay off, and then no PCM can
  reach the monitor. pclog now records the ELD/jack state and beeps each HDMI PCM
  (`hdmi-check.txt`) to settle that on the next real-PC boot. Checked on Cuttlefish
  (`snd-aloop`, connection through the audio service): media moves to the HDMI port and
  back, the tone arrives on the chosen PCM, VTS `IModule/default` 158 passed / 0 failed,
  no AVC denials. See `docs/rust-hals.md` (F6, HDMI).
  *USB image `out/aosp17-usb-f9e.img` (2026-11-13, `tools/mkusb-aosp17.sh --disk /dev/sda
  --menu`, sized for the 57.3 GiB SanDisk stick, `userdata` 48.6 GiB):* holds F9h–F9j (main
  at 4a92ef3). Checked in the image's super partition (identical to the build's images):
  the HDMI audio HAL (`HDMI Out` port, `persist.vendor.audio.hdmi` in
  `vendor_property_contexts`), the framework RRO with `config_useDevInputEventForAudioJack`,
  `/odm/etc/fstab.cf.f2fs.hctr2` with the USB/SATA/NVMe `voldmanaged` entries, the patched
  vold (PC disk guard), and Termux, Termux:API and Termux:Boot on `/product` with Termux's
  x86_64 libraries. QEMU smoke boot: boot completes, `/data` is 49 GB, the three Termux
  packages are installed, vold logs `PC disk guard: ignoring sda (boot device)`, the audio
  HAL runs, no AVC denials. Write it with the two `dd` commands `mkusb` prints (sparse
  image). Next: the real-PC test (HDMI beep in `hdmi-check.txt`, adopting an SSD, Termux).
  *F9l (2026-11-13, real-PC f9e boot 1): ueventd recovery and a pclog size cap.* ueventd
  crashed right after coldboot (SIGSEGV in `libpcre2`, probably the PC's memory
  corruption); the restarted ueventd skipped coldboot, so `pcfb`'s `/dev/dri`, the ALSA
  PCMs and `/dev/video*` never appeared and the boot hung at the animation. That boot also
  filled the 256 MiB `pclog` partition, so the next one logged nothing.
  `aosp-device/pc/ueventd-watch.sh` (service `vendor.ueventd_watch`) watches ueventd. While
  it is down the script makes missing `/dev/block` nodes and dm links itself, so
  `mount_all` does not hang. Once ueventd is back it replays `add` for each device whose
  node is missing and restarts the HALs that ran without them. `pclog.sh` now writes
  dmesg/logcat in pieces and keeps the first and last ones (about 40 MiB a boot). It keeps
  5 boots and frees at least 48 MiB before each boot. Tested in QEMU (early kill through
  `androidboot.android_pc.test_ueventd_gap=1`, a kill after boot, a flooded and pre-filled
  `pclog`); see `docs/usb-boot-aosp17.md` §4 and §7 (F9l).
  *USB image `out/aosp17-usb-f9f.img` (2026-11-13, `--disk /dev/sda --menu`):* holds F9l
  on top of f9e. Its super partition matches the build. The QEMU smoke boot completes with
  no AVC denials. Writing it resets the `pclog` partition. Next: boot it on the PC.
  *F9m (2026-11-13, real-PC f9f boot 1): mouse clicks in games.* Delta Force ignored
  clicks; the log had 586 "Dropping untrusted touch event". Those were clicks during a
  splash screen (starting windows are not touchable, so the click fell to the Play Store
  under it; the splash is drawn by SystemUI's Shell, hence uid 10095) and on tasks that
  were closing (desktop-mode task layers, uid 1000): tapjacking protection, no stray
  overlay. After the game was drawn the clicks reached it as `SOURCE_MOUSE` and it did
  nothing: Unreal/Unity games read only touchscreen input. Fix: InputDispatcher sends mouse
  clicks to listed apps as one-finger touches (`tools/patches/aosp17-input-mouse-as-touch.patch`,
  built-in list of touch-only games, `persist.sys.pc.mouse_as_touch` to add or remove apps,
  `*` for all). Checked on Cuttlefish at 2560x1080 in desktop mode with VLC arm64
  (fullscreen, freeform, maximized; captions and taskbar stay mouse). TikTok's white
  screen after Google sign-in is not in the f9f log (TikTok was not started); the F9b case
  points at the missing Play-served split module. `pclog.sh` now saves a `snap-K/` dump
  (window/input/activity/WebView state + the screenshot) whenever the user takes a
  screenshot (Print Screen). See `docs/mouse-input.md`.
  *F9n (2026-11-13, real-PC f9f logs): Where Winds Meet, Genshin, Play Integrity.*
  Where Winds Meet installs completely: `base.apk`, the splits and the 3 GB of asset
  packs. It dies at launch because NetEase's packer (`libunisec.so`) hooks the host
  `libartbase.so` with arm64 code. Under libndk_translation that is a jump into
  non-executable guest memory (SIGSEGV in `ReadMagicAndReset`). It cannot be fixed in the
  ROM; the `CoreComponentFactory` ClassNotFound is only the packer stub.
  Genshin is missing from the Play Store because of Google Play's own filtering. The
  device side matches its needs: arm64-v8a in the abilist, GLES 3.1, Vulkan 1.3, ASTC/ETC2,
  RAM. The device is uncertified/unknown with an x86_64 primary ABI, and Genymotion shows
  the same result. Changing that would mean faking a certified device, which we do not do.
  Play Integrity failed outright because microG ships DroidGuard/SafetyNet off. `microg.xml`
  now turns both on (checked on Cuttlefish through the microG settings provider). Expect
  at best `MEETS_BASIC_INTEGRITY`; Delta Force-style device-integrity checks will still
  fail. Needs a new `product.img`. Details: `docs/arm-translation.md` (F9n).
  *F9o (2026-11-13): OS updates without a reflash.* Every fix so far meant a new stick
  image, and `dd` of a whole image wipes apps, games and accounts. `tools/update-usb-aosp17.sh
  NEW.img /dev/sdX` (sudo) now rewrites only the OS partitions (esp, boot/init_boot/
  vendor_boot, vbmeta*, super) and keeps userdata, metadata, misc, frp and pclog. It checks the
  layout, the AVB key and rollback index, the patch level and that the build number changed.
  `tools/aosp17-build.sh` sets a per-run BUILD_NUMBER (AOSP's default `eng.<user>` never
  changed the fingerprint, so Android would not rescan system apps). `mkusb-aosp17.sh` writes
  `android/build.txt` to the ESP. Tested in QEMU from f9f to a new build: the app, its
  CE data, PIN, files and settings survive. A/B OTA was rejected (needs `_b` partitions, a
  slot-aware bootloader and boot_control). Details: `docs/usb-boot-aosp17.md` §10.
  *USB image `out/aosp17-usb-f9g.img` (2026-11-13, `--disk /dev/sda --menu`):* holds F9m
  and F9n on top of f9f (build `eng.caoanh.20261113.2019`, already built after both commits,
  so nothing was rebuilt), with F9o's `android/build.txt` on the ESP. Its super partition
  matches the build. Checked in the build: `libinputflinger.so` has the mouse-as-touch code,
  `microg.xml` turns DroidGuard/SafetyNet on, `pclog.sh` is the F9m one. The QEMU smoke
  boot completes, and the same three are on the device. `pclog` writes `boot-1`, `/data` is
  49 GB, and there are no AVC denials. Write it with the two `dd` commands `mkusb` prints (this
  wipes Android data and `pclog`). Next: on the PC, rerun Delta Force, Where Winds Meet,
  the Genshin search and the TikTok login so `pclog` captures them.
  *F9p (2026-11-13, real-PC f9g boot 2): TikTok white page after Google sign-in.* The
  page is TikTok's identity check for a "suspicious" sign-in (`idv_inapp/verification`,
  in every PC boot that logged a sign-in). Its CSS/JS come from TikTok's
  `shouldInterceptRequest`, and each stream is already closed (`GZIPInputStream`, `Stream
  closed`) when WebView first calls `available()`: TikTok's own loader closes it on the
  `exist:false` Gecko path. WebView only reports it. Our WebView is the same Chromium
  145.0.7632.218 as Google's, the translator libraries match the emulator's, and a test app
  (`tools/webview-intercept-test/`) gives the same result on both providers. Still, Google's
  Android System WebView (from the pinned emulator image, `fetch-apps.sh webview`, not
  committed) is now the default provider with AOSP's as a choice in Developer options, to
  match a Google device. Needs a new `product.img` and `vendor.img`. Details:
  `docs/arm-translation.md` (F9p).
  *F9q (2026-11-13, real-PC f9g boot 2): Delta Force dies after 22 s.* The arm64 game
  thread returned to address 0 through a zeroed stack frame (`pc`, `lr` and `fp` all 0)
  in `libUE4.so` code, right after the Play asset-pack sync. libndk_translation then
  raised SIGSEGV (`berberis_HandleNoExec`). Ruled out: a translator gap (no SIGILL, engine
  code never remapped, the same path ran fine in f9f), the software GPU (renderer up,
  crash not on the render thread), Play Integrity (not asked for in this run). Left open:
  Tencent's ACE anti-cheat (`libanogs`/`libanort`, loaded and running) killing on purpose,
  or a game bug on the second launch or on touch input during loading. The f9f run had the
  same root/VM signs and ran 6 minutes. Nothing to change in the ROM for the crash itself.
  `pclog.sh` now copies the end of Unreal games' own logs (`boot-N/applogs/`) so the next
  crash shows what the game was doing. The 950 untrusted-touch drops are all clicks during
  window transitions; there are none while the game was up, and its clicks reached
  `GameActivity` as touches (F9m works). Even without the crash, Garena's login and the
  device-integrity check will most likely keep the game out. Details:
  `docs/arm-translation.md` (F9q).

  **F9 status: frozen (2026-11-13).** The F9 release is `out/aosp17-usb-f9.img`
  (`tools/mkusb-aosp17.sh --menu --userdata 64`, an update image for the flashed stick),
  build `eng.caoanh.20261113.2228` (main at 8c8638e, security patch 2026-06-05, AVB test
  key, rollback index 0). It holds F9a–F9q; the only change over the f9g stick is F9p
  (Google WebView as the default provider) and F9q (pclog keeps Unreal logs). Checks:
  `update-usb-aosp17.sh --dry-run` against the f9g stick image passes (layout, AVB key,
  patch level, new build number); the real update on a sparse copy of that image reads
  back OK and boots in QEMU with the new fingerprint, `com.google.android.webview`
  145.0.7632.218 as the WebView provider, Play Store, microG, Termux and Chromium
  installed, desktop-first on, 18 HAL processes up and 0 AVC denials (only the known
  QEMU tombstones: `dlkm_loader` at the end of its list, Bluetooth without a controller).
  The user installs it with `tools/update-usb-aosp17.sh` (docs/usb-boot-aosp17.md §10),
  which keeps apps, accounts and data.

  | | |
  |---|---|
  | Works on the real PC | Boot from USB at 2560x1080 in desktop mode (freeform windows, taskbar); keyboard and mouse, mouse clicks as touches for listed games (F9m); Ethernet, Wi-Fi next to Ethernet (F9e), Bluetooth, webcam, onboard audio; the nine Rust HALs (F4–F8); ARM64 apps through libndk_translation (VLC, Termux arm64 JNI, TikTok starts); microG with Google sign-in, Play Store downloads and installs (F9d), Play Integrity at most basic (F9n); Termux preinstalled (F9j); `userdata` filling the stick and extra SSDs as portable/adopted storage (F9i); ueventd recovery and capped pclog (F9l); OS updates that keep user data (F9o) |
  | Not fixable in the ROM | Where Winds Meet: NetEase's `libunisec.so` hooks ART with arm64 code, which cannot run under translation (F9n). Genshin Impact: hidden by Google Play's device filtering for an uncertified x86_64 device; faking a certified device is out of scope (F9n). Device-integrity checks (Play Integrity `MEETS_DEVICE_INTEGRITY`) |
  | Still open | Delta Force crashes 22 s in (zeroed stack frame in `libUE4.so`): Tencent ACE or a game bug, not the translator; the next PC run's `applogs/` should tell (F9q). TikTok's identity-check page stays white with either WebView provider (TikTok's own loader closes the streams, F9p); to be retried on the real PC with Google WebView. HDMI audio: no sound on the monitor, because the NVIDIA HDA codec gets no ELD without a GPU driver (F9h); it needs either the RTX 4070 driver stack (Phase G, G1) or the Intel iGPU through `i915` with the monitor on the board's port. The user has not chosen yet. Shader/`dex2oat` crashes seen only on bare metal (F9d) |

Order: F1 → F2 → F3a → F4–F8 (smoke-tested on Cuttlefish, then one combined USB test
on the real PC). The combined Cuttlefish round for F4–F8 ran on 2026-11-12: every HAL's VTS
passes (results table in `docs/rust-hals.md`), no AVC denials, no fallback needed.
The combined USB test on the real PC (Z790, 2026-11-12) ran all nine Rust HALs with 0 AVC
denials, and the SELinux enforcing entry boots normally: F4 (AC, ACPI + CPU zones, pstate
limits), F5 (scan, pairing with a phone), F7 (webcam preview and photo in the Camera app) and
F8 hotspot (after creating hostapd's data directory) work; F8 scans work, but a Wi-Fi
connection was not tried with Ethernet unplugged (the framework will not join Wi-Fi while
Ethernet is the validated default); F6 plays through the HAL, but the tone/mic check is not
confirmed yet. Chromium's black flicker (Graphite/Dawn on SwiftShader Vulkan) is fixed by
Chromium flags. No fallback kept: Cuttlefish's camera APEX is off on the PC, Wi-Fi runs
the Rust HAL. Details in `docs/usb-boot-aosp17.md` §7. ARM-only apps and Google services
are a separate task (no ARM64 native bridge in the image).

## Phase G — NVIDIA RTX 4070 driver stack

Goal: drive the PC's **RTX 4070 (AD104, `10de:2786`, HDMI audio `10de:22bc`)** from
Android instead of the UEFI GOP framebuffer (`pcfb`). That gives sound on the monitor
(HDMI/DP audio needs the GPU driver to light the link; see F9h), native modes and
refresh rates, more than one monitor, and in the end hardware GPU rendering instead of
SwiftShader. Decided 2026-11-13.

**Approach.** A GPU driver written from scratch is years of work, so the plan builds on
the open stacks that already support Ada, adds what Android is missing, and keeps a
Rust track:

| Layer | Choice | Why |
|---|---|---|
| Kernel | **nouveau with GSP-RM** (in `gki/common`, AD102-AD107 supported), vendor module | Mature modesetting and HDMI audio on Ada; GSP does clocks and power |
| Firmware | `nvidia/ad104/gsp/*-570.144` from linux-firmware into `/vendor/firmware` | Required by nouveau on Ada |
| Vulkan | **Mesa NVK** (`aosp/external/mesa3d`, Mesa 26.1; its NAK shader compiler is Rust) | The only open Vulkan for Ada |
| GLES | **ANGLE on Vulkan** (already in AOSP) | Android's own GLES-over-Vulkan path |
| Buffers / composer | minigbm + drm_hwcomposer (both in `aosp/external`) | Standard for KMS GPUs on Android |
| Rust track | **Nova** (`nova-core`, `drm/nova`, Rust, in the 6.18 tree) | NVIDIA's future open driver; experimental only |

Rejected: NVIDIA's open-gpu-kernel-modules plus the proprietary userspace (glibc-only,
no Android build). Still open as the alternative for HDMI audio only (the user has not
chosen, 2026-11-13): the Intel iGPU through `i915` (no GPU acceleration for games, needs
the monitor moved to the board's port).

**Testing.** QEMU and Cuttlefish cannot pass the 4070 through (the host's desktop runs
on it), so every G step ends on the **real PC**: the session builds, checks the image
in QEMU that it still boots on `pcfb` (nouveau must stay harmless without NVIDIA
hardware), then hands the user the `tools/update-usb-aosp17.sh` command (CLAUDE.md
USB rule) and reads the results from the pclog partition afterwards. Each step keeps
`pcfb` as the fallback: a boot property (`androidpc.gpu=pcfb|nouveau`, chosen from the
Limine menu) picks the path, so a broken GPU step never leaves the stick unbootable.

- **G1. nouveau + GSP on the GKI kernel (display and HDMI audio).** Enable
  `DRM_NOUVEAU` (and whatever helpers the GKI image lacks: TTM, display helpers,
  `drm_gpuvm`, `drm_exec`) as vendor modules in `tools/gki-build.sh`; ship the AD104
  GSP firmware (decompressed); load nouveau only on the `nouveau` boot entry, with
  `pcfb` not bound. pclog dumps `dmesg` (GSP boot), `/sys/class/drm/*/status`, EDID
  and the HDA ELD. Done when: on the real PC nouveau boots GSP, the monitor shows
  its connected mode in `modetest`, and the F9h Rust audio HAL plays a test tone
  through the monitor.
  **Status (2026-11-14): code done, real-PC test pending.** nouveau + 9 helper modules
  build against the unchanged GKI image, GSP-RM 570.144 ships in
  `/vendor/firmware/nvidia/ad104/gsp`, the Limine menu has an "NVIDIA nouveau" entry
  (`androidboot.android_pc.gpu=nouveau`, `pcfb.enable=0`, ranchu composer in no-op
  mode, `modetest -r` keeps the monitor lit), pclog dumps nouveau/GSP dmesg, connectors,
  EDID and the 10de:22bc ELD. QEMU: both entries boot, nouveau stays idle without NVIDIA
  hardware. Image `out/aosp17-usb-g1.img`; checklist and commands in `docs/nvidia.md`.
- **G2. Android display on KMS.** Composer = drm_hwcomposer on the nouveau card
  (client composition still on SwiftShader), gralloc = minigbm with dumb buffers (or
  its nouveau backend), native resolution and refresh rate, hotplug, a second
  monitor. Rust audio HAL follows HDMI/DP hotplug. VTS composer and gralloc subsets.
  Done when: the desktop runs on KMS at the monitor's native mode with sound, and
  the `pcfb` entry still works.
  **Status (2026-11-14): code done, real-PC test pending.** The composer is a per-entry
  vendor APEX choice on the command line. "NVIDIA nouveau" runs drm_hwcomposer (overlay
  planes off, SwiftShader client composition) with minigbm dumb buffers on nouveau's
  render node. minigbm drops DRM master (new patch). There are "VTS run" and
  "modetest only" (G1) entries, and pclog runs the VTS from the ESP. QEMU (vkms stands in
  for nouveau): pcfb unchanged; drm_hwcomposer drives the connector at its preferred mode;
  VTS allocator 11/11, mapper 63/63, composer 163/163 pass (skips aside). Image
  `out/aosp17-usb-g2.img`; checklist in `docs/nvidia.md`.
- **G3. NVK Vulkan on Android.** Build Mesa's NVK (and NAK) through Soong for
  x86_64 Android, gralloc interop (`AHardwareBuffer`, dma-buf modifiers that NVK
  and minigbm agree on), `vulkan.nouveau` as the system Vulkan HAL. Done when:
  `vulkaninfo` shows the RTX 4070 and dEQP-VK smoke groups (api, memory, wsi.android)
  pass on the real PC.
  **Status (2026-11-14): code done, real-PC test pending.** NVK, NAK (Rust) and NIL
  build through hand-written Soong files, since the meson2hermetic config has no nouveau:
  - Patches: `aosp17-mesa-nvk.patch`, `aosp17-cf-vulkan-nouveau.patch` and
    `aosp17-deqp-apk-64bit.patch`.
  - `nil.h` is hand-written (no cbindgen), with layout asserts on both the C and Rust
    sides.
  - NVK's CL kernels are built with the host clang's SPIR-V backend
    (`tools/mesa-nvkcl-spv.sh`).
  - A new u_gralloc backend reads minigbm handles, so both sides agree on
    `DRM_FORMAT_MOD_LINEAR`.
  - NVK gains `VK_ANDROID_external_memory_android_hardware_buffer` and a tiled shadow
    for linear swapchain images.
  - `vulkan.nouveau` ships in the `com.google.cf.vulkan` APEX.
  - `androidboot.hardware.vulkan` is now set per entry. A new entry, "NVIDIA nouveau, NVK
    Vulkan (test)", runs ANGLE and SurfaceFlinger on NVK too; "NVIDIA nouveau" keeps
    SwiftShader.
  - `pcvktest.sh` writes `cmd gpu vkjson` and the dEQP-VK smoke results to pclog.

  QEMU (no NVIDIA GPU): the driver loads and finds no device. pcfb and G2's nouveau entry
  still boot. Image `out/aosp17-usb-g3.img`; checklist in `docs/nvidia.md`.
- **G4. GLES through ANGLE, GPU composition.** ANGLE as the system GLES driver on
  NVK, SurfaceFlinger and HWC client composition on the GPU, SwiftShader removed
  from that boot entry. Done when: dEQP-GLES3 smoke groups pass, Chromium/WebView
  and the games from F9 (Delta Force, Genshin's renderer) run GPU-accelerated, and
  the frame rate beats the `pcfb` entry.
  **Status (2026-11-14): code done, real-PC test pending.** Android's GLES is already
  ANGLE on the system Vulkan driver. So G3's NVK entry, renamed **"Android 17, NVIDIA GPU
  (NVK + ANGLE)"**, runs everything on the GPU:
  - SurfaceFlinger's client composition for drm_hwcomposer;
  - HWUI, Chromium/WebView (Ganesh on ANGLE) and the games.

  libvulkan loads only `vulkan.nouveau` there, so SwiftShader is never loaded; pclog's
  `== swiftshader users` checks this. G4 adds:
  - `pcvktest.sh` runs the dEQP-GLES3 smoke groups after the VK ones (pclog `gles/`).
  - A new `pcgfxbench.sh` runs the same benchmark on every entry, so pcfb is the
    baseline:
    - the San Angeles demo;
    - hwuimacro scenes, off screen and on screen (pclog `bench/`).
  - SurfaceFlinger timestats are on at every boot, for the games' FPS.

  QEMU: the benchmark runs on pcfb and on the SwiftShader nouveau entry. The GPU entry
  needs the 4070 (no device in QEMU). Image `out/aosp17-usb-g4.img`; checklist in
  `docs/nvidia.md`.
- **G5. Power, thermal, stability.** GSP clocks under load, fan and temperature
  from nouveau's hwmon into the F4 Rust thermal HAL, suspend/resume, a long
  game-plus-video soak with no GPU faults in pclog. Done when: one hour of play
  shows no hangs and sane temperatures; then `nouveau` becomes the default entry.
  **Status (2026-11-14): code done, real-PC soak pending.** On GSP-RM, nouveau has no
  therm/clk subdev, so it showed no hwmon at all. A kernel patch
  (`tools/patches/gki-nouveau-gsp-thermal.patch`) adds two things:
  - the GPU temperature as `temp1_input`, read from the GSP-shared sensor register;
  - a debugfs `perf` file with GSP-RM's current P-state, read through RM control
    `PERF_GET_CURRENT_PSTATE`.

  The open headers do not have NVIDIA's clock-domain and fan RM controls, so the clocks
  are checked as P0 under load against P8 idle. The fan speed has no source. The Rust
  thermal HAL now does two things:
  - it gives GPU sensors that have no limits default thresholds: SEVERE at 90 °C;
  - it reports any hwmon `fanN_input` as a fan cooling device.

  Suspend: the nouveau entries stop Cuttlefish's `suspend_blocker`, so Android suspends
  (S3) when the screen is off. `pcwake.sh` lets keyboards wake the PC and turns the
  screen on after a wake that did not come from an alarm. pclog on the nouveau entries
  adds `gpu-perf.txt` (P-state and temperature every 5 s), a suspend self-test with an RTC
  wake (`suspend-test.txt`, once per build), and `== gpu faults` / `== suspend` sections.
  QEMU: S3 self-test, autosuspend and a keyboard-style wake all work on the nouveau entry
  (vkms). Image `out/aosp17-usb-g5.img`; soak checklist in `docs/nvidia.md`. The default
  Limine entry stays pcfb until the soak passes.
- **G6. Nova (Rust) experiment.** Build `NOVA_CORE` and `DRM_NOVA` as modules on
  a third boot entry, see how far they bring up the 4070 (GSP boot, VRAM), write
  down what works next to nouveau, and keep our patches upstream-shaped. Never
  the default until it can drive the display. Done when: a short report in
  `docs/nvidia.md` with logs from the real PC.

Order: G1 → G2 → G3 → G4 → G5, each in its own sub-session, one build at a time;
G6 after G5. `docs/nvidia.md` is the running log for the whole phase.

## Risks

- Disk space for AOSP (`/` is the limit; check free space between build steps).
- The host CPU is flaky under build load (see `docs/aosp-build.md`, "Flaky host").
- The RTX 4070 runs on the UEFI GOP framebuffer (`pcfb`) with software rendering
  until Phase G lands. nouveau on Ada is young, and every G test needs the real PC
  (no GPU passthrough on this host).
