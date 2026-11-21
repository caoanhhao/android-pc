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
| Apps | `tools/fetch-apps.sh` | Chromium, F-Droid, microG, Play Store and Termux as prebuilt `/product` apps |
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
  - starts `vendor.ueventd_watch` at `early-init` (below).
- `pclog.sh` (debug aid; service domain `su`, so it only runs with SELinux permissive):
  mounts the stick's FAT partition `pclog` at `/mnt/vendor/pclog` (not directly under
  `/mnt`: vold unmounts those at start and killed it), writes `boot-N/dmesg.*`,
  `logcat.*`, `state-30/60/120/180/300/600.txt` (getprop, ps, lsmod, device nodes,
  addresses, SurfaceFlinger/window/ethernet dumps, service list) and copies
  `/data/tombstones` and the end of Unreal games' own logs (`applogs/`, F9q), syncing every 3 s. `PATH` must include `/system/bin` (vendor `sh`).
  Size limits (F9l; the f9e stick's first boot wrote 198 MB of logcat and 48 MB of dmesg,
  filled the 256 MiB partition, and the next boot could not log at all):
  - dmesg and logcat go through `split` into 2 MiB (`dmesg.aaa`, `dmesg.aab`, ...) and
    4 MiB (`logcat.aaa`, ...) pieces. Every 3 s the script drops the pieces between the
    first ones and the last four: dmesg keeps its first 2 MiB and last 8 MiB, logcat its
    first 8 MiB and last 16 MiB, so the start of the boot and the most recent part both
    survive. Dropped pieces are listed in `boot-N/pruned.txt`. With the state dumps a boot
    takes about 40 MiB at most.
  - Before making `boot-N` it deletes the oldest boots until at most 4 remain (5 with the
    new one) and at least 48 MiB is free. Boot numbers keep counting up (from the `last`
    file), so `boot-N` is never reused. If `mkdir` still fails, all old boots go.
  - `tools/pclog-read.sh` joins the pieces into `boot-N/dmesg.txt` and `boot-N/logcat.txt`.
