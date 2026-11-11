# AOSP 17 from a USB stick on the real PC, stock GKI kernel (phase F3a)

Status (2026-11-11): the stick image boots to the lock screen / launcher in QEMU the way
the PC boots it (UEFI/OVMF, the image as a USB mass-storage stick on xHCI, the GOP
framebuffer as the only display, USB keyboard + tablet, e1000e with DHCP), with SELinux
permissive and enforcing. Cuttlefish still boots `aosp_pc_x86_64` with the same kernel
and images. **Not yet tried on the real PC** (section 7 tracks that).

The target is the phase E PC: Gigabyte Z790 AORUS ELITE AX, i7-13700K, RTX 4070 (the
display; no open driver, so the UEFI GOP framebuffer is used), Realtek RTL8125B 2.5 GbE,
Realtek ALC897 HD Audio, Intel AX211 Wi-Fi + Bluetooth (CNVi, BT on USB `8087:0033`), a
UVC webcam. See [usb-boot.md](usb-boot.md) for the same machine under the Rust kernel.

## 1. Pieces

| Piece | Where | What |
|---|---|---|
| Kernel source | `gki/` (gitignored) | `repo` checkout of `common-android17-6.18-2026-06` (the branch of the F1 prebuilt GKI, now 6.18.21) |
| Build | `tools/gki-build.sh` | Kleaf builds through `tools/limited-build.sh`, installs into `aosp/device/android-pc/` |
| Vendor kernel package | `aosp-device/kernel/` | `BUILD.bazel` (`//android-pc`), `pc_x86_64.fragment`, `pcfb/`, `pchvc/` |
| Product glue | `aosp-device/pc/`, `aosp-device/BoardConfig.mk` | `init.pc.rc`, `pclog.sh`, firmware, sepolicy |
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
virtual-device fragments **plus** `pc_x86_64.fragment`. Using the same fragments keeps the
config of modules both builds produce identical: the symbol CRCs of the 20 884 symbols the
two `Module.symvers` share match, so `iwlwifi` built here loads against virtual-device's
`cfg80211`/`mac80211`. `install` takes virtual-device's modules and adds the PC build's
modules that virtual-device does not have.

Built into the GKI image already: NVMe, xHCI, usb-storage + UAS, USB HID + hid-generic,
evdev, uvcvideo, snd-usb-audio, ACPI button, RTC CMOS, intel_pstate, intel_idle, VFAT,
efi-pstore. The fragment adds, as modules:

| Hardware | Modules |
|---|---|
| SATA | `libata`, `libahci`, `ahci` |
| RTL8125 2.5 GbE | `r8169`, `realtek` (PHY) |
| e1000e (QEMU tests) | `e1000e` |
| HD Audio | `snd-hda-core`, `snd-hda-codec`, `snd-hda-intel`, `snd-hda-codec-generic`, the Realtek codecs (`snd-hda-codec-alc662` drives the ALC897) and the HDMI codecs (`-hdmi`, `-nvhdmi`, `-intelhdmi`, ...), `snd-ctl-led` |
| AX211 Wi-Fi | `iwlwifi`, `iwlmvm` (`cfg80211`, `mac80211` from virtual-device) |
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
- sepolicy (`pc/sepolicy`): `vendor_pc_prop` for the property above (it was in
  `vendor_default_prop`, which every app then failed to read), `init` may `mounton`
  `vendor_configs_file`.

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
`firmware_class.path=/vendor/firmware`, `pcfb.enable=1 pchvc.enable=1`.

## 5. The stick image

```
tools/mkusb-aosp17.sh                    # → out/aosp17-usb.img
```

Options: `--userdata MiB` (default 4096), `--resolution WxH` (default 1920x1080),
`--density N` (default 240), `--append "WORDS"`, `--enforcing` (makes the enforcing entry
the default), `--product DIR`. The build's `super.img` is an Android sparse image (Cuttlefish
unsparses it in place when it runs, so it may also be raw): the script handles both.

GPT layout (12.7 GiB; needs a stick of at least 14 GB, i.e. a "16 GB" stick):

