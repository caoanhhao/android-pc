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
  (`aosp-device-vendor/`, removed): `IBluetoothHci/default` over an HCI user channel to the
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

## Risks

- Disk space for AOSP (`/` is the limit; check free space between build steps).
- The host CPU is flaky under build load (see `docs/aosp-build.md`, "Flaky host").
- The RTX 4070 has no usable open driver, so display is the UEFI GOP framebuffer
  (`pcfb`) with software rendering.