- `ueventd-watch.sh` (service `vendor.ueventd_watch`, F9l; domain `su` like `pclog.sh`, so
  it runs on the default permissive stick but not with the "SELinux enforcing" entry):
  ueventd recovery. When ueventd dies, init restarts it, but the new ueventd skips coldboot
  (`ro.cold_boot_done` is already set), so devices the kernel added while it was down never
  get a `/dev` node. That happened on the PC (f9e, boot 1, `out/pclog-f9e/boot-1/`):
  ueventd crashed at 9.07 s (SIGSEGV in `libpcre2` during a restorecon, probably the PC's
  memory corruption), `dlkm_loader` loaded `pcfb` at 9.66 s, init restarted ueventd only at
  13.8 s (it was blocked in Cuttlefish's `wait /dev/dri/card0`), and `/dev/dri`,
  `/dev/video*` and the ALSA PCMs never appeared: SurfaceFlinger never started and
  system_server restarted 38 times at the boot animation. The script waits for
  `ro.cold_boot_done`, then watches ueventd's pid. When ueventd is replaced it writes `add`
  to the `uevent` file of every device under `/sys/devices` whose `DEVNAME` has no node in
  `/dev` (or `/dev/block`), so the new ueventd creates the nodes (with their permissions
  and labels). It replays only those devices, not every `uevent` like a real coldboot:
  vold, netd and the health HAL listen to the same events, and vold creates a second
  `Disk` for a block device it already has. While ueventd is down the script also makes
  missing `/dev/block` nodes itself (`mknod`, `restorecon`) plus the
  `/dev/block/mapper/<name>` and `by-uuid` links of dm devices, and hands them back to
  ueventd with an `add` once it returns: init does not restart (or even reap) ueventd while
  it is blocked in a command, and in `mount_all --late` vold waits for the dm node of
  userdata's default-key device. Without that a crash at that moment leaves `/data`
  unmounted (seen in QEMU: vold `Failed to wait for default-key device userdata`, keystore2
  panics, the device reboots). A dead ueventd stays a zombie until init reaps it, so the
  script checks the process state, not just `/proc/PID`. A few devices are replayed every
  time because ueventd puts their node elsewhere (`mapper/control` is
  `/dev/device-mapper`, `hwrng` is `/dev/hw_random`) or makes none (`bsg/*`); those extra
  `add`s are harmless. If a HAL was already running without its
  nodes it is restarted with `ctl.restart`: allocator, composer and SurfaceFlinger for
  `dri/*` (SurfaceFlinger's restart brings zygote down with it), the audio HALs and
  audioserver for `snd/*`, the camera HAL and cameraserver for `video*`. Everything it
  does goes to the kernel log as `pc-ueventd-watch: ...`. Test knob:
  `androidboot.android_pc.test_ueventd_gap=1` (e.g. `mkusb-aosp17.sh --append`) makes it
  kill ueventd once, right after coldboot, which reproduces the PC failure.
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
- **Termux** (F9j): `fetch-apps.sh termux` takes F-Droid's builds of Termux 0.118.3
  (`com.termux_1002.apk`), Termux:API 0.53.0 and Termux:Boot 0.8.1 from
  `f-droid.org/repo`, pinned by SHA-256 and checked against the certificate F-Droid signs
  all three with (SHA-256 `228fb2cf…d41c42`; the GitHub releases use a different key). The
  APK is universal and its x86_64 bootstrap runs natively, without the ARM translator. All
  three share the `com.termux` user ID and are plain, non-privileged `/product` apps, so
  F-Droid can update them (the update goes to `/data`; "Uninstall updates" goes back to
  the bundled version); they can be disabled but not uninstalled.
  Termux keeps its JNI libraries compressed (`extractNativeLibs="true"`), and
  PackageManager never extracts a `/product` app's libraries: it looks for them in
  `/product/app/Termux/lib/x86_64/`. Soong's `android_app_import` cannot install them
  there (and an `Android.mk` module is ignored by the Soong-generated product image), so
  the script unpacks `lib/x86_64/*.so` into `apps/Termux/lib/x86_64/` and `pc.mk` installs
  every `apps/<Name>/lib/x86_64/*.so` beside its APK with `PRODUCT_COPY_FILES`. The APK
  itself is unchanged (`skip_preprocessed_apk_checks`, because its libraries are
  compressed). Checked on Cuttlefish, enforcing: `primaryCpuAbi=x86_64`,
  `libtermux-bootstrap.so` loads from `/product/app/Termux/lib/x86_64`, "Bootstrap packages
  installed successfully", and `pkg install file tree htop` from the terminal downloads and
  runs x86_64 binaries, with no AVC denials.
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
tools/mkusb-aosp17.sh --disk /dev/sda      # → out/aosp17-usb.img
```

Options: `--disk DEV` sizes the image for that stick (its size from `lsblk`, no root;
refused for a disk that is not on USB or has mounted partitions, so never the PC's NVMe):
`userdata` takes all the space left and the backup GPT sits at the stick's real end, so
nothing of an older layout survives. **Always build release images with `--disk`**: the
F9d image was made without it and its 4 GiB `userdata` was too small for large games
(Genshin Impact, Delta Force); see §9; without it `--userdata MiB` (default 4096) sets the
size. `--resolution WxH` (default 2560x1080, the test monitor in landscape, since F9f;
Limine uses another mode if the monitor lacks it; `auto` lets Limine pick the monitor's
preferred mode from its EDID, which gave 1920x1080 on the first boot), `--density N`
(default 160 since H4, 240 before; see `docs/desktop-mode.md`), `--append "WORDS"`,
`--enforcing` (SELinux enforcing;
default permissive so `pclog.sh` runs), `--product DIR`. The build's `super.img`
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

Limine: one entry, "Android 17", no menu (`timeout: 0`): the RTX 4070 on NVIDIA's own driver
(Phase L, `docs/nvidia-native.md`), which also sends sound to a monitor on the card
(`docs/rust-hals.md`, "NVIDIA without a GPU driver"). The earlier `--menu` (pcfb entries for
SELinux enforcing, the kernel log on screen, no pcfb, and the other GPU paths) is on branch
`gpu-alternatives`; `--append` adds words for a one-off debug image.

A stick that already runs Android is updated with `tools/update-usb-aosp17.sh` instead,
which keeps apps, accounts and data (§10). The `dd` commands below are only for a new,
empty stick, or for a layout change the update script refuses; they wipe every app and
account. Release images since F9 are update images (`--userdata 64`, not sized for a
stick), which must not be written with `dd`.

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

`QEMU_EXTRA` passes more QEMU arguments, e.g. the extra disks of §9 and
`-device vhost-vsock-pci,guest-cid=3`, after which `adb connect vsock:3:5555` reaches
Cuttlefish's adbd (it listens on vsock only; host vsock CID 3 must be free, so not while
Cuttlefish runs, which the one-VM rule already ensures).

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

### F9l: ueventd recovery and the pclog size cap (2026-11-13)

The f9e stick's first boot hung at the boot animation after a ueventd crash (§4,
`ueventd-watch.sh`) and filled the `pclog` partition, so its second boot, which reached
Android, left no logs. F9l adds `ueventd-watch.sh` and the `pclog.sh` size cap (§4).

QEMU tests (`out/f9l-qemu-serial-1.log`, `out/f9l-qemu-serial-2.log`):
- `androidboot.android_pc.test_ueventd_gap=1` (`mkusb-aosp17.sh --append`): the script
  kills ueventd at 3.0 s, right after coldboot. The first version only replayed uevents
  after the restart: init was then blocked for 25 s in `mount_all --late` (vold waiting for
  `dm-17`), `/data` did not mount and the device rebooted in a loop. With the block bridge:
  `made /dev/block/dm-17`, `linked /dev/block/mapper/userdata`, init restarts ueventd at
  7.0 s, the script hands `dm-17` back and replays 13 devices (`snd/*`, `video0`, a USB bus,
  `dma_heap/*`, ...), restarts the audio and camera HALs and their servers, and boot
  completes; `/dev/snd`, `/dev/video0`, `/dev/dri` all present with ueventd's modes.
- After boot: `kill -9` ueventd, delete `/dev/dri/card0` and `renderD128`, reload
  `dummy_hcd`. ueventd is back 1.5 s later; the script replays `dri/card0`, `dri/renderD128`,
  restarts the allocator, the composer and SurfaceFlinger; zygote restarts with it,
  `sys.boot_completed=1` again and the lock screen is back on the screen dump.
- pclog: the partition was pre-filled with `boot-1`..`boot-6` (`boot-2` holding 200 MB) and
  `last=6`. The boot deleted `boot-1` and `boot-2` and wrote `boot-7`. Under a flood of
  70000 kernel lines and 60000 1 KB logcat lines, `boot-7` stayed at 38 MB, with
  `pruned.txt` listing the dropped pieces; `tools/pclog-read.sh` joined the rest into a
  9 MB `dmesg.txt` and a 25 MB `logcat.txt`.
- 0 AVC denials in all runs (permissive, the stick's default).

Not covered: the "SELinux enforcing" entry (the script runs in `su`, like `pclog.sh`, and
does not start there), devices other than block ones needed while ueventd is down (init
waits for them with a timeout, as it did on the PC), and the real PC.

USB image `out/aosp17-usb-f9f.img` (`tools/mkusb-aosp17.sh --disk /dev/sda --menu`, same
layout as f9e): its super partition is identical to the build's `super.img` (sha1 of the
unsparsed image). QEMU smoke boot: boot completes, `vendor.ueventd_watch` runs and logs
nothing (no restart), `pclog` writes `boot-1/dmesg.aaa` and `logcat.aaa`, `/data` is 49 GB,
no AVC denials. Writing the image resets the `pclog` partition (the old logs are gone).

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

## 9. Storage: `userdata` size and extra disks (F9i)

### `userdata` fills the stick

Games did not install ("not enough space") because the F9d image was made without
`--disk`, so `userdata` was the 4 GiB default. `tools/mkusb-aosp17.sh --disk /dev/sda`
makes `userdata` everything after `super` (48.6 GiB on the 57.3 GiB SanDisk). The image
file is sparse (`truncate`, `dd conv=sparse`), and the two printed `dd` commands write
only up to the first 64 MiB of `userdata` plus the backup GPT, so writing the stick takes
the same ~3 minutes as before. Those 64 MiB of zeros wipe the old `userdata`, and fs_mgr
formats it (f2fs, full partition size) on the first boot. Growing `userdata` at first
boot was not chosen: it needs a step in first-stage init that rewrites the boot disk's GPT
(and moves the backup GPT) before fs_mgr formats `userdata`, while `--disk` gives the same
result with no code on the device. The cost is one image per stick size.

### Extra disks as portable or internal storage

An SSD or other disk on USB, SATA or NVMe shows up in Android as a "USB drive". Android's
own wizard (notification → "Format" or "Format another way" → confirm the dialog that says
all data will be erased) makes it portable storage (FAT/exFAT) or adopts it as internal
storage (encrypted ext4, apps and media can move there). Nothing is formatted without
that dialog.

- **fstab.** `aosp-device/pc/fstab.pc.disks` adds three `voldmanaged=...:auto,encryptable=userdata`
  entries (DEVPATH patterns `/devices/*/usb*/block/sd*`, `/devices/*/ata*/block/sd*`,
  `/devices/*/nvme*n*`). A genrule appends them to Cuttlefish's `fstab.cf.f2fs.hctr2`
  and installs the result as `/odm/etc/fstab.cf.f2fs.hctr2`. fs_mgr reads `/odm/etc` before
  `/vendor/etc`, so vold and `mount_all` take it; first-stage init keeps the vendor ramdisk
  copy. `encryptable=userdata` sets the disk's adoptable flag (`vold.has_adoptable=1`), so
  no `persist.sys.adoptable` override is needed.
- **vold patch** (`tools/patches/aosp17-vold-pc-disk-guard.patch`; apply with
  `git -C aosp/system/vold apply ../../../tools/patches/aosp17-vold-pc-disk-guard.patch`).
  A PC disk guard runs before vold creates a disk, so a disk it rejects never reaches the
  framework, Settings, or a format dialog:
  - `nvme0n1` is always ignored (the PC's Arch Linux disk; an extra NVMe SSD enumerates
    as `nvme1n1`);
  - the boot device (the disk holding `/dev/block/by-name/*`) is ignored;
  - any other disk is managed only if it is blank (first MiB zero, or an empty partition
    table, and no filesystem signature), holds FAT/exFAT on the whole disk, or has only
    partitions Android makes or mounts: GPT basic data, `android_meta`, `android_expand`,
    Microsoft reserved; MBR types 06/07/0b/0c/0e. A data partition must hold FAT, exFAT
    or nothing: NTFS, ext4, btrfs, LUKS, swap... make vold ignore the whole disk.
  - It logs `PC disk guard: ignoring <disk> (<reason>)` / `managing <disk>` (vold, warning
    level). To use a disk that is ignored, empty it on another computer first (for example
    delete all partitions), since Android will not.
- **NVMe partition bug in AOSP vold (fixed by the same patch).** Stock vold takes a
  partition's device number as disk minor + partition number. NVMe disks (and SCSI
  partitions past 15) use the extended major 259 with minors in probe order, so with two
  NVMe disks that is a partition of the *other* disk. In the QEMU test, adopting the blank
  `nvme1n1` (259:0) formatted 259:2, the first partition of the Arch-like `nvme0n1`.
  `Disk::readPartitions` now reads each partition's `dev` from sysfs
  (`GetPartitionDevices` in `Utils.cpp`).
- **Large games.** An adopted disk takes app installs that allow external storage;
  Settings → Apps → *app* → Storage → Change moves one, `pm move-package PKG UUID` too,
  and `pm install --install-location 2` puts new installs there. Apps that do not allow
  external storage stay on `userdata`, which is why `userdata` must fill the stick anyway.

### Test (QEMU, 2026-11-13)

`tools/mk-test-disks.sh` makes four sparse test disks in `out/f9i-test/` and prints the
`QEMU_EXTRA` line for them (plus vsock for adb):

```bash
eval "$(tools/mk-test-disks.sh | tail -1)"; export QEMU_EXTRA
```
```bash
NO_WINDOW=1 tools/run-qemu-aosp17-usb.sh out/aosp17-usb-qemu.img
```

| Disk | Contents | Result |
|---|---|---|
| `sda` (boot stick, USB) | the image | ignored (boot device) |
| `nvme0n1` (259:1) | GPT: ESP (FAT32) + Linux (ext4), like the PC's Arch disk | ignored (name); file unchanged (sha1) |
| `nvme1n1` (259:0) | blank | "USB drive detected" notification → wizard → adopted: `private:259,5` (= `nvme1n1p2`, 7.6 GiB), primary storage moved, test APK installed under `/mnt/expand/<uuid>/app` |
| `sdc` (SATA, AHCI) | GPT basic-data partition holding ext4 | ignored (`partition 1 holds ext4`); file unchanged |
| `sdb` (USB) | MBR, FAT32 | mounted as portable storage `/storage/<uuid>` |

Not covered: the adopted disk after a reboot (the QEMU stick runs in snapshot mode, so
`/data` and the disk's key are gone on reboot; this path is stock AOSP), and real hardware.
Real-PC check: plug a blank SSD (USB enclosure, SATA or a second M.2 slot), boot, set it up
from the notification, and confirm in `logcat -s vold` that `nvme0n1` is ignored.
Side note: `pm install` over adb waited forever for Play Protect (Finsky) verification in
QEMU (no network); `settings put global verifier_verify_adb_installs 0` avoids it.


## 10. Updating the OS without losing data (`tools/update-usb-aosp17.sh`)

Writing a whole new image with `dd` (§5) formats `userdata` and `metadata` on the next
boot, so every installed app, game, Google account and file is lost. To change only
Android, write only the OS partitions over the stick's old ones:

| Partition | On an update |
|---|---|
| `esp` (Limine, kernel, initrd, `limine.conf`, `android/build.txt`) | rewritten |
| `boot_a`, `init_boot_a`, `vendor_boot_a`, `vbmeta*_a` | rewritten |
| `super` (system, vendor, product, ... with their dm-verity) | rewritten |
| `userdata` (apps, app data, accounts, `/sdcard`) | kept |
| `metadata` (the FBE metadata key, wrapped by KeyMint) | kept |
| `misc`, `frp`, `pclog` (logs), the GPT | kept |

This is what an OTA does on a phone: same partitions, new contents, `/data` untouched. On
the first boot of the new build Android sees a new fingerprint, rescans the system apps,
upgrades the ones whose APK changed (their data stays: same package, same signing key),
re-grants permissions and recompiles code; that first boot is slower.

### What has to stay the same

- **The layout.** Same partition names, each OS partition at least as big as the new
  build's. `super` is fixed at 8 GiB by the board config (`BOARD_SUPER_PARTITION_SIZE`)
  and the boot images by their `BOARD_*IMAGE_PARTITION_SIZE`, so builds fit unless those
  change; the script refuses otherwise (then one more full flash is needed).
- **The AVB key and rollback index** (`vbmeta`). KeyMint binds its keys to the verified
  boot key; the FBE keys of `userdata` and the `metadata` key are KeyMint keys. Our builds
  use AOSP's test key and rollback index 0; the script compares them.
- **No patch-level downgrade.** KeyMint refuses keys from a newer OS/security patch level,
  so going back to an older `ro.build.version.security_patch` would lock the data. Checked.
- **The signing keys of system apps** (platform/test keys from this tree, the prebuilt
  microG, Play Store, F-Droid, Termux, Chromium APKs): an updated app with another
  certificate would not install over its data. Unchanged as long as the same tree builds it.
- **A new build fingerprint.** AOSP's default `BUILD_NUMBER` is the constant
  `eng.<user>`, so two builds had the same fingerprint, and Android would neither rescan the
  system apps nor drop its package cache. `tools/aosp17-build.sh` now sets
  `BUILD_NUMBER=eng.<user>.<YYYYMMDD.HHMM>` for each run (override with `BUILD_NUMBER=...`);
  the number lands in `system/build.prop` only when `system.img` is rebuilt, so an update
  build needs the `system.img` step (the default `STEPS` have it). `mkusb-aosp17.sh` copies
  the build number, patch level and fingerprints to `android/build.txt` on the ESP, read
  from `system.img`/`vendor.img` themselves (`fsck.erofs --extract`): the product dir's
  `system/build.prop` is a stale staging copy that Soong no longer updates, and the
  `com.android.build.*` props in `vbmeta*.img` keep the old number until Kati reruns. The
  update script refuses when the number did not change (`--force` overrides).

### Commands

Build, then make an image of the new build with the **same** `mkusb` options the stick
was made with (the user's stick, flashed from f9f with `--menu`; that option is gone since
Phase L, which boots one entry), but without `--disk` and with
a tiny `userdata` (the update never copies it). Name it after the release; the F9 release
is `out/aosp17-usb-f9.img`:

```bash
tools/aosp17-build.sh
```
```bash
tools/mkusb-aosp17.sh --userdata 64 out/aosp17-usb-f9.img
```

Find the stick (a SanDisk of about 57 GB on `usb`; `/dev/sda` on the build host so far):

```bash
lsblk -o NAME,SIZE,MODEL,TRAN
```

Run the script as yourself, never with `sudo`: for a stick it asks for the sudo password
once at the start and runs only the commands that read or write `/dev/sdX` (`sfdisk`, `dd`,
`mcopy` on the stick) through `sudo`, keeping the ticket fresh for the whole run.

Check what the update would do (writes nothing):

```bash
tools/update-usb-aosp17.sh --dry-run out/aosp17-usb-f9.img /dev/sda
```

Then update (asks for `yes` after showing the stick; ~9 GB written, then read back and
compared):

```bash
tools/update-usb-aosp17.sh out/aosp17-usb-f9.img /dev/sda
```

The script refuses a disk that is not on USB, has mounted partitions, is not an
android-pc stick (ESP partition UUID `a17c0de0-…-e5`) or lacks `metadata`/`userdata`. It
writes `super` first and the ESP last, so an interrupted run still boots the old kernel;
running it again finishes the update. A target that is an image file works the same way
(that is how it is tested).

### Why not A/B OTA (`update_engine`)

An in-Android update (OTA package, `update_engine`, write the inactive slot, switch) would
not need the PC with the build tree, but it needs a second copy of every OS partition
(`_b`, +8.3 GiB, so a full reflash now), a `boot_control` HAL and a bootloader that
reads the slot and falls back (Limine boots one fixed kernel/initrd from the ESP; the
kernel and initrd would have to come from the slot's boot images), plus signed OTA
packages and an updater app. For one PC updated from the machine that builds it, rewriting
the partitions from Linux gives the same result (data kept) with none of that.

### Test (QEMU, 2026-11-13)

On a sparse copy of `out/aosp17-usb-f9f.img` (`WRITABLE=1 tools/run-qemu-aosp17-usb.sh`):

1. Booted the old build (`.../eng.caoanh:userdebug/test-keys`), installed a small
   debuggable test APK over adb, wrote `files/marker.txt` into its credential-encrypted data
   (`run-as`), a file in `/sdcard/Download`, a secure setting, and set a lock-screen PIN
   (`locksettings set-pin`), so the CE keys are bound to a credential. Powered off.
2. Built with the new `tools/aosp17-build.sh` (image steps up to `super.img`, BUILD_NUMBER
   `eng.caoanh.20261113.2019`), made `out/aosp17-usb-update.img` with
   `mkusb-aosp17.sh --menu --userdata 64`, ran `update-usb-aosp17.sh` on the copy (21 s,
   read-back compare OK). A second run refuses: same build number.
3. Booted: `ro.build.fingerprint` is the new one; `pm_critical_info: Upgrading from ...
   eng.caoanh ... to ... eng.caoanh.20261113.2019`, "Build fingerprint changed; clearing code
   caches". The user starts locked (`RUNNING_LOCKED`), the PIN unlocks it
   (`RUNNING_UNLOCKED`), and the test app, its CE file, the `/sdcard` file and the setting
   are all there. 0 AVC denials.

Not covered: a Google account and Play Store apps (no network sign-in in QEMU; they live in
the same `userdata` as the test app) and the real stick (same code path, block device
instead of a file).

### F9 release (2026-11-13)

`out/aosp17-usb-f9.img` (`--menu --userdata 64`, build `eng.caoanh.20261113.2228`, main at
8c8638e) is the frozen F9 build. `--dry-run` against the f9g stick image accepts it
(same layout and AVB key, patch level 2026-06-05 on both sides, new build number). The
real update on a sparse copy of that image wrote and compared all OS partitions, and the
copy then booted in QEMU with the new fingerprint, Google's WebView as the provider and 0
AVC denials. The older stick images (f9b–f9g, `aosp17-usb-update.img`) and the pclog dumps
before `pclog-f9f` were deleted from `out/`; the logs quoted in the docs from those boots
are no longer on disk.
