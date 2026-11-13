# AOSP 17 from a USB stick on the real PC, stock GKI kernel (phase F3a)

Status (2026-11-11): **AOSP 17 runs on the real PC from the USB stick** with the stock
GKI kernel (section 7): launcher at the monitor's 2560x1080, USB keyboard and mouse,
Ethernet (RTL8125, DHCP), Wi-Fi (AX211), Bluetooth (AX211), HD Audio as ALSA card 0, the
webcam as `/dev/video*`, Chromium and F-Droid preinstalled. The same image boots in QEMU
the way the PC boots it (SELinux permissive and enforcing), and Cuttlefish still boots
`aosp_pc_x86_64` with the same kernel and images.

The target is the phase E PC: Gigabyte Z790 AORUS ELITE AX, i7-13700K, RTX 4070 (the
display; no open driver, so the UEFI GOP framebuffer is used), Realtek RTL8125B 2.5 GbE,
Realtek ALC897 HD Audio, Intel AX211 Wi-Fi + Bluetooth (CNVi, BT on USB `8087:0033`), a
UVC webcam.

## 1. Pieces

| Piece | Where | What |
|---|---|---|
| Kernel source | `gki/` (gitignored) | `repo` checkout of `common-android17-6.18-2026-06` (the branch of the F1 prebuilt GKI, now 6.18.21) |
| Build | `tools/gki-build.sh` | Kleaf builds through `tools/limited-build.sh`, installs into `aosp/device/android-pc/` |
| Vendor kernel package | `aosp-device/kernel/` | `BUILD.bazel` (`//android-pc`), `pc_x86_64.fragment`, `pcfb/`, `pchvc/` |
| Product glue | `aosp-device/pc/`, `aosp-device/BoardConfig.mk` | `init.pc.rc`, `pclog.sh`, firmware, sepolicy |
| Bluetooth HAL | `aosp-device/hals/bluetooth/` | our Rust HAL (F5) over an HCI user channel |
| Apps | `tools/fetch-apps.sh` | Chromium and F-Droid as prebuilt `/product` apps |
| Stick image | `tools/mkusb-aosp17.sh` | GPT image file with Limine, kernel, initrd and the Android partitions |
| QEMU test | `tools/run-qemu-aosp17-usb.sh` | boots the image as on the PC |
| Logs | `tools/pclog-read.sh` | copies the boot logs from the stick's `pclog` partition |

## 2. Kernel

### Source

```
mkdir gki && cd gki
repo init -u https://android.googlesource.com/kernel/manifest -b common-android17-6.18-2026-06 \
    --depth=1 --no-tags --partial-clone --clone-filter=blob:limit=10M
mkdir -p .repo/local_manifests && cp ../aosp-device/kernel/gki-trim.xml .repo/local_manifests/trim.xml
# (drops raviole, u-boot, GBL, fuchsia_sdk, ltp, ...)
repo sync -c -j4 --no-tags --no-clone-bundle --optimized-fetch --retry-fetches=5
```

About 28 GB (`prebuilts` 19 GB, `.repo` 6 GB); the build output adds ~10 GB under
`gki/out`. The F1 prebuilt (`6.18.16-android17-1-gb61cd7ae4209`, "26Q2-release") came
from this branch; its exact commit is not published, so the build uses the branch tip.

### Build: `tools/gki-build.sh`