| Partition | Size | Contents |
|---|---|---|
| `esp` | 256 MiB | FAT32 `ANDROIDPC17`: Limine `EFI/BOOT/BOOTX64.EFI`, `boot/limine/limine.conf`, `android/kernel`, `android/initrd.img` (vendor ramdisk + generic ramdisk + bootconfig), `android/bootconfig.txt` |
| `pclog` | 256 MiB | FAT32 `PCLOG`, boot logs |
| `misc`, `frp` | 1 MiB | zero |
| `metadata` | 64 MiB | zero (formatted on first boot) |
| `vbmeta_a`, `vbmeta_system_a`, `vbmeta_vendor_dlkm_a`, `vbmeta_system_dlkm_a` | 1 MiB | the build's images |
| `boot_a`, `init_boot_a`, `vendor_boot_a` | 64/8/64 MiB | the build's images (AVB reads them) |
| `super` | 8 GiB | `super.img` |
| `userdata` | 4 GiB | zero (fs_mgr formats a wiped formattable partition) |

Limine menu (5 s): **SELinux permissive** (default, logs to `pclog`), **SELinux
enforcing**, **kernel log on screen** (`earlycon=efifb keep_bootcon`, slow, for boots
that stop before Android draws), **no pcfb** (to tell a display problem from the rest).

Writing it (erases the whole stick):

```bash
lsblk -o NAME,SIZE,MODEL,TRAN
```
```bash
sudo dd if=out/aosp17-usb.img of=/dev/sdX bs=4M conv=fsync oflag=direct status=progress
```

Firmware settings as in [usb-boot.md](usb-boot.md) section 3 (Secure Boot off, CSM off,
Fast Boot off), then F12 and the `UEFI: <stick>` entry.

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
SELinux enforcing. Remaining noise: `com.android.bluetooth` crash-loops (Cuttlefish's BT HAL
on the placeholder `hvc5` never answers HCI reset: F5 replaces it), and a few sysfs
denials (USB HID `country`, wakeup source names, `discard_max_bytes`).

Cuttlefish (`aosp/cvd-launch.sh`) with the same images: `sys.boot_completed=1`, Launcher3
resumed, kernel `6.18.21-android17-5`, `pcfb`/`pchvc` loaded and idle, `virtio_gpu` card0
as before.

## 7. On the real PC

Not tried yet. What to report after a boot: plug the stick back into the build host and
run `tools/pclog-read.sh /dev/sdX` (or copy from the mounted `PCLOG` partition), plus
photos of the screen if Android did not draw. If the PC rebooted after a kernel panic
(`panic=10`), the panic is in `/sys/fs/pstore` on the next Linux boot (efi-pstore).

## 8. What the F4–F8 HALs will find (stock kernel)

| HAL | Device / sysfs | Notes |
|---|---|---|
| F4 health/power/thermal | `/sys/class/power_supply` (none on the PC: no `ACPI_AC`/battery driver loaded; Cuttlefish's health HAL reports a fake battery), `/sys/class/thermal/thermal_zone*` (`x86_pkg_temp`, ACPI `thermal`), `/sys/class/hwmon/hwmon*` (`coretemp`), `/sys/devices/system/cpu/cpufreq/policy*` (`intel_pstate`), `/sys/class/rtc/rtc0` | labels: `sysfs_thermal`, `sysfs_hwmon` need `genfscon` for the PC's ACPI/PCI paths |
| F5 Bluetooth | AX211 on USB `8087:0033`: the kernel's `btusb`+`btintel` take it as `hci0` (firmware `ibt-0040-0041`/`ibt-1040-0041`); a Rust HAL can use it through an HCI user channel socket (`AF_BLUETOOTH`, `HCI_CHANNEL_USER`) | Cuttlefish's HAL uses `/dev/hvc5` (here `pchvc`, never answers) |
| F6 audio | ALSA `/dev/snd/controlC0`, `pcmC0D0p` (ALC897 analog), HDMI PCMs of the RTX 4070 on another card; the webcam's microphone as its own card (`snd-usb-audio`) | `audio_device` label from the platform policy |
| F7 camera | `/dev/video0` (+ metadata node) from `uvcvideo` | `video_device` |
| F8 Wi-Fi | `wlan0` from `iwlmvm` (nl80211, firmware API 89 + PNVM) | Cuttlefish's Wi-Fi HAL expects `mac80211_hwsim`; `androidboot.wifi_impl` is not set on the PC |
| lights (F2) | `/sys/class/leds`, `/sys/class/backlight` (the desktop has none) | registered, 0 lights |