Steps (`STEPS=...`, default all): `gki` → `//common:kernel_x86_64_dist` (the GKI image
and its 100 system_dlkm modules, the same set as the prebuilt), `virt` →
`//common-modules/virtual-device:virtual_device_x86_64_dist` (Cuttlefish's modules),
`pc` → `//android-pc:pc_x86_64_dist` (ours), `install`, `firmware`. Flags:

- `--notrim`: the GKI image exports every symbol, not only the KMI lists; the PC drivers
  (r8169, snd-hda, iwlwifi, ...) need symbols no symbol list has.
- `--lto=none`: the GKI build takes ~8 min and a few GB instead of a full LTO build.
- The Bazel server runs inside the `limited-build` scope (12 CPUs, 25 GB) and exits after
  5 idle minutes (`--max_idle_secs=300`).

`install` puts `kernel-6.18` (bzImage), `system_dlkm/*.ko` and `vendor/*.ko` into
`aosp/device/android-pc/kernel/6.18/`. `aosp-device/BoardConfig.mk` points Cuttlefish's
`SYSTEM_DLKM_SRC`, `KERNEL_MODULES_PATH` and `TARGET_KERNEL_PATH` there when it exists, so
the same kernel runs on Cuttlefish and on the PC (without it the prebuilts are used).

### Vendor modules (`aosp-device/kernel`)

`pc_x86_64` is a Kleaf mixed build on top of `//common:kernel_x86_64` with the two
virtual-device fragments **plus** `pc_x86_64.fragment`, so its config is a superset of
virtual-device's (the CRCs of the 20 884 symbols the two `Module.symvers` share match).
`install` takes virtual-device's modules, then **replaces** them with every module of the
PC build: equal CRCs are not enough, our options add exports. `IWLWIFI_LEDS` selects
`MAC80211_LEDS`, and with virtual-device's `mac80211` the real PC logged `iwlmvm:
Unresolved symbol __ieee80211_get_radio_led_name` and had no Wi-Fi. Only virtual-device's
external modules (goldfish, v4l2loopback, virtio-media) stay. `cfg80211`/`mac80211` are
also in the vendor ramdisk (first stage loads them), so after changing modules rebuild
`vendor_boot.img` too, not only `vendor_dlkm`.

Built into the GKI image already: NVMe, xHCI, usb-storage + UAS, USB HID + hid-generic,
evdev, uvcvideo, snd-usb-audio, ACPI button, RTC CMOS, intel_pstate, intel_idle, VFAT,
efi-pstore. The fragment adds, as modules:

| Hardware | Modules |
|---|---|
| SATA | `libata`, `libahci`, `ahci` |
| RTL8125 2.5 GbE | `r8169`, `realtek` (PHY) |
| e1000e (QEMU tests) | `e1000e` |
| HD Audio | `snd-hda-core`, `snd-hda-codec`, `snd-hda-intel`, `snd-hda-codec-generic`, the Realtek codecs (`snd-hda-codec-alc662` drives the ALC897) and the HDMI codecs (`-hdmi`, `-nvhdmi`, `-intelhdmi`, ...), `snd-ctl-led` |
| AX211 Wi-Fi | `iwlwifi`, `iwlmvm`, `cfg80211`, `mac80211` (with LED triggers) |
| AX211 Bluetooth | `btusb`, `btintel`, `btrtl` (virtual-device), `bluetooth` (GKI) |
| Sensors / ACPI | `hwmon`, `coretemp`, `x86_pkg_temp_thermal`, `thermal` (ACPI), `fan` |
| Display | `pcfb` (below) |
| Cuttlefish HAL ports | `pchvc` (below) |

Not built: i915 (the PC's display is on the RTX 4070; the iGPU is normally off with a
dGPU, and an i915 render node would make minigbm pick the Intel backend), nouveau.

All vendor modules are in `vendor_dlkm` and `modules.load`, so Cuttlefish's
`dlkm_loader` loads them on both machines; drivers without hardware just do not bind.
`pcfb` and `pchvc` do nothing unless their `enable=1` parameter is on the kernel command
line, which only the stick sets.

### `pcfb`: the UEFI framebuffer for Cuttlefish's graphics HALs

The GKI image has no `CONFIG_SYSFB`, so nothing registers the `efi-framebuffer` platform
device, and `efidrm`/`simpledrm` cannot be plain vendor modules (they `select` vmlinux
options). `pcfb.ko` registers the device from the boot `screen_info` and binds Linux's
own `efidrm` driver to it; `gki-build.sh` copies `efidrm.c` and the `drm_sysfb` helpers
from the kernel tree into `gki/android-pc/pcfb/gen/` with these changes (each checked, so
an upstream change fails loudly):

- the DRM driver is named **`virtio_gpu`**: minigbm picks its backend by driver name
  (none for efidrm), the ranchu HWC3 composer accepts only names containing `virtio`;
  both then use only generic KMS (dumb buffers, PRIME, atomic). minigbm's virtgpu backend
  logs that `VIRTGPU_GETPARAM` fails and runs as a 2D (non-virgl) virtio-gpu;
- a **render node** (`DRIVER_RENDER`): minigbm allocates on `renderD128`. Without it,
  minigbm opened `card0` first, became DRM master, and the composer failed with
  `failed to get master drm device`;
- the CRTC may be **enabled without a primary plane**: the composer commits the mode first
  and attaches its framebuffer later (virtio-gpu allows that; efidrm's
  `drm_atomic_helper_check_crtc_primary_plane` returned EINVAL, `failed to set display
  mode`);
- the plane also takes **ARGB8888, XBGR8888, ABGR8888** (the composer scans out XBGR8888;
  GOP framebuffers are XRGB8888); `pcfb_blit.c` does the red/blue swap that
  `drm_fb_blit()` lacks;
- no aperture or fbdev-client calls (not in the GKI image).

Result: `/dev/dri/card0` + `renderD128`, one connector whose only mode is the GOP mode
Limine set (default 1920x1080), page flips by copying into the framebuffer (no vsync; the
composer's software vsync paces it). The composer logs `failed to read EDID blob` and uses
the connector size, which is fine.

### `pchvc`: placeholder Cuttlefish HAL ports

Cuttlefish's vendor HALs (oemlock on `/dev/hvc10`, UWB `hvc9`, confirmation UI `hvc8`,
Bluetooth `hvc5`, ...) talk to host daemons over virtio-console ports. Without them they
abort, and system_server's `OemLockService` waited for the oemlock HAL until the watchdog
killed it. `pchvc.ko` registers a tty driver `hvc` (major 229, minors 0–17): opening
works, writes are discarded, reads never return data. The HALs start and register; calls
that need the host block (as on the Rust kernel in phase E3). The sensors HAL is handled
differently (section 4), since it waits for its host *during* registration.

## 3. Firmware

`/vendor/firmware` (the stick's command line sets `firmware_class.path=/vendor/firmware`;
the GKI image cannot load compressed firmware, so files are stored decompressed):

- from AOSP's `external/linux-firmware` (`aosp-device/pc/pc.mk`):
  `linux_firmware_iwlwifi-so-a0-gf-a0-89` and `-pnvm` (AX211 Wi-Fi, iwlmvm API 89) and
  `linux_firmware_btusb-ibt_ax211` (`intel/ibt-0040-0041.{sfi,ddc}`). These modules exist
  in the ninja graph whether or not a product uses them, so the same paths cannot also
  come from `PRODUCT_COPY_FILES` ("multiple rules generate");
- from the build host's linux-firmware (`gki-build.sh` step `firmware` →
  `aosp/device/android-pc/firmware/`): `rtl_nic/rtl8125*.fw` (RTL8125) and
  `intel/ibt-1040-0041.{sfi,ddc}` (the name btintel may ask for; same files).

No `regulatory.db` (the host has no wireless-regdb): cfg80211 uses the world domain.

## 4. Android side (`aosp-device/pc`)

The stick sets `androidboot.android_pc=1`; everything below keys on it, so the same
images still boot on Cuttlefish unchanged.

- `init.pc.rc`
  - `ro.vendor.disable_rename_eth0=1`: Cuttlefish renames `eth0` to `buried_eth0` (its
    virtual Wi-Fi bridge); on the PC `eth0` is the RTL8125 and EthernetService must see it.
  - bind-mounts an empty `/vendor/etc/sensors/hals.pc.conf` over
    `/vendor/etc/sensors/hals.conf`: Cuttlefish's sensors sub-HAL sends `list-sensors` to
    its host and waits for the answer, so `SensorService` (and with it system_server)
    blocked until the watchdog fired. With no sub-HAL the multi-HAL reports no sensors.
  - starts `vendor.pclog` when `androidboot.android_pc.log=1`.
- `pclog.sh` (debug aid; service domain `su`, so it only runs with SELinux permissive):
  mounts the stick's FAT partition `pclog` at `/mnt/vendor/pclog` (not directly under
  `/mnt`: vold unmounts those at start and killed it), writes `boot-N/dmesg.txt`,
  `logcat.txt`, `state-30/60/120/180/300/600.txt` (getprop, ps, lsmod, device nodes,
  addresses, SurfaceFlinger/window/ethernet dumps, service list) and copies
  `/data/tombstones`, syncing every 3 s. `PATH` must include `/system/bin` (vendor `sh`).
  - `on boot`: stops Cuttlefish's Bluetooth HAL `bt_hci` (H4 to a host over `/dev/hvc5`)
    and starts `vendor.bluetooth-pc` (below). A trigger on `init.svc.bt_hci` does not work
    from a vendor rc: vendor_init may not read `init.svc.*`.
- Bluetooth HAL `android.hardware.bluetooth-service.pc` (`aosp-device/hals/bluetooth`, Rust,
  phase F5; see [rust-hals.md](rust-hals.md)): finds the kernel's `hci0` (AX211, `btusb` +
  `btintel`) on the management channel and opens it through an HCI user channel. It serves
  the `IBluetoothHci/default` instance that Cuttlefish's `com.google.cf.bt` APEX declares,
  so it has no VINTF fragment. Domain `hal_pc_bluetooth`. Until F5 this was AOSP's default
  C++ HAL, built from `aosp-device-vendor/` (mirrored to `aosp/vendor/android-pc/`, which
  `tools/aosp-device-link.sh` now deletes); with it the PC showed `HCI Reset successful` and
  the adapter `ON` within 3 s of the stack starting. Without a controller (QEMU) the Rust
  HAL reports `UNABLE_TO_OPEN_INTERFACE` after 3 s and the stack stops retrying after 6
  restarts.
- Wi-Fi needs nothing PC-specific: Cuttlefish's legacy Wi-Fi HAL and wpa_supplicant use
  `wlan0` through nl80211 like any Linux Wi-Fi card.
- Apps (`tools/fetch-apps.sh` → `aosp/device/android-pc/apps/<Name>/`, installed by
  `pc.mk` when present): **Chromium** (the Chromium project's `AndroidDesktop_x64`
  snapshot, r1741609 = 160.0.8156.0, 406 MB; AOSP has only the WebView test shell, which
  cannot download, and Chrome has no x86_64 APK outside the Play Store) and **F-Droid**
  (2.0.2 from f-droid.org, signing certificate SHA-256 `43238d51…c9ccab` checked).
  Presigned APKs with targetSdk ≥ 30 must be `preprocessed: true`, or the build rejects
  them.
- sepolicy (`pc/sepolicy`): `vendor_pc_prop` for the property above (it was in
  `vendor_default_prop`, which every app then failed to read), `init` may `mounton`
  `vendor_configs_file`, the Bluetooth HAL's file label and rules.

### Boot parameters

Bootconfig appended to the initrd (`android/bootconfig.txt` on the ESP shows it):
Cuttlefish's own set (`hardware=cutf_cvm`, `fstab_suffix=cf.f2fs.hctr2`, the graphics
properties `gralloc=minigbm`, `hwcomposer=ranchu`, `display_finder_mode=drm`,
`egl=angle`, `vulkan=pastel`, the vendor APEX choices for composer, keymint
`rust_nonsecure`, gatekeeper `nonsecure`, ...) plus what Cuttlefish's U-Boot adds and the
PC needs:

- `androidboot.force_normal_boot=1`: without it first-stage init treats the boot as
  recovery ("First stage mount skipped (recovery mode)");
- `androidboot.slot_suffix=_a`, `androidboot.verifiedbootstate=orange`,
  `androidboot.vbmeta.device_state=unlocked`: there is no AVB boot loader; first-stage
  init reads `vbmeta_a` itself and runs AVB in permissive mode (dm-verity stays on);
- `androidboot.boot_part_uuid=a17c0de0-0017-4c0d-9e00-0000000000e5` (the ESP's
  PARTUUID): init waits for the partition with that UUID and takes its disk as the boot
  device, whatever port or controller the stick is on;
- `androidboot.openthread_node_id=1` (the Thread HAL aborts without it).

Kernel command line: the board's (`printk.devkmsg=on audit=1 binder.impl=rust cma=0
loop.max_part=7 init=/init bootconfig ...`), with `panic=10` (instead of `-1`),
`firmware_class.path=/vendor/firmware`, `pcfb.enable=1 pchvc.enable=1`, and
`snd_hda_intel.index=0,1 snd_usb_audio.index=2,3,4,5`: Cuttlefish's audio HAL plays to ALSA
card 0, and the built-in `snd-usb-audio` otherwise numbered the webcam's microphone first
(the HD Audio modules load later, from vendor_dlkm).

## 5. The stick image

```
tools/mkusb-aosp17.sh --disk /dev/sda --resolution 2560x1080      # → out/aosp17-usb.img
```

Options: `--disk DEV` sizes the image for that stick (its size from `lsblk`, no root):
`userdata` takes all the space left and the backup GPT sits at the stick's real end, so
nothing of an older layout survives; without it `--userdata MiB` (default 4096) sets the
size. `--resolution WxH` (default: Limine picks the monitor's preferred mode from its
EDID), `--density N` (default 240), `--append "WORDS"`, `--enforcing` (SELinux enforcing;
default permissive so `pclog.sh` runs), `--menu`, `--product DIR`. The build's `super.img`
is an Android sparse image (Cuttlefish unsparses it in place when it runs, so it may also
be raw): the script handles both.

GPT layout (for the 57.3 GiB SanDisk of the tests):

| Partition | Size | Contents |
|---|---|---|
| `esp` | 256 MiB | FAT32 `ANDROIDPC17`: Limine `EFI/BOOT/BOOTX64.EFI`, `boot/limine/limine.conf`, `android/kernel`, `android/initrd.img` (vendor ramdisk + generic ramdisk + bootconfig), `android/bootconfig.txt` |
| `pclog` | 256 MiB | FAT32 `PCLOG`, boot logs |
| `misc`, `frp` | 1 MiB | zero |
| `metadata` | 64 MiB | zero (formatted on first boot) |
| `vbmeta_a`, `vbmeta_system_a`, `vbmeta_vendor_dlkm_a`, `vbmeta_system_dlkm_a` | 1 MiB | the build's images |
| `boot_a`, `init_boot_a`, `vendor_boot_a` | 64/8/64 MiB | the build's images (AVB reads them) |
| `super` | 8 GiB | `super.img` |
| `userdata` | the rest (48.6 GiB) | zero (fs_mgr formats a wiped formattable partition) |

Limine: one entry, "Android 17", no menu (`timeout: 0`). `--menu` gives a 5 s menu with
**SELinux permissive**, **SELinux enforcing**, **kernel log on screen**
(`earlycon=efifb keep_bootcon`, slow, for boots that stop before Android draws) and
**no pcfb** (to tell a display problem from the rest).

Writing it (erases the stick; the script prints the commands with the right numbers):
everything up to the first 64 MiB of `userdata` (~9 GB, 3 min at 53 MB/s), then the
backup GPT in the last MiB:

```bash
lsblk -o NAME,SIZE,MODEL,TRAN
```
```bash
sudo dd if=out/aosp17-usb.img of=/dev/sda bs=1M count=8975 conv=fsync oflag=direct status=progress
```
```bash
sudo dd if=out/aosp17-usb.img of=/dev/sda bs=1M skip=58673 seek=58673 count=1 conv=fsync oflag=direct
```

Firmware settings (Gigabyte Z790, Del at power-on; other boards are similar):

- Boot → Secure Boot: Disabled (Limine and the kernel are not signed).
- Boot → CSM Support: Disabled (UEFI only).
- Boot → Fast Boot: Disabled (otherwise USB devices may not be initialized before the OS).
- Settings → IO Ports → USB Configuration → XHCI Hand-off: Enabled.

Save with F10, then press F12 at power-on and pick the `UEFI: <stick>` entry (not the one
without `UEFI:`). Use a rear USB port of the board, and plug the keyboard and mouse in
before powering on.

## 6. Testing without the PC

```
tools/mkusb-aosp17.sh --append "console=ttyS0,115200" out/aosp17-usb-qemu.img
WRITABLE=1 tools/run-qemu-aosp17-usb.sh out/aosp17-usb-qemu.img
tools/pclog-read.sh out/aosp17-usb-qemu.img          # → out/pclog/boot-N/
```

QEMU q35 + OVMF, `qemu-xhci` + `usb-storage`, `-vga std` (its GOP is what pcfb drives),
`usb-kbd` + `usb-tablet`, `e1000e`. Serial log in `out/aosp17-usb-serial.log`, monitor
socket `out/aosp17-usb-monitor.sock` (`screendump`). adb over the user network does not
work (Cuttlefish's adbd listens on vsock only); the `pclog` partition is the way in.

Results (2026-11-11, 4 vCPUs, 6 GB): Limine → kernel → first stage finds the stick by
PARTUUID in ~1.5 s → second stage → boot animation at ~15 s → `Boot is finished` /
`sys.boot_completed=1` → lock screen at 1920x1080 with the Ethernet icon; `eth0` (e1000e)
gets 10.0.2.15 by DHCP; `ILights/default` registered by the F2 Rust HAL; the same with
SELinux enforcing. Remaining noise: a few sysfs denials (USB HID `country`, wakeup source
names, `discard_max_bytes`), and `dlkm_loader` aborts at its end because `coretemp` and
`x86_pkg_temp_thermal` find no sensor in QEMU (it loads every other module first).
`run-qemu`'s QEMU has no Bluetooth controller, so the PC Bluetooth HAL falls back to its
serial path there.

Cuttlefish (`aosp/cvd-launch.sh`) with the same images: `sys.boot_completed=1`, Launcher3
resumed, kernel `6.18.21-android17-5`, `pcfb`/`pchvc` loaded and idle, `virtio_gpu` card0
as before.

## 7. On the real PC

Four boots on 2026-11-11 (logs read with `tools/pclog-read.sh` / the `PCLOG` partition
after rebooting into the build host; the build host *is* the target PC):

1. Boot completed; GOP framebuffer 1920x1080 (the monitor is 2560x1080), RTL8125 up with
   DHCP (`eth0` 192.168.1.15, network validated), USB keyboard (FC900R) and mouse, HD Audio
   and the NVIDIA HDMI codecs, webcam (`uvcvideo`, UGREEN Camera 4K), AX211 Bluetooth
   firmware loaded. Broken: Wi-Fi (`iwlmvm` unresolved LED symbol), ALSA card 0 was the
   webcam microphone, and the only "browser" was the WebView test shell.
2. 2560x1080 (`pcfb: 2560x1080, 32 bpp, stride 16384`), HDA as card 0, Chromium and F-Droid
   start. Wi-Fi still broken: `vendor_boot` had not been rebuilt with the new `mac80211`.
3. Wi-Fi works (`iwlwifi … firmware version 89 … op_mode iwlmvm`, `wlan0`, Cuttlefish's
   Wi-Fi HAL). Bluetooth does not (Cuttlefish's HAL talks to the placeholder `hvc5`).
4. Bluetooth works with the default HAL: `opening hci interface 0`, `HCI Reset
   successful`, adapter `ON`, Bluetooth on in Settings (pairing not tried yet).

### Combined F4–F8 test (2026-11-12)

One stick with every Rust HAL (`tools/mkusb-aosp17.sh --disk /dev/sda --resolution 2560x1080
--menu`; logs in `out/pclog-f4f8-2/` and `out/pclog-f4f8-3/`). All nine Rust HAL processes run
in their own domains (`hal_pc_{audio,camera,health,light,power,thermal,wifi,bluetooth}`), and
the permissive boots log **0 AVC denials** at all. The "SELinux enforcing" menu entry boots
to the launcher and behaves like the permissive boot (user report; `pclog.sh` does not run
in that mode, so no log).

| HAL | Real PC result |
|---|---|
| F4 health/power/thermal | AC online, no battery, level 100; thermal service lists `acpitz`, `acpitz-1`, `CPU0`, `VIRTUAL-SKIN`, `iwlwifi_1` (the ACPI zones that loaded after the HAL now show up); `intel_pstate` `min_perf_pct` 50 and EPP `performance` writable |
| F5 Bluetooth | `hci0: user channel open`, stack ON, inquiry + LE scan find devices; pairing with a phone: SSP numeric comparison → `BOND_BONDED` |
| F6 audio | ALC897 as card 0 (Speaker/Mic ports), AudioFlinger 48 kHz output thread writes through the HAL (no delayed writes); tone/mic check not confirmed yet (see below) |
| F7 camera | the UGREEN 4K UVC webcam is camera `100` (`/dev/video0`, MJPEG up to 4000x3000); preview and photo in the Camera app work. The CF emulated provider APEX is off (`...camera.provider.hal=none` in the boot config) |
| F8 Wi-Fi | `wlan0` on phy0 with the AX211's combinations, scans return the nearby BSSes; **hotspot works** (`wlan1` created with `NEW_INTERFACE`, hostapd `AP-ENABLED`, a phone connected). Joining a network while the Ethernet cable is plugged in does nothing: ConnectivityService ranks Ethernet above Wi-Fi, so Wi-Fi's network offer gets no request and `ClientModeImpl` logs `CMD_START_CONNECT but no requests and not connected, bailing` (framework behaviour, not the HAL). Unplug Ethernet to test a Wi-Fi connection. |

Fixes from this test:

- Hotspot: hostapd (in Cuttlefish's `com.google.cf.wpa_supplicant` APEX) failed with
  `Cannot read/write hostapd config /data/vendor/wifi/hostapd/hostapd_wlan1.conf`: Cuttlefish
  creates that directory only for its hwsim radios. `init.pc.rc` creates it on the PC.
- Chromium flickered black: its GPU process draws with Skia Graphite on Dawn/Vulkan, and on
  the `pastel` (SwiftShader) Vulkan driver `vkGetSemaphoreFdKHR` fails
  (`VK_ERROR_INVALID_EXTERNAL_HANDLE`), so the GPU process crashed and restarted every few
  seconds. `init.pc.chromium.rc` (on `/product`, so it runs in init's own domain) writes
  `/data/local/tmp/chrome-command-line` = `--disable-features=SkiaGraphite,VulkanFromANGLE`
  when `ro.boot.hardware.vulkan=pastel`; Chromium reads it on debuggable builds and draws
  with Ganesh on GL (ANGLE). Disabling `Vulkan` itself, or `--disable-gpu`, makes Chromium
  exit ("GPU process isn't usable"). Checked in QEMU (same images): 11 GPU process crashes in
  30 s before, none after, page renders and scrolls; on the PC no more flicker.
- `pclog.sh` now also records `getenforce`, denials per source context, the HAL processes,
  `dumpsys bluetooth_manager`/`wifi`/`media.camera`/`media.audio_flinger`, ALSA state and
  `/data/media/0/DCIM`, at more times (up to 60 min). If `tools/mkusb-aosp17.sh` finds
  `out/audio-check/classes.dex` (built by `tools/audio-check.sh`) it copies it to the
  `pclog` partition and `pclog.sh` runs it at 240 s and 480 s (5 s of 440 Hz, then 10 s of
  mic recording, results in `boot-N/audio-check.txt`). Both runs on the PC aborted
  (`BOOTCLASSPATH and DEX2OATBOOTCLASSPATH must not be empty`): the service starts on `init`,
  before init exports the class path. It now loads `/data/system/environ/classpath` before
  each run (not yet run on the PC).

Not yet checked on the PC: a Wi-Fi connection with Ethernet unplugged, the line-out tone
and mic level (F6), power off/restart, suspend. ARM-only apps (e.g. TikTok:
`libnxreword.so is for EM_AARCH64`) need an ARM64 native bridge, which the image does not
have (`ro.dalvik.vm.native.bridge=0`); that and Google services are a separate task.

### F9e: Wi-Fi does not connect while Ethernet is up (2026-11-13)

On the `f9b` stick (`out/pclog-f9b/boot-1/`) the user picked a WPA2 network in Settings and
nothing happened. Everything below the framework worked: `iwlwifi` loaded firmware 89,
`wlan0` came up, the Rust HAL started (`wlan0 on phy0: combinations ...`),
wpa_supplicant registered its AIDL service, and scans found the network (2412 MHz). Every
attempt ended in `ClientModeImpl` with `CMD_START_CONNECT but no requests and not connected,
bailing` (16 times), because the Ethernet cable was plugged in:

- ConnectivityService sends a network request to a provider's offer only if the offer
  could beat the network that satisfies it now (`NetworkRanker.mightBeat`). Its ranking
  puts Ethernet above Wi-Fi (`PREFERRED_TRANSPORTS_ORDER`), so with a validated Ethernet
  network the Wi-Fi factory has no request, and `ClientModeImpl` refuses to connect, even
  to a network the user picked.
- Fix: `Settings.Global.WIFI_ALWAYS_REQUESTED` (`wifi_always_requested`), which AOSP made
  for this case: ConnectivityService then files a background request for an internet Wi-Fi
  network. `aosp-device/hals/wifi/overlay` (`pc_overlay_settings_provider_wifi`, a static
  RRO on `/vendor`, added by `hals/wifi/hal.mk` for both HALs) sets SettingsProvider's
  `def_enable_wifi_always_requested` to `true`. The default is written when a new
  `userdata` is set up; on a device that already ran, run
  `adb shell settings put global wifi_always_requested 1`. Ethernet stays the default
  network; Wi-Fi connects next to it, and saved networks auto-join.

A second bug showed in the same log: `updateSupportedBandForStaInternal wlan0 : 0x0`. The
Rust HAL's `getUsableChannels` returned channel numbers, but `WifiUsableChannel.channel` is
a frequency in MHz, so the framework found no supported band. The HAL now returns MHz.

Verified on Cuttlefish (same `vendor.img`), with a user-mode QEMU NIC renamed back to
`eth0` (validated Ethernet default network, as on the PC) and two hwsim radios
(`--enable_wifi=true`), SoftAP `pcap` on `wlan1`:

- `wifi_always_requested` is `1` on a fresh `userdata`; the overlay is enabled.
- With the setting at `0`, `cmd wifi connect-network pcap wpa2 ...` logs the same
  `bailing` line as the PC.
- Set back to `1` (no other change), `WifiNetworkFactory` has a request
  (`mGenericConnectionReqCount 1`), and `wlan0` joins the saved network at once:
  `CTRL-EVENT-CONNECTED`, DHCP `ACK`. Then Tethering restarted the SoftAP, because the
  station had an address in the hotspot's own prefix (`restart tethering`). That only
  happens when the device joins its own hotspot.
- `updateSupportedBandForStaInternal wlan0 : 0x3` (2.4 + 5 GHz). Host tests 25/25. No AVC
  denials.

To be checked on the PC: joining the home network with the cable plugged in.

What to report after a boot: plug the stick back into the build host and run
`tools/pclog-read.sh /dev/sdX` (or copy from the mounted `PCLOG` partition), plus photos of
the screen if Android did not draw. If the PC rebooted after a kernel panic (`panic=10`),
the panic is in `/sys/fs/pstore` on the next Linux boot (efi-pstore).

## 8. What the F4–F8 HALs will find (stock kernel)

| HAL | Device / sysfs | Notes |
|---|---|---|
| F4 health/power/thermal (done, see [rust-hals.md](rust-hals.md)) | `/sys/class/power_supply` (empty on the desktop even with `ac.ko`/`battery.ko`: the HAL reports AC online, no battery), `/sys/class/thermal/thermal_zone*` (`x86_pkg_temp`, ACPI `thermal`), `/sys/class/hwmon/hwmon*` (`coretemp`), `/sys/devices/system/cpu/cpufreq/policy*` (`intel_pstate`), `/sys/class/rtc/rtc0` | labels: `genfscon` for `/devices/virtual/thermal`, `/sys` regexes in `file_contexts` for hwmon and power_supply |
| F5 Bluetooth | AX211 on USB `8087:0033`: the kernel's `btusb`+`btintel` take it as `hci0` (firmware `ibt-1040-0041`); an HCI user channel socket (`AF_BLUETOOTH`, `HCI_CHANNEL_USER`) after the management channel powers it down | the Rust HAL (F5, domain `hal_pc_bluetooth`) replaced AOSP's default C++ HAL under the same binary name; real-PC check in the combined F4–F8 test |
| F6 audio | ALSA card 0 = HD Audio PCH (`pcmC0D0p`/`pcmC0D1p` playback, `pcmC0D0c` capture; ALC897 Line Out, Front headphone, Mic), card 1 = RTX 4070 HDMI/DP (`pcmC1D3p`, `D7p`, `D8p`, `D9p`), card 2 = the webcam's microphone (`pcmC2D0c`); `/dev/snd/*` are `system:audio` 0660 | `audio_device` label from the platform policy |
| F7 camera | `/dev/video0`–`video2` (`system:camera` 0660) from `uvcvideo` | `video_device` |
| F8 Wi-Fi | `wlan0` from `iwlmvm` (nl80211, firmware API 89 + PNVM); `/dev/socket/wpa_wlan0` | works today with Cuttlefish's legacy Wi-Fi HAL (`vendor.wifi_hal_legacy`) + wpa_supplicant; no `regulatory.db` |
| lights (F2) | `/sys/class/leds`, `/sys/class/backlight` (the desktop has none) | registered, 0 lights |
