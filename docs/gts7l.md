# gts7l — android-pc on the Galaxy Tab S7 LTE (SM-T875)

Work happens on branch `gts7l` (worktree `../android-pc-gts7l`). Builds use the shared
`aosp/` tree of the main checkout with `AOSP_DEVICE_SRC` pointing at this worktree, one
build at a time, through `tools/limited-build.sh` (see `CLAUDE.md`).

A new **android-pc** target for exactly one device: the Samsung Galaxy Tab S7 LTE (SM-T875,
`gts7l`) on the desk, serial `R52R504WFTK`. It shares android-pc's AOSP 17 tree, PcSystemUI,
HyprLauncher, hyprtile and `libpchal`, the way the Waydroid target shares them. No LineageOS,
no Samsung or Qualcomm userspace: every program that runs on the application processor is
built from source we own, and the vendor HALs are written in Rust. Other models or devices
are out of scope until needed.

## 1. The device (read over adb, 2026-11-23)

| Property | Value |
|---|---|
| Model / codename | SM-T875 / `gts7l`, CSC XXV, board revision 7 |
| SoC | Snapdragon 865+ (`kona`, SM8250), Adreno 650; LTE through an external X55 modem (`mdm1m9kefs*` partitions) |
| Stock firmware | `T875XXS8DXH1`, Android 13, bootloader revision 8, patch 2024-08-01 |
| Kernel (stock) | Samsung 4.19.113; display through Qualcomm's SDE driver (`sde-crtc-*`, `card0-DSI-1`, `card0-DP-1`) |
| Storage | UFS, dynamic partitions (`super` = `sda29`), non-A/B, separate `recovery`, `dtbo`, `vbmeta`, `vbmeta_samsung` |
| Input | `sec_touchscreen`, `sec_e-pen` (S Pen digitizer), `sec_touchpad` (Book Cover keyboard), power key, hall IC |
| Power | MAX77705 charger + fuel gauge, PCA9468 direct charger, wireless supply |
| Locks | Bootloader locked, verified boot green, Knox warranty 0, OEM unlock allowed |
| Virtualisation | No `/dev/kvm`: EL2 belongs to Qualcomm/Samsung's hypervisor, so no pKVM/AVF. Linux apps run through Termux / chroot, not a VM |

Panel and touch controller IDs are behind root; they are read from the stock device tree in T0.

## 2. Reference: the Tab S7+ Wi-Fi port

[JeyKul/gts7xl-mainline](https://github.com/JeyKul/gts7xl-mainline) runs mainline Linux 7.3
on the sibling Tab S7+ Wi-Fi (SM-T970, same SM8250 board family), and **AOSP 17 on that
kernel with no Samsung/Qualcomm userspace**: Mesa freedreno, drm_hwcomposer, minigbm_msm, its
own small HALs, firmware read from the tablet's partitions at runtime. Working there: display,
GPU, touch, S Pen, keyboard cover, Wi-Fi/BT (QCA6390), speakers (CS35L41), sensors through
the SLPI, charging (MAX77705), Venus video, cameras via libcamera on Linux. Not working:
fingerprint (owned by TrustZone), TrustZone keymaster.

It is GPL/Apache source we can fork and own. What differs on our SM-T875: the 11" LCD panel
(2560x1600, not the S7+'s AMOLED), possibly the touch controller, the side-key fingerprint,
and the LTE modem. Those are the device-specific work.

## 3. Ownership boundary

| Layer | Who owns it | Plan |
|---|---|---|
| Bootloader (PBL/XBL/ABL), TrustZone, hypervisor | Samsung/Qualcomm, signed | Untouched; we only boot through ABL |
| Co-processor firmware (ADSP, CDSP, SLPI, modem, GPU zap/GMU, Venus, Wi-Fi/BT, CS35L41 protection) | Samsung/Qualcomm, signed | **Not in our build or git.** Read at runtime from the tablet's own `apnhlos` and `modem`; the files of the stock vendor go into a local-only `gts7l_fw` partition made from the tablet's own data (§13); they run on the DSPs, not on Android |
| Linux kernel | **Ours** | Mainline v7.3 + our patch series (forked from gts7xl-mainline) + our `sm8250-samsung-gts7l.dts` |
| Android framework | **Ours** | android-pc's AOSP 17 tree and patches, PcSystemUI/Hypr |
| GPU userspace | Open source (Mesa freedreno/Turnip), built from source in our tree | A GPU driver in Rust is out of scope |
| Composer, gralloc | Ours | Start with drm_hwcomposer + minigbm (open source, from our tree); replace with Rust later (T7) |
| Vendor HALs | **Ours, Rust** | `libpchal` + `hals/` pattern of android-pc |

## 4. Target layout in android-pc

```
android-pc/
  aosp-device/
    AndroidProducts.mk          + aosp_gts7l (lunch aosp_gts7l-cp2a-userdebug)
    gts7l/                      new: the tablet target
      aosp_gts7l.mk             arm64 product: generic system + hypr.mk + desktop.mk (tablet variant) + Rust HALs
      BoardConfig.mk            arm64, boot header v2 (Image.gz + DTB section), super layout of sda29, no A/B, no recovery image
      fstab.gts7l, init.gts7l.rc, ueventd.gts7l.rc
      firmware/                 init script linking /vendor/firmware to apnhlos/modem/vendor partitions
      sepolicy/
      kernel/                   defconfig fragment, our patch series, sm8250-samsung-gts7l.dts
      hals.list                 which hals/* this target uses
    hals/<name>/                shared Rust HALs; new ones for the tablet (below)
  tools/gts7l-*.sh              kernel build, image/zip build, flash, update keeping userdata
  gts7l-kernel/                 mainline v7.3 checkout (gitignored, like gki/)
```

`aosp_pc_x86_64.mk` keeps inheriting Cuttlefish; the tablet product must not. `hals/*/hal.mk`
gets split so each target picks its HAL list without Cuttlefish switches.

## 5. Rust HALs

| HAL | Kernel interface | Source |
|---|---|---|
| health | `power_supply` (max77705-fuelgauge/charger) | android-pc `hals/health`, tablet config |
| power, thermal | cpufreq, devfreq, thermal zones | android-pc `hals/power`, `hals/thermal` |
| light | backlight | android-pc `hals/light` |
| vibrator | input force-feedback (regulator-haptic) | **new** |
| audio | ALSA (q6asm/q6afe via ADSP, CS35L41 amps), our routing table | android-pc `hals/audio`, new routing + per-tablet calibration from `sec_efs` |
| bluetooth | HCI (hci_qca over serdev) | android-pc `hals/bluetooth` |
| wifi | nl80211 (ath11k, QCA6390) | android-pc `hals/wifi` |
| sensors | SLPI over QRTR/QMI (accelerometer, gyro, light, magnetometer) | **new**, protocol from gts7xl `sensord` |
| keymint, gatekeeper | none (software, no TrustZone access from mainline) | AOSP's Rust KeyMint in software mode; userdata unencrypted until a secure path exists |
| usb | MAX77705 Type-C | **new**, small |
| camera | V4L2 / CAMSS raw sensors + soft ISP | android-pc `hals/camera` design, later phase |
| radio (LTE) | MHI + QMI to the X55 modem | **new**, later phase |
| gnss | BCM47531 (closed protocol) | last, may stay out |
| fingerprint | TrustZone only | out of scope |

## 6. Phases

### T0 — Read-only inventory (now, tablet stays stock)
- Download the stock `T875XXS8DXH1` XXV firmware with `samloader-rs`, keep it as the way back.
- Extract its `boot.img` DTB and `dtbo`: panel compatible/timings, touch controller, regulators,
  GPIOs, modem wiring; diff against gts7xl's DTS.
- List partition sizes (`super`, `dtbo`, `boot`, `param`) and the firmware files in
  `apnhlos`/`modem`/vendor that the kernel will need.

### T1 — Unlock and back up (irreversible step, needs the user's go-ahead)
- OEM unlock in Download mode (wipes data, trips Knox), flash a TWRP for gts7l.
- Full backup to the host of every partition we must never lose: `efs`, `sec_efs`, `persist`,
  `param`, `modemst1/2`, `fsg`, `dtbo`, `boot`, `vbmeta*`.

### T2 — Kernel bring-up (Linux first, Android later)
- Mainline v7.3 + gts7xl patch series + our `sm8250-samsung-gts7l.dts`.
- Booted from ABL (header v1, Image.gz + appended DTB, blank `dtbo` header, `param` byte
  cleared), first with an initramfs that gives a USB gadget shell.
- Port in order: storage, USB gadget, panel + backlight, touch, Wi-Fi/BT, GPU, audio, SLPI,
  S Pen, keyboard cover, charging.

### T3 — `aosp_gts7l` boots to PcSystemUI
- Product and board files of §4, drm_hwcomposer + minigbm_msm + Mesa freedreno/Turnip.
- Minimal Rust HALs: health, power, thermal, light, vibrator.
- Done when HyprLauncher and PcSystemUI run on the panel with touch.

### T4 — Rust HALs for the rest
- Audio, Wi-Fi, Bluetooth, sensors, USB, KeyMint/gatekeeper (software), each with host
  tests in `libpchal` style and device tests.

### T5 — Tablet desktop in PcSystemUI/hyprtile
- Portrait/landscape with rotation, touch gestures (swipe workspaces, edge gestures), bar sized
  for 2560x1600 @ 11", S Pen input, Book Cover keyboard/trackpad as the keybind source,
  on-screen keyboard when the cover is detached.
- Linux apps: Termux (native arm64, already in android-pc) + Termux:X11 and a chroot
  (Debian/Arch) with Turnip for GUI apps.

### T6 — Install and update
- A TWRP zip: checks it is this SM-T875, writes `boot` and `super`, blanks `dtbo`, clears the
  TWRP flag in `param`, never touches `userdata`. Updates keep data, like android-pc's USB
  stick update.

### T7 — Later
- Cameras (libcamera-style soft ISP in Rust), LTE radio, composer and gralloc in Rust,
  suspend power draw, GNSS.

## 7. Risks

- Unlocking wipes the tablet and trips Knox permanently.
- Losing `efs`/`sec_efs`/`persist` loses IMEI, calibration and the Bluetooth address: back up
  before writing anything.
- Never Odin-flash firmware older than bootloader revision 8.
- Samsung's bootloader falls back to Download mode when it rejects an image; stock firmware
  through `samloader-rs` is the way back.
- Without TrustZone keymaster, `/data` starts unencrypted and Play Integrity stays basic.

## 8. Work breakdown

Each step lands as its own commits on `gts7l`. Steps in the same group can run in
parallel sessions (they touch different files); builds still go one at a time.

| Step | Phase | What | Needs | Done when |
|---|---|---|---|---|
| S1 ✅ | T0 | Stock firmware: download `T875XXS8DXH1` XXV, extract `boot`/`dtbo`/`vendor_boot` DTBs to DTS, list partitions and firmware files; write `docs/gts7l-inventory.md` | tablet over adb (read-only) | panel, touch, regulators, modem wiring documented; firmware kept under `gts7l-stock/` (gitignored) |
| S2 | T2 | Kernel tree: `tools/gts7l-kernel.sh` fetches mainline v7.3 into `gts7l-kernel/`, applies the gts7xl patch series kept in `aosp-device/gts7l/kernel/patches`, builds `Image.gz` + DTB with an arm64 cross toolchain | nothing (DTS from S1 later) | `Image.gz-dtb` for `sm8250-samsung-gts7xl` builds; `sm8250-samsung-gts7l.dts` stub ready for S1's data |
| S3 | T3 | Product scaffolding: `aosp_gts7l` in `AndroidProducts.mk`, `gts7l/aosp_gts7l.mk`, `BoardConfig.mk`, fstab/init/ueventd, sepolicy, `hals.list`; `hals/*/hal.mk` split so the PC product keeps Cuttlefish and the tablet does not | nothing | `lunch aosp_gts7l-cp2a-userdebug` passes Soong analysis; PC product unchanged |
| S4 ✅ | T3 | Image tooling: `tools/gts7l-image.sh` packs `boot.img` (header v2 as stock, Image.gz + DTB section, our ramdisk) and `super.img` sized for `sda29`, plus `vbmeta` disabled | S2, S3 | `out/gts7l/boot.img` + `super.img` produced from a build |
| S5 ✅ | T1 | Unlock, TWRP, full partition backup | **user's go-ahead** (wipes tablet, trips Knox) | backups of §6/T1 partitions on the host, checksummed |
| S6 🟡 | T2 | Boot S2's kernel with a USB-gadget-shell initramfs, then port drivers in order | S1, S2, S5 | shell over USB on the tablet |
| S7 | T3 | First `aosp_gts7l` boot to PcSystemUI | S3, S4, S6 | HyprLauncher on the panel with touch |
| S8.. | T4-T7 | Rust HALs, tablet desktop, TWRP update zip, later items | S7 | per §6 |

Group A (now, in parallel): S1, S2, S3. Then S4. S5 waits for the user. S6 and S7 follow.

## 9. Product scaffolding (S3)

`aosp-device/gts7l/` holds the `aosp_gts7l` product (`lunch aosp_gts7l-cp2a-userdebug`):

| File | What |
|---|---|
| `aosp_gts7l.mk` | arm64 64-bit-only generic system + handheld/telephony system_ext + aosp_product, handheld vendor; `desktop.mk` with `PC_DESKTOP_VARIANT := tablet`, `hypr.mk`; drm_hwcomposer APEX, minigbm AIDL allocator + stable-C mapper; Mesa from `device/android-pc/gts7l-mesa` when present; software KeyMint/gatekeeper; kernel `Image.gz` and DTB from `device/android-pc/gts7l-kernel` (copied from `out/gts7l/` by `tools/aosp-device-link.sh`) or `prebuilt/kernel-placeholder` |
| `BoardConfig.mk` | Soong-only build (set in `aosp_gts7l.mk`; with Kati the analysis exceeds the 20 GB cap), kona, armv8-2a-dotprod/cortex-a76, header v2 with the DTB section and stock offsets (boot 71303168 B), non-A/B, no recovery image, `super` 10171187200 B with one 10166992896 B group of system/system_ext/product/vendor (ext4), AVB off, SELinux permissive for bring-up (`GTS7L_SELINUX_PERMISSIVE`) |
| `fstab.gts7l` | logical partitions (first-stage mount), metadata, userdata unencrypted (f2fs, ext4), apnhlos/modem vfat read-only, efs/sec_efs/persist read-only |
| `init.gts7l.rc`, `ueventd.gts7l.rc` | modules, `mount_all`, schedutil; firmware directories, backlight |
| `firmware/init.gts7l.firmware.rc` | `/mnt/vendor/firmware/gts7l/{modem,apnhlos,vendor}` links into the partitions; the kernel loads from there (`firmware_class.path`) and ueventd falls back to it |
| `sepolicy/` | firmware/efs mount types, block devices, the link tree |
| `hals.list`, `hals.mk` | Rust HALs of the tablet: health, power, thermal, light |

Each `hals/<name>/hal.mk` now only adds packages (any product); `hals/<name>/hal-pc.mk` holds the
PC product's Cuttlefish switches and PC-only files (health, light, thermal, power, audio).
`aosp_pc_x86_64.mk` reads both for every HAL, so its package list is unchanged; `aosp_gts7l.mk`
reads only `hal.mk` of the HALs in `hals.list` and inherits nothing of Cuttlefish.
Checked 2026-11-23: `m nothing` passes for `aosp_gts7l-cp2a-userdebug` and for the PC product,
whose dumpvars output (packages, copy files, overlays, properties, sepolicy dirs) is
byte-identical before and after the split; the four HALs of `hals.list` build for arm64
(they have no x86-only code; the power HAL only finds no `intel_pstate`). `hals/nvgpu`'s
`vulkan.nvidia` is now x86_64-only like `nvglibc`, so arm64 analysis does not trip on it.

Open TODOs:

- S1 data is in (super and boot sizes, header v2 and offsets, docs/gts7l-inventory.md); still
  open: the userdata file system in `fstab.gts7l`, which partition holds each firmware file.
- Device firmware: done, §13 (`gts7l_fw` partition, `tools/gts7l-firmware.sh`).
- S2/S6: the kernel needs ext4, f2fs, vfat; check `androidboot.boot_devices=soc@0/1d84000.ufshc` and
  `androidboot.usbcontroller=a600000.usb`.
- T3: Mesa freedreno/Turnip is done (`tools/gts7l-mesa.sh`, §14); minigbm AIDL
  allocator/mapper with the msm backend; power HAL
  on cpufreq/devfreq (it only knows `intel_pstate`, hints are ignored without it).
- S4: `boot.img` packed with the stock offsets (the product is Soong-only, as the PC one is, and
  Soong's boot.img ignores `BOARD_MKBOOTIMG_ARGS`), `super.img`, disabled `vbmeta`.
- S7: sepolicy compiled and checked on the device, then enforcing; density/orientation.

## 10. Images (S4)

`tools/gts7l-image.sh` builds system, system_ext, product, vendor and the generated ramdisk
(one step at a time, under the shared lock) and packs `out/gts7l/images/` of the main checkout:
`boot.img` (header v2, `Image.gz`, ramdisk + `fstab.gts7l`, gts7l DTB, stock offsets),
`super.img` (sparse, 10171187200 B, group `gts7l_dynamic_partitions`), `vbmeta.img` (flags 3),
`dtbo.img` (zeroed) and `AP_gts7l-<build>.tar.md5` for Odin's AP slot (those four images only;
userdata, efs and firmware partitions stay). `tools/gts7l-image.sh pack` repacks without
building. The script and the build wrappers run from a git worktree: device files from the
worktree, `aosp/` and `out/` from the main checkout.

First build 2026-11-23 (`eng.caoanh.20261123.1606`): system 861 MB, system_ext 215 MB, product
313 MB, vendor 44 MB; super.img 1.4 GB sparse; Odin tar 969 MB.

Soong-only quirks found on the way: the generated `boot.img` is GKI-style with no ramdisk; the
generated ramdisk (`aosp_gts7l_generated_ramdisk_image`, no phony target) drops ramdisk
`PRODUCT_COPY_FILES`, so the script appends `fstab.gts7l`; nothing is staged under
`$PRODUCT_OUT/system`, so build props are read from `system.img`.

Not in this image yet: panel/touch drivers (S6). Mesa (§14) is in vendor, and the device
firmware (§13) goes into `super.img` as `gts7l_fw` when `tools/gts7l-firmware.sh` has made it.
It is the first-boot test image for S6/S7,
flashed only after S5.

## 11. Unlock and backup (S5, 2026-11-23)

- Bootloader unlocked from the Download-mode warning screen (long press Volume Up). The first try
  only entered Download mode: `OEM LOCK : ON (U)` and Odin's `FAIL! Auth` ("Custom Binary Blocked
  By OEM Lock"); after the real unlock the screen shows `OEM LOCK : OFF (U)`. No `KG STATE` line,
  `FRP LOCK : OFF`.
- Flashing from Linux with [samloader-rs](https://github.com/topjohnwu/samloader-rs) built from
  source (`samloader flash --no-reboot -p VBMETA ... -p RECOVERY ...`); the PIT is in
  `gts7l-stock/s5/gts7l.pit`. Partition names in the PIT: `VBMETA`, `RECOVERY`, `BOOT`, `SUPER`, `DTBO`.
- `vbmeta`: ours (`out/gts7l/images/vbmeta.img`, unsigned, flags 3, rollback index 0); ABL
  accepts it once unlocked (verified boot state `orange`).
- `recovery`: no TWRP exists for gts7l, so the LineageOS 20261117 gts7l recovery is used as a tool
  only (root adb). It stays until our own recovery replaces it (T6).
- `tools/gts7l-backup.sh`: 72 partitions (all but userdata, super, cache) read with dd and their
  SHA-256 compared on the tablet and the host: all OK, in `gts7l-backup/20261123-2230/` (main
  checkout root, gitignored, read-only, `SHA256SUMS`). Keep a second copy off this machine.

## 12. First mainline boot (S6, 2026-11-23)

`tools/gts7l-bringup.sh BUSYBOX` packs `out/gts7l/bringup/boot.img`: header v1, page 2048, base
0x10000000, `Image.gz` + gts7l DTB, a busybox initramfs (`aosp-device/gts7l/bringup/init`) with
`usbroute` (Rust, MAX77705 USB data switch) and a configfs gadget: ACM serial + NCM
(`172.16.42.1`, telnet). It never mounts UFS. Busybox: Debian `busybox-static` 1.38.0 arm64.

Flashing from the recovery's root adb, as on gts7xl-mainline: zero `boot`, dd the image, read
back and compare SHA-256; zero the first 4 KiB of `dtbo`; clear byte 0 of `param`.
**Rebooting from the recovery with `adb reboot` boots the recovery again** (ABL passes
`androidboot.boot_recovery=1`; the recovery's 4.19 kernel sets the flag on its way down). A sysrq
reboot (`echo b > /proc/sysrq-trigger`) after clearing `param` boots `boot`.

Result: `Linux 7.3.0-gts7l` up, gadget on the host 12 s after reset (host side: NetworkManager
connection `gts7l-bringup`, 172.16.42.2/24 on the NCM interface; `/dev/ttyACM0` needs the `uucp`
group). Stable, no pstore records. Seen:

| Works | Not yet |
|---|---|
| 8 CPUs with cpufreq, 5.5 GiB RAM, 25 thermal zones | remoteprocs (ADSP/CDSP/SLPI): none registered, no firmware in the initramfs |
| UFS: sda-sdd, 82 partitions | PCIe (QCA6390 Wi-Fi): `qcom-pcie: cannot initialize host` (later: the pwrctrl module was missing, §23) |
| USB gadget (DWC3 peripheral) after `usbroute` | display: `msm_dpu` binds (DP only), DSI and the NT36523 panel are off, no simpledrm/fbdev |
| Book Cover keyboard, touchpad, tablet-mode switch (STM32 pogo, i2c-9) | touch (Novatek, i2c-5 0x62): node disabled in the DTS (driver since §17) |
| power key, volume down (resin), gpio-keys, regulator haptics | S Pen (i2c-17 0x56): deferred on `spen-3v3-regulator`, GPIO from the S7+ wiring |
| MAX77705 charger and Type-C, CS35L41 x4 (i2c-11), PS5169 (i2c-14) probed | `lcd-buck-regulator`: GPIO from the S7+ wiring; fuel gauge deferred (`max17042: temp channel`) |

Raw logs: `gts7l-stock/s6/dmesg-1.txt`, `gts7l-stock/s6/survey-1.txt`. Next in S6: the DTS GPIOs
from the stock dtbo (lcd buck, S Pen 3.3 V), PCIe/Wi-Fi, then the NT36523 panel driver and touch.

### Panel bring-up status (2026-11-23, kernel #6)

Patches 0095 (NT36523 entry for the CSOT PPA957DB1 panel, stock on-commands as a table, DCS
backlight 4..462) and 0096 (DTS: S Pen supply from pm8150 L13 at 3.008 V instead of GPIO 93, which
is the LCD bias buck; DSI0/DSI1 and both C-PHYs enabled). 0097-0099 are DEBUG switches in
`/sys/module/panel_novatek_nt36523/parameters` (`skip_reset`, `skip_init`, `skip_disable`,
`cmd_hs`, `init_limit`, `init_dsi`), applied on the next fbdev blank/unblank; drop them once
the panel works. `tools/gts7l-netflash.sh` writes a boot image from the running bring-up
shell over the NCM link and reboots into it (no recovery round trip).

- S Pen (Wacom W90xx) probes; the LCD buck regulator probes.
- DSI: both hosts bound, connector 1600x2560 connected, clocks as stock (VCO 997.96 MHz,
  pclk 285.13 MHz, byte 142.57 MHz, DSI1 from DSI0's PLL), no DPU underruns.
- With the bootloader's panel state kept (no reset, no init): the panel shows our frames,
  but only on the DSI0 half and only the first ~100-150 lines of each frame; below that the
  panel repeats garbage lines. The DSI1 half stays black. Writing noise to `/dev/fb0` changes
  the top lines, so video reaches the panel.
- Full reset + stock init: black, backlight off. The init tests with blank/unblank were
  invalid until `skip_disable` (disable sent display off + sleep in).
- Suspects: C-PHY signal tuning that the stock DT sets and mainline does not
  (`samsung,phy_vreg_ctrl_0`, `phy_ctrl2`, `phy_offset_{top,mid,bot}_ctrl`,
  `phy_str_swi_cal_sel_ctrl`, stock phy-timings `[00 24 0a 0a 11 3e 0d 1e 0a 02 04 00 1e 1a]`),
  `DSI PLL(0) lock failed` at PHY probe, and DSI1 never starting.

Photos: `gts7l-stock/s6/panel-test1.jpg`, `panel-k6.jpg`, `panel-pattern.jpg`.

Kernel #7 (patch 0100, DEBUG: stock Samsung C-PHY analog values `vreg_ctrl_0 0x50`,
rescode offsets top/bot/mid `0x1f`, `str_swi_cal_sel 0x04`, `ctrl_2 0x64`, module parameters
`/sys/module/msm/parameters/cphy_*`): the 4-quadrant test pattern shows the right colours and
orientation (portrait top-left = red "1", yellow border) on the DSI0 half. The left ~40 px are
right on every line, the full line width only for the first ~150 lines; the rest of each line
turns white. DSI1 half still black. Next: the stock PHY timings instead of mainline's computed
ones, burst vs. non-burst video, then DSI1. Photo: `gts7l-stock/s6/panel-pattern-k7.jpg`.

### Panel bring-up, kernels #8-#10 (2026-11-24, night run)

**Nothing in this section was verified on the screen.** The webcam (`/dev/video0`) showed the
user and then a dark room, never the tablet, so every picture result below is unverified; the
checks were made over the USB shell (kernel log, DSI/PHY registers, DCS reads).

Kernel #8 (patch 0102 now, DEBUG), built with `CONFIG_DEVMEM=y` added to the fragment by hand
(not committed; `busybox devmem` needs it), plus
- `/sys/module/msm/parameters/cphy_timing`: the 14 stock phy-timing bytes for the C-PHY
  `TIMING_CTRL` registers (bytes 12/13 to the host's `CLKOUT_TIMING_CTRL`, as downstream);
- `/sys/module/msm/parameters/dbg_flags_{set,clear}`: `MIPI_DSI_MODE_*` bits forced in
  `dsi_ctrl_enable()` (burst/sync pulse/`NO_HSA/HBP/HFP`/`CLOCK_NON_CONTINUOUS`/LPM);
- `/sys/module/panel_novatek_nt36523/parameters/dcs_read` (`echo 0x0a > ...`): reads a DCS
  register over each link and logs it.

Findings (bootloader panel state kept, computed timings):
- Mainline's C-PHY timings at 997.96 Msym/s: `hs_exit 39, clk_pre 43, clk_prepare 10,
  clk_post 26, hs_rqst 11`; stock writes `hs_exit 0x11, 0x3e, 0x0d, 0x1e, 0x0a` (TIMING_CTRL_4..8)
  and `CLKOUT_TIMING_CTRL` 0x1e/0x1a.
- Link budget: the byte clock (142.57 MHz, 16 bits x 3 trios = 6 bytes per cycle) carries exactly
  the pixel rate (285.13 MHz x 3 bytes) including blanking: a line is 458 byte clocks, the
  800-pixel payload 400, leaving 58. Both timing sets need about 130 byte clocks for an HS
  exit/entry, so the link cannot drop to LP inside a line in either case; the timings can only
  matter at frame boundaries. This weakens the "per-line LP overhead" explanation of the
  ~150-good-lines symptom.
- Both DSI controllers are programmed alike and match the stock mode per link: active 800x2560,
  h total 916 (HSA 2, HBP 30, HFP 84), v total 2594 (VSA 1, VBP 7, VFP 26), `VID_CFG0 0x9230`
  (burst, RGB888, BLLP/EOF-BLLP LP), `CPHY_MODE_CTRL 1`, lanes 0-2 out of stop state (HS), no
  FIFO under/overflow, no ACK errors, `LANE_CTRL` with the clock-lane HS force request
  (continuous clock; stock is non-continuous). DSI1's `STATUS0` has bit 13 set, DSI0's not.
- DCS reads: DSI1 answers (`0x0a` = `0x9e`: booster on, sleep out, normal mode, display on;
  `0x0e` = `0x80`; `0x05` error count 1 at boot, then 0). DSI0 reads return no data (also the
  backlight's `actual_brightness`), so on this panel only the DSI1 port answers reads: DSI1's
  link works in LP and BTA, its black half is a video-path problem.
- PHY common registers differ between the links only in `CLK_CFG0/1` (0x10/0x14) and 0x3c, as
  expected for the slave PLL.

Hazards found:
- Reading the DPU `INTF_1` registers (0x0ae6a800) with `devmem` rebooted the tablet.
- Kernel #9 dumped the bootloader's DSI/PHY/INTF registers from the first DSI PHY probe (guarded
  by the dispcc `CBCR` CLK_OFF bits of the AHB and MDP clocks). It never came back: no USB
  device since 00:04:45, so the tablet hangs or boot-loops in the early boot and needs a
  person. The dump code is not in the series (`gts7l-stock/s6/k9-bootloader-dump.diff`).

**Recovery (needs the user):** force a restart (Power + Volume Down, ~7 s), enter the
recovery (LineageOS, root adb) or Download mode, and write `gts7l-stock/s6/boot-k10-good.img`
(kernel #10: the code and config of the tested kernel #8, 16314368 bytes, SHA-256
`1bbd9e45a204868a...`) to `boot` as in section 12 (zero
`boot`, dd, compare SHA-256, clear byte 0 of `param`, sysrq reboot), or from Download mode with
`samloader flash -p BOOT`. Then `tools/gts7l-netflash.sh` works again.

Series after this run:
- 0100 (clean): `drm/msm/dsi/phy: 7nm` reads optional `qcom,cphy-{vreg-ctrl-0,ctrl-2,
  str-swi-cal-sel,rescode-offset-{top,bot,mid}}` from the PHY node (binding updated) and uses
  them in C-PHY mode; 0101 (clean): the stock values in the gts7l DTS for both PHYs. Same
  register values as the old DEBUG 0100 (kernel #7, verified on screen: right colours), but
  the clean build itself (#11, `gts7l-stock/s6/boot-k11-untested.img`, also the current
  `out/gts7l/bringup/boot.img`) was never booted: flash `boot-k10-good.img` first.
- 0097-0099 and 0102 stay DEBUG: the panel does not work from a cold reset yet, so there are
  no verified values to turn them into clean patches.

Next, once the tablet is back and the webcam sees it:
1. `echo 0,0x24,0x0a,0x0a,0x11,0x3e,0x0d,0x1e,0x0a,0x02,0x04,0x00,0x1e,0x1a >
   /sys/module/msm/parameters/cphy_timing`, blank/unblank (with `skip_disable`), photo.
2. `dbg_flags_set=0x400` (non-continuous clock, as stock), then sync-pulse/sync-event modes
   (`dbg_flags_clear=0x2`, `dbg_flags_set=0x4`), each with a photo.
3. DSI1 black: compare the DPU INTF_2/CTL setup with the bootloader's (read it safely: from
   the DPU driver after it takes its clocks, not with `devmem`), and check that the panel's
   second port gets video at all (DSI1 `STATUS0` bit 13).

## 13. Device firmware (`gts7l_fw`)

The co-processors need firmware that is Samsung/Qualcomm/Cirrus-signed and must never go into
git. The tablet holds it in three places: `apnhlos` (vfat: ADSP, CDSP, SLPI, Venus, GPU zap,
IPA), `modem` (the X55) and the stock `vendor` in `super`. Our `super` replaces that vendor,
which had the only copies of the GPU microcode (`a650_sqe.fw`, `a650_gmu.bin`), the QCA6390
Wi-Fi (`qca6390/*`) and Bluetooth (`htbtfw20.tlv`, `htnv20.bin`) firmware and the CS35L41
speaker-protection firmware.

**Where it lives: a `gts7l_fw` logical partition in `super`**, ext4, read-only, about 11 MiB,
mounted at `/mnt/vendor/firmware` (`fstab.gts7l`, `wait,logical,nofail`). Why:

- Updates stay userdata-safe: every update rewrites `super` whole (Odin AP or the netflash
  path), and `gts7l_fw` is packed into it each time, so the firmware always matches the
  image and never depends on what `/data` holds. `userdata`, `efs`, `persist` and the firmware
  partitions are never written.
- A copy on `/data` at first boot has no source once our `super` is flashed (the stock
  vendor is gone from the tablet), and a factory reset would lose it. `persist` is Samsung's
  sensor calibration (32 MiB) and stays read-only.
- No first-boot logic: first-stage init maps every logical partition of `super`, so
  `mount_all` mounts `gts7l_fw` like any partition.
- The bring-up initramfs cannot map `super` (no dm-linear in busybox), so it gets the same
  tree as real files (`tools/gts7l-bringup.sh --firmware`, `/lib/firmware`, ~23 MB gzip; the
  boot image stays under 40 of the 68 MiB of `boot`).

The cost: `super.img` and the Odin tar now contain this tablet's firmware. They are built and
kept on this host only and are never published (`out/` is gitignored).

**`tools/gts7l-firmware.sh`** reads only host copies of the tablet's own data, checked against
their checksums: `gts7l-backup/<latest>/` (S5: `apnhlos.img`, `sec_efs.img`; `modem.img` and
`dsp.img` only listed), the stock `vendor.img` of the `T875XXS8DXH1` Odin package
(`gts7l-stock/ap/super/vendor.img`) and the kernel's DTB. Output in `out/gts7l/firmware/`:
`tree/` (real files), `gts7l_fw.img`, `MANIFEST.tsv` (path, size, sha256, source, driver, form),
`SHA256SUMS`, `SOURCES`, `partitions/{modem,dsp}.txt`. Names are the ones the kernel requests:

| Path | Source | Driver |
|---|---|---|
| `qcom/sm8250/gts7xl/{adsp,cdsp,slpi}.mbn` + `.bNN`, `*.jsn` | apnhlos `<dsp>.mdt`, segments, PD maps | `qcom_q6v5_pas`; pd-mapper (userspace) |
| `qcom/sm8250/gts7xl/venus.mbn` + `.bNN` | apnhlos `venus.*` | `qcom_iris` |
| `qcom/sm8250/a650_zap.mbn` + `.bNN` | apnhlos `a650_zap.*` | `msm` (zap shader) |
| `qcom/a650_sqe.fw`, `qcom/a650_gmu.bin` | vendor | `msm` (Adreno 650) |
| `ath11k/QCA6390/hw2.0/{amss,m3,board,regdb}.bin` | vendor `qca6390/{amss20.bin,m3.bin,bdwlan.elf,regdb.bin}` | `ath11k_pci` |
| `qca/htbtfw20.tlv`, `qca/htnv20.bin` | vendor | `btqca` (hci_uart) |
| `cirrus/cs35l41-dsp1-spk-prot.{wmfw,bin}` | vendor `cs35l40-spk-dsp1-spk-prot.*` | `snd-soc-cs35l41` (wm_adsp) |
| `cirrus/cs35l41-cal-11-00{40..43}.bin` | made from `sec_efs:/cirrus/*_cal*` (rdc, temp, vsc, isc; suffix per address from the stock DT: 40 `_r`, 41 none, 42 `_br`, 43 `_b`) | `snd-soc-cs35l41` (our calibration patch) |
| `ipa_fws.*`, `ipa_uc.*` | apnhlos (TZ rejects the vendor copies) | `ipa` (node not enabled yet) |

The remoteproc, Venus and zap names are read from the DTB's `firmware-name` properties, so the
layout follows the DTS (today it still uses the S7+'s `gts7xl/` directory). In `gts7l_fw.img`
the vendor-only and generated files are real files and the apnhlos ones are symlinks to
`/mnt/vendor/apnhlos/image/<file>`, so Android still reads those from the partition; the
script checks every manifest entry in the image. `gts7l/{apnhlos,modem,vendor}` links stay.
Not laid out: `modem` (X55 over MHI, T7) and `dsp` (FastRPC shared objects, userspace; no
fstab entry yet).

Android side: `firmware/init.gts7l.firmware.rc` writes `/mnt/vendor/firmware` to
`/sys/module/firmware_class/parameters/path` at early-init (the kernel's forced built-in
command line says `/vendor/firmware`), and `init.gts7l.rc` loads the kernel modules after
`mount_all --early`, so remoteproc, ath11k, cs35l41 and btqca find their firmware at probe.
`tools/gts7l-image.sh` adds `gts7l_fw` to `super.img` when the image exists
(`GTS7L_FIRMWARE=0` leaves it out).

```
tools/gts7l-firmware.sh                      # out/gts7l/firmware/
tools/gts7l-bringup.sh --firmware BUSYBOX    # bring-up boot.img with /lib/firmware
tools/gts7l-image.sh pack                    # super.img with gts7l_fw
```

## 14. GPU userspace: Mesa freedreno and Turnip (T3)

`tools/gts7l-mesa.sh` builds Mesa from AOSP 17's own `external/mesa3d` (26.1.0-devel) for
Android arm64: freedreno (gallium, EGL + GLES 1/2/3) and Turnip (Vulkan) on the msm KMD, with
a static libdrm from `external/libdrm` linked in. Output: `out/gts7l-mesa/prebuilt/` of the
main checkout, the stripped libraries plus a generated `Android.bp` (vendor
`cc_prebuilt_library_shared`); `tools/aosp-device-link.sh` copies it to
`aosp/device/android-pc/gts7l-mesa`, and `aosp_gts7l.mk` adds the packages and properties
when that `Android.bp` exists.

| On vendor (lib64) | |
|---|---|
| `egl/libEGL_mesa.so`, `egl/libGLESv1_CM_mesa.so`, `egl/libGLESv2_mesa.so` | `ro.hardware.egl=mesa` |
| `libgallium_dri.so` | the freedreno gallium driver the three link |
| `hw/vulkan.freedreno.so` | `ro.hardware.vulkan=freedreno`; Vulkan 1.3 / level 1 features declared |

Why not Soong: `external/mesa3d`'s `Android.bp` files are generated by meson2hermetic and
cover only lavapipe, zink and gfxstream; generating freedreno/Turnip ones would need that
meson fork and a patch set to the mirror. Mesa's `Android.mk` meson wrapper (LineageOS
`external/mesa`, used by gts7xl-mainline) needs Kati, and `aosp_gts7l` is Soong-only. So this
is Mesa's upstream Android route: meson cross build with the NDK (r30 from the host package
`android-ndk`, `/opt/android-ndk`, API 37) and `-Dandroid-stub=true`, whose stub libraries only
satisfy the link; at runtime the libraries bind to the platform's `liblog`, `libcutils`,
`libsync`, `libhardware`, `libnativewindow` and `libz` (nothing else is `NEEDED`; libc++ is
static). The script never writes into the AOSP tree (`--wrap-mode=nofallback`, no
`__pycache__`); build time about 5 minutes on the 8 E-cores.

Gralloc: Mesa built this way has no IMapper path, so `u_gralloc` reads buffers through the
CrOS gralloc0 `perform()`: the product installs `gralloc.minigbm` (found by libhardware via
`ro.hardware.gralloc=minigbm`) next to the AIDL allocator and stable-C mapper, all on minigbm's
generic backend (linear buffers). The msm backend for UBWC is still a TODO; Mesa needs no change
for it. SELinux labels for `/dev/dri` and the libraries (`same_process_hal_file`) are in
`sepolicy/file_contexts`.

Checked 2026-11-24: the Mesa modules and `gralloc.minigbm` build through Soong, `vendor.img`
(75 MB, was 44 MB) holds all five libraries, the properties and the Vulkan/GLES permission
files, and `tools/gts7l-image.sh pack` packs it into `super.img`. Not yet run on the device.

What the kernel and firmware must provide (S2/firmware session): the msm DRM driver with the
Adreno 650 GPU node enabled (one device: `card0` for KMS, `renderD128` for Mesa), the GPU's
`qcom,gmu` and zap-shader nodes, and the firmware `qcom/a650_sqe.fw`, `qcom/a650_gmu.bin` plus
the device's signed zap shader (the DTS `firmware-name`, from the stock vendor, never in git)
under the kernel's firmware path. Without the zap shader the GPU stays in secure mode and
`msm` fails to bring it up.

### Panel bring-up, kernels #10-#13 (2026-11-24 morning, verified on screen)

The tablet came back through the LineageOS recovery (`boot-k10-good.img` written to `boot` as in
section 12). Kernel #10 ran fine. From here on, every result was checked with a webcam photo.
The webcam is held by OBS, so the photos are grabs of OBS's preview
(`gts7l-stock/s6/pt/obscap`). They are in `gts7l-stock/s6/cam/`. The camera sees the tablet
in landscape, so the DSI0 half (portrait x 0-799) is the lower half of each photo.

| Test (kernel #10, bootloader panel state) | Result |
|---|---|
| Our test pattern, computed C-PHY timings | portrait rows 0-~160 right on the DSI0 half, the rest white; DSI1 half dark (`k10-bootstate-pattern.jpg`) |
| `cphy_timing` = stock bytes | no change (`k10-stock-phy-timings.jpg`) |
| `CLOCK_NON_CONTINUOUS` (as stock) | no change (`k10-noncont-clock.jpg`) |
| non-burst sync pulse | no picture at all (`k10-sync-pulse.jpg`) |
| non-burst sync event | as burst (`k10-sync-event.jpg`) |
| DSI1 PHY `CLK_CFG1` DSICLK_SEL 3, as on DSI0 | no change |
| `byte_intf` clocks / 2 (dispcc dividers 0x2128/0x2144) | slightly different garbage, not fixed (`k10-byteintf-div2.jpg`) |
| DSI controller test pattern generator | no checkered pattern; the panel decays to grey |
| DSI1 controller disabled | DSI0 half unchanged (`k10-dsi1-ctrl-off.jpg`) |
| stock init without reset (`skip_init=N`) | black, backlight off |

**Backlight bug, fixed (patch 0103, clean):** with `qcom,sync-dual-dsi`, msm only queues a
command sent to DSI0 and transmits it with the next DSI1 command. The nt36523 backlight wrote
to DSI0 only, so no brightness ever reached the panel. The stock init sets `51 00 00`, so
after the init the panel stayed dark. That was the "full reset + init: black" of kernel #6. Now
the brightness goes to both links and is read back from the first link that answers. Verified:
sysfs brightness 462 and 4 give clearly different screen brightness.

Kernel #13 (0103 + 0104 DEBUG `dcs_write`, built with `CONFIG_DEVMEM=y` by hand):
- **Cold reset + stock init now lights the panel** (`skip_reset=N skip_init=N`, blank/unblank):
  our frame, the same picture as with the bootloader state (`k13-cold-reset-init.jpg`).
  `0x0a` = `0x9e` afterwards.
- LP and HS DCS writes work: brightness 0 vs 0x1ce changes the photo's mean from 17 to 136.
- Measured with 20-line colour bands: exactly the first **~160 portrait lines** are right; from
  there on the DSI0 half is white (`k13-bands-160lines.png`). A solid colour makes the tail show
  wrong colours (blue becomes yellow/white), so data still arrives but is corrupted. The fault
  is deterministic and resets every frame.
- Both DSI controllers had `TIMEOUT_STATUS` = 1 (HS TX timeout): `HS_TIMER_CTRL` is 0xffff
  byte clocks = 460 µs ≈ 143 lines, and the link never returns to LP inside a frame (58 spare
  byte clocks per line). With the timer at resolution 0xf the status stays clear, but the picture
  is the same. `HS_TX_TO_STOP_EN` is off, so this timeout is not the cause.
- No effect either: `GLBL_HSTX_STR_CTRL_0` 0x00-0xff (`k13-hstx-sweep.jpg`), resync FIFO
  toggle (`RBUF_CTRL`), `HS_REQ_SEL_PHY` (`k13-hs-req-sel-phy.jpg`).
- The DPU is fine (debugfs `encoder-0/status`: both INTFs in video mode, no underruns; plane
  split 800+800 on SSPP 8/9).

Open:
- Why only ~160 lines arrive intact on DSI0, and why DSI1 never shows video. Only DSI1 answers
  reads, so DSI1 is probably the panel's primary port. If that port's video is not accepted, the
  panel may lose DSI0's data after a buffer of ~160 lines. The per-lane PHY registers (0xae94600
  and 0xae96600, 0x280 bytes) are identical on both links, LP RX enabled on lane 0 of both. Next:
  get the bootloader's DSI/PHY registers in a safe way (from the
  recovery's 4.19 kernel while it drives the panel, or from the DSI driver after it takes its
  clocks, never at PHY probe).
- 0097-0099, 0102, 0104 stay DEBUG. Clean: 0100/0101 (C-PHY tuning from DT) and 0103 (backlight).

The tablet runs kernel #13 (`gts7l-stock/s6/boot-k13.img`, SHA-256 `993fdd5ec1545d0c...`,
booted and tested). `boot-k10-good.img` stays the fallback.

## 15. First Android image: reachable without the screen (S7)

The panel shows only part of a picture, so the first `aosp_gts7l` boots are debugged over USB
alone. What the image and the flash path provide:

**Reachability in Android** (`init.gts7l.usb.rc`, `bringup/usbroute/`):

- `usbroute` is also a Soong `rust_binary` (`/vendor/bin/usbroute`, same source as the
  bring-up's cargo build). `gts7l-usb.sh` (vendor, domain `gts7l_usb`) runs it at `on init`,
  retrying for 30 s while the MAX77705 probes; without it the host never sees the gadget.
- configfs gadget `g1` on `a600000.usb` (`sys.usb.controller`, from `ro.boot.usbcontroller`),
  `sys.usb.configfs=1`, FunctionFS for adbd at `/dev/usb-ffs/adb`; AOSP's
  `init.usb.configfs.rc` binds it when adbd is ready. `persist.sys.usb.config=adb`,
  `ro.adb.secure=0` (userdebug, `verifiedbootstate=orange`: no key confirmation on a screen
  nobody sees), `service.adb.tcp.port=5555`.
- Two more functions stay in the configuration as `f8`/`f9` (the stock triggers only touch
  `f1`-`f3`), in the bring-up image's order so the host's interface is again
  `enp0s20f0u1i2` and the NetworkManager connection `gts7l-bringup` comes up by itself:
  `acm.gs8`, a root shell on the host's `/dev/ttyACM0` even without adbd
  (`vendor.gts7l-console`), and `ncm.gs9` with `172.16.42.1/24` (`adb connect
  172.16.42.1:5555`). If adbd has not bound the gadget after 90 s, `gts7l-usb.sh` binds it
  with ACM + NCM alone.
- USB IDs: Android `18d1:4ee7` (serial = the bootloader's `androidboot.serialno`), the
  bring-up shell `1d6b:0104`.

**Debugging** (`tools/gts7l-bringup.sh`, kernel 0105):

- The kernel forces its built-in command line (`CONFIG_CMDLINE_FORCE`), which carries
  `androidboot.init_fatal_reboot_target=recovery`, `androidboot.selinux=permissive`,
  `printk.devkmsg=on`; `BOARD_KERNEL_CMDLINE` lists the same values and
  `tools/gts7l-image.sh` checks the built-in one.
- `recovery.img` is the bring-up image with the firmware tree (`GTS7L_BRINGUP_OUT=
  out/gts7l/recovery tools/gts7l-bringup.sh --firmware BUSYBOX`). An init failure reboots
  there (IMEM reason `0x77665502`): telnet `172.16.42.1`, and `/sys/fs/pstore` holds the
  previous boot. The shell logs the list of records to the kernel log at start.
- pstore: ramoops at `0x9fa00000` (`sm8250-samsung-common.dtsi`): `console-ramoops-0`
  (256 KiB of kernel console, enabled whatever `console=` says), `pmsg-ramoops-0` (256 KiB:
  liblog writes every log buffer to `/dev/pmsg0` on a debuggable build; read with
  `logcat -L` on Android, or `strings` in the bring-up shell), `dmesg-ramoops-N` on a panic.
- Patch 0105 adds `mode-normal = <0x77665501>` (stock's value) to the IMEM reboot-mode, so a
  plain `reboot` after a recovery boot does not leave the stale recovery reason behind. Only
  `reboot`/`reboot -f` run the notifier, sysrq `b` does not. Unverified: whether ABL also
  keeps the PON spare register's recovery bit (`&pon mode-recovery = 1`, never cleared by a
  normal reboot); if a reboot from the recovery shell goes back to recovery, that is why.
- SELinux stays permissive (`GTS7L_SELINUX_PERMISSIVE`); `sepolicy/gts7l_usb.te` covers the
  USB script for later.

**Data**: Samsung's `userdata` is f2fs with Samsung's file-based encryption, unusable
without its TrustZone keys. `tools/gts7l-android-flash.sh` zeroes its first 16 MiB and all of
`metadata` (Samsung's encryption metadata for it); `mount_all` finds no superblock
(`partition_wiped`) on the `formattable` entries and formats them: userdata as f2fs with
`/system/bin/make_f2fs` (first matching line of `fstab.gts7l`), metadata as ext4 with
`/system/bin/mke2fs`; first-stage mount skips the unformatted metadata (`formattable`).
**Flashing therefore wipes `/data`.** That is fine now: it holds nothing since the unlock wipe
(S5). Later updates must not run this tool; they will need a path that keeps `userdata`.

**Flashing** from the host while the tablet runs the bring-up shell (from boot or recovery):

```
tools/gts7l-kernel.sh                                   # out/gts7l/
GTS7L_BRINGUP_OUT=out/gts7l/recovery tools/gts7l-bringup.sh --firmware BUSYBOX
tools/gts7l-image.sh                                    # out/gts7l/images/, CHECK.txt
tools/gts7l-android-flash.sh                            # asks for 'flash'; --yes skips
```

`tools/gts7l-android-flash.sh` first checks `SHA256SUMS`, the bring-up kernel, and every
partition's name (`PARTNAME` in sysfs) and size against its table (`boot` sda23, `recovery`
sda24, `super` sda29, `userdata` sda37, `metadata` sda28, `dtbo` sda22, `param` sda10; `vbmeta`
sdd25 only read: flags must be 3) and writes nothing if one differs. Then: `super` expanded
from the sparse image on the host while it streams (simg2img cannot write to a pipe; about
830 MB after `gzip -1`), `nc` + `gunzip` + `dd` on the tablet, SHA-256 of all 10171187200
bytes read back; `boot` and `recovery` like `tools/gts7l-netflash.sh` (pushed to `/tmp`,
partition zeroed, written, range read back); `userdata`/`metadata`/`dtbo` headers and `param`
byte 0 zeroed and read back; `reboot -f`. It then waits up to 10 min for Android's gadget
(`18d1:4ee7`) or the bring-up shell coming back (Android failed, see pstore). efs, sec_efs,
persist, modem*, apnhlos, dsp and the bootloader/firmware partitions are never written.

First build 2026-11-24 (`eng.caoanh.20261124.0735`, commit of this section): kernel 7.3.0-gts7l
from the series with 0105 (committed fragment: no `CONFIG_DEVMEM`), `boot.img` 16924672 B
(header v2), `recovery.img` 39430144 B (header v1, 111 firmware files), `super.img` 1.5 GB
sparse with system, system_ext, product, vendor, gts7l_fw; all checks of
`tools/gts7l-image.sh` pass (`out/gts7l/images/CHECK.txt`). With `J_BIG=3` the system.img step
was OOM-killed under the 20 GB cap (parallel R8/metalava), so `tools/gts7l-image.sh` now builds
the big images with one job (system.img ~17 min). Not yet run on the tablet.

Risks for the first boot: `init.gts7l.rc` modprobes all 1114 modules of `modules.load` at
`on fs` (slow, and one bad module can hang it; pstore shows where); first boot formats the
107 GiB userdata; ABL's handling of the PON recovery bit (above).

## 16. Android first boot (S7)

Every attempt to boot `aosp_gts7l` on the tablet, flashed with `tools/gts7l-android-flash.sh`
from the bring-up shell (§15). The screen is not used: everything below was read over USB.

**Reading the tablet without adb.** Android's gadget keeps the bring-up shell's ACM function
(`f8`), so a root shell is on the host's `/dev/ttyACM0` (group `uucp`; `sudo setfacl -m
u:$USER:rw /dev/ttyACM0` if the user is not in it). The shell's `PATH` is the vendor one: call
`/system/bin/logcat` by its full path. `dmesg` is useless with SELinux permissive: the avc
denials overwrite the kernel ring buffer within seconds; `logcat -b kernel` keeps the early
kernel messages.

### Attempt 1 (2026-11-24, build `eng.caoanh.20261124.0735`)

Flash: all checks and read-backs OK (super 10171187200 B streamed in 61 s). After the reboot
the host saw `18d1:4ee7` ("android-pc Galaxy Tab S7 LTE") within the tool's wait, so first-stage
mount, `mount_all`, the userdata/metadata formats (f2fs on `sda37`, ext4 on `sda28`) and the
vendor init all worked. The gadget had only ACM + NCM (the `gts7l-usb.sh` fallback): adbd ran
but never bound. Up 4 min, `sys.boot_completed` unset. Found:

| Problem | Cause | Fix |
|---|---|---|
| No adb on USB | `init.gts7l.usb.rc` mounted FunctionFS before `mkdir functions/ffs.adb`; FunctionFS finds its device by the function's name, so the mount failed and `/dev/usb-ffs/adb` stayed empty | mount after the `mkdir` |
| SurfaceFlinger aborts: `no suitable EGLConfig found` | Mesa: `get_param failed! -6`; kernel: `a650 SQE ucode is too old. Have version 93 need at least 95`. The only `a650_sqe.fw` the tablet has is Samsung's 0.93; mainline refuses anything older than 0.95 for a650 (CP security fixes), so the GPU never loads | kernel patch 0106: warn and use 0.93 (linux-firmware's newer SQE is the cleaner fix; not on this host) |
| `vendor.graphics.allocator` never starts | `allocator-service.minigbm` labelled `vendor_file`: init refuses a service with no domain transition even in permissive mode | `hal_graphics_allocator_default_exec` in `file_contexts` |
| drm_hwcomposer on the wrong node | the msm card is `/dev/dri/card1` (card0 does not exist), the product set `card0`; the `card%` pattern stops at the first missing card | `vendor.hwc.drm.device=/dev/dri/card1` |
| audioserver: SIGSEGV in `AudioFlinger::onFirstRef` every few seconds | no audio HAL in the image | AOSP's default AIDL audio + effects HALs (`com.android.hardware.audio`), as on Cuttlefish |
| zygote, netd restarting | zygote follows SurfaceFlinger's restarts | (from the fixes above) |

About 6 min after boot the ACM shell and NCM stopped answering while the USB device stayed
enumerated (no reboot, no re-enumeration): the tablet hung.
The kernel was still alive (the NCM interface answered ARP) but nothing let us in: the ACM
shell stopped reading and writing, adbd's TCP port 5555 and ICMP were dropped (netd, which was
restarting, had left its firewall chains in place), and nothing reboots an Android that is
merely stuck. pstore of this boot can be read only after a reset that keeps RAM.

**Recovery from this state (needs a person):**

1. Hold **Power + Volume Down** for about 7 s until the screen goes off (forced restart).
2. Immediately hold **Volume Up + Power** until the tablet boots: that starts the `recovery`
   partition, which is now our bring-up shell (§15), and the host sees `1d6b:0104` and
   `telnet 172.16.42.1` answers within ~15 s.
   (If step 2 is missed, the tablet boots the attempt-1 Android again; that is also fine: its
   ACM root shell answers for a few minutes, enough for `reboot recovery`.)
3. From the worktree: `tools/gts7l-android-flash.sh --yes` (first read `/sys/fs/pstore` in the
   bring-up shell for attempt 1's last console).

### Attempt 2 (2026-11-24, build `eng.caoanh.20261124.0823`)

Kernel with patch 0106; vendor with attempt 1's fixes. After the person's reset (10:24) the
tablet came up in the bring-up shell twice (a second reset a minute later). Both boots had an
empty `/sys/fs/pstore`, so attempt 1's hang left no record.

Flash: all checks and read-backs OK. **adb came up over USB** (`R52R504WFTK device`) about 40 s
after the reboot, so the FunctionFS fix works. `sys.boot_completed` was still unset:

| Problem | Cause | Fix |
|---|---|---|
| `vendor.graphics.allocator` exits with status 1 at every start: `Failed to initialize driver` | minigbm was built with its generic backend, whose list has no `msm` entry, so `drv_create` found no backend for the card's driver name. The service now starts (the label fix works) | `$(call soong_config_set,minigbm,platform,msm)`: every minigbm module of this product (allocator, mapper, gralloc0) gets `DRV_MSM` |
| SurfaceFlinger, zygote and system_server restart every ~5 s (`boot_progress_start` loops) | SurfaceFlinger: `Failed to initialize driver` from gralloc.minigbm (same generic backend), and the allocator it waits for never registers | (same fix) |

Everything else came up. The GPU loaded Samsung's SQE with patch 0106's warning (`a650 SQE
ucode 93 is older than 95 ... using it anyway`), then the GMU 2.1.8 and the zap shader.
drm_hwcomposer runs on `card1`. The audio HALs run, and audioserver no longer crashes: init
only kills it along with SurfaceFlinger's restarts. netd and vold came up too.

### Attempt 3 (2026-11-24, build `eng.caoanh.20261124.1032`)

minigbm built with `DRV_MSM`. The tablet was still running attempt 2. `adb reboot recovery` did
nothing: adbd's shell is uid 2000, and `sys.powerctl` needs root. It went to the bring-up shell
through the ACM root shell (`setprop sys.powerctl reboot,recovery`).

The first flash reached super's read-back (super written in 56 s) when the tablet dropped off
USB and showed Samsung's "Downloading" screen. The person brought it back with Power + Volume
Down, then Volume Up + Power, and pstore was empty again. The second flash was all OK.

What came up: adb about 40 s after the reboot, the minigbm allocator, drm_hwcomposer and
SurfaceFlinger. All three stayed up with no restarts. system_server got to `pms_ready`, but
`sys.boot_completed` stayed unset:

| Problem | Cause | Fix |
|---|---|---|
| system_server killed by its watchdog every ~90 s: `Blocked in handler on main thread (main) for 66s` in `AudioSystem.isMicrophoneMuted` from `AudioService.<init>` | `/vendor/etc/audio_policy_configuration.xml` is missing. `audio_policy_config_vendor_1.mk` installs only the files it includes (r_submix, volumes, surround). The default AIDL HAL builds its modules from that file, so it registered only `IConfig`, with no `IModule/default`. audioserver waited for the module forever (`Waited one second for android.hardware.audio.core.IModule/default`). | install AOSP's generic `audio_policy_configuration.xml` (primary + r_submix) and `primary_audio_policy_configuration.xml` |

### Attempt 4 (build `eng.caoanh.20261124.1108`, not flashed)

The two files were added as `PRODUCT_PACKAGES`, and the build ended without them in
`vendor.img`. The names are ambiguous: Cuttlefish's auto targets define prebuilt modules with
the same names (`audio_policy_configuration.xml`), so the generated vendor image left
frameworks/av's out without an error. Fix: `PRODUCT_COPY_FILES`.

### Attempt 5 (2026-11-24, build `eng.caoanh.20261124.1117`)

**Getting to the bring-up shell.** `setprop sys.powerctl reboot,recovery` (from the ACM root
shell) did nothing. init logged `Failed to set bootloader message: failed to find /misc
partition` and dropped the reboot, because `fstab.gts7l` had no `/misc` line. A forced restart
with the keys came back into Android, since the recovery key combination was missed. What worked:

1. From the ACM root shell, write the bootloader message by hand:
   `printf boot-recovery | dd of=/dev/block/sda13 conv=notrunc` (misc).
2. Reboot with `setprop sys.powerctl reboot`. ABL honours the message and started recovery.
3. In the bring-up shell, zero misc's first 32 bytes again before going back to Android.

Fixes: a `/misc` line in `fstab.gts7l`, and `tools/gts7l-android-flash.sh` now clears misc's
command before its reboot.

Flash: all OK. The audio HAL now registered `IModule/default` and `IModule/r_submix`.
audioserver then waited for `IModule/bluetooth` forever. The HAL's VINTF fragment declares
default, r_submix and bluetooth, and audioserver waits for every declared instance, but the
generic configuration has no bluetooth module. `sys.boot_completed` stayed unset (system_server
was again in its watchdog loop).

| Problem | Cause | Fix |
|---|---|---|
| audioserver: `Waited one second for android.hardware.audio.core.IModule/bluetooth` | frameworks/av's generic configuration has no bluetooth module | Cuttlefish's audio policy set: its top file and primary file, plus frameworks/av's `bluetooth_with_le_audio_policy_configuration_7_0.xml` |

### Attempt 6 (2026-11-24, build `eng.caoanh.20261124.1147`)

Getting to the bring-up shell: the bootloader message was written by hand again (attempt 5's
image has no `/misc`). The flash tool cleared it, and the tablet booted Android.

Flash: all OK. The audio HAL registered `IModule/default`, `r_submix` and `bluetooth`, and
audioserver started. SystemUI, Phone and Settings were launched, but `sys.boot_completed`
stayed unset: system_server aborted every 5–8 s (11 `boot_progress_start` in 4 min).

| Problem | Cause | Fix |
|---|---|---|
| system_server: SIGABRT in `BpfMapRO<…>::abortOnMismatch` from `meminfo::ReadProcessGpuUsageKb` (ActivityManager's `MemInfoReader.getGpuKernelUsedSizeKb`) | `/sys/fs/bpf/map_gpuMem_gpu_mem_total_map` does not exist. Mainline msm selects no `TRACE_GPU_MEM`, so the kernel has no `gpu_mem/gpu_mem_total` tracepoint, and bpfloader does not load `gpuMem.o`. android-pc's libmeminfo patch skipped the missing map only in Waydroid. | libmeminfo: skip a missing map everywhere (`tools/waydroid/aosp17-patches/system+memory+libmeminfo.patch`) |
| com.android.bluetooth: `HidlHci: assert failed: hci_ != nullptr` | no Bluetooth HAL | none yet: the app crash does not block boot |

### Attempt 7: booted (2026-11-24, build `eng.caoanh.20261124.1208`)

The running attempt 6 has `/misc`, so a plain `setprop sys.powerctl reboot,recovery` from the
ACM root shell brought up the bring-up shell, with no keys and no hand-written bootloader
message. Flash: all OK, misc's command cleared, and the tablet rebooted into Android.

**`sys.boot_completed=1` at about 43 s** (`boot_progress_enable_screen` 43.2 s,
`sf_stop_bootanim` 43.5 s), with a single `boot_progress_start` (no system_server restarts).
About 50 s after boot_completed, the host saw the gadget leave and come back. That is
UsbDeviceManager applying `sys.usb.config=adb`: the gadget keeps adb, ACM (`/dev/ttyACM0`) and
NCM, and the tablet did not reboot (uptime continuous). The boot reason reads
`reboot,factory_reset` because the flash tool wipes userdata.

Properties (`getprop`):

```
ro.build.fingerprint     Android/aosp_gts7l/gts7l:17/CP2A.260605.016/eng.caoanh.20261124.1208:userdebug/test-keys
ro.build.version         17 (SDK 37), security patch 2026-06-05, userdebug
ro.product.model         android-pc Galaxy Tab S7 LTE (device gts7l, serial R52R504WFTK)
ro.kernel.version        7.3 (Linux 7.3.0-gts7l, clang 22.0.2)
ro.boot.selinux          permissive; verifiedbootstate orange
ro.hardware.egl/vulkan   mesa / freedreno; ro.hardware.gralloc minigbm
vendor.hwc.drm.device    /dev/dri/card1
init.svc.*               surfaceflinger, audioserver, vendor.graphics.allocator running; none restarting
```

`dumpsys SurfaceFlinger --display-id`: `Display 0 (HWC display 0): no display identification
data` (the DSI panel has no EDID).

`dumpsys SurfaceFlinger` (summary):
- GLES: `freedreno, FD650, OpenGL ES 3.2 Mesa 26.1.0-devel (git-875160ed32)`, EGL 1.5.
- Sync: `EGL_ANDROID_native_fence_sync EGL_KHR_wait_sync`.
- Active mode: `hwcId=2, resolution=1600x2560, vsyncRate=120.00 Hz, dpi=274.59x274.59`.
- The composition list has the status bar and launcher layers. HWC missed 16 frames by then.

`dumpsys display` (summary):
- `"Built-in Screen": uniqueId="local:0", 1600 x 2560`, `mState=ON`.
- Supported refresh rates: 119.9998, 59.9999, 40, 30, 24 and 20 Hz. The render frame rate is
  60 by default, and the active SF mode runs at 120 Hz.
- 274.59 dpi, density 320, internal viewport 1600x2560.

dmesg (kernel log buffer, `logcat -b kernel`):
- simpledrm on the bootloader's framebuffer is minor 0.
- msm_dpu binds both DSI links and the DP controller, and is minor 1 (card1, renderD128).
- The GPU loads Samsung's SQE 0.93 with patch 0106's warning, then GMU 2.1.8 and the zap shader.
- One `dsi_get_phy: phy driver is not ready` deferral on `ae96000.dsi` before the bind.

Stability: still up at 8966 s (2 h 29 min) uptime, `sys.boot_completed=1`, one
`boot_progress_start` in the event log (no runtime restart), nothing in `restarting`, focus
on SystemUI's NotificationShade over the desktop-mode wallpaper. The host lost the gadget at
about 3 min uptime (`USB disconnect`, then `device descriptor read/64, error -110`) and saw it
again 2 h 25 min later. Android kept running across the gap, so it was a USB link or cable
drop, not a hang; the cause is not known yet.

## 17. Touchscreen: NT36523 TDDI (2026-11-24)

**Chip.** The touch half of the NT36523 TDDI (display and touch in one chip). Stock DT
(dtbo fragment@131 on `qupv3_se5_i2c`): `novatek,nvt-ts` at `0x62`, IRQ gpio15 (falling edge,
`attn_irq`: gpio, no pull), 1600x2560, 10 touches, firmware `tsp_novatek/nt36523_gts7l.bin`,
**no supplies and no reset GPIO**: the panel powers and resets the chip. Stock driver:
`drivers/input/touchscreen/novatek/nt36523/` in LineageOS `android_kernel_samsung_sm8250`
(I2C, the firmware runs from the chip's own flash; the .bin is only for flash updates).
`0x62` is the chip's HW/bootloader address; the firmware answers at **`0x01`**, with the
NT36672A's register map. Read from the running chip with `i2ctransfer` (`/dev/i2c-5`):

```
0x78: 24 db 20 32 06 40 0a 00 0e 0a 00 01 02 01 08 10
      fw 0x24, 32x50 channels, 1600x2560, 10 touches, no buttons, IRQ type 1 (falling), chip ID 0x08
```

**Kernel** (patches 0107-0109): mainline `novatek-nvt-ts` with compatible
`novatek,nt36523-ts` (chip ID 0x08), the reset GPIO made optional, and palm (5) / glove (6)
touch states, which Samsung's firmware reports, mapped to `MT_TOOL_PALM` / finger. DTS:
`touchscreen@1` on i2c5, `CONFIG_TOUCHSCREEN_NOVATEK_NVT_TS=y` in the fragment.

**Tested** (kernel #18 in the boot image of build `1208`, only `boot` rewritten from Android's
root adb, userdata kept): the driver probes at 0.8 s and registers `nt36523-ts` (input4,
`ABS_MT_*`, `ABS_MT_TOOL_TYPE`), and `getevent` shows its events. **But touch does not
work yet**: the firmware sensing data is wrong.

- After boot the firmware stays in its init state (`0x60` = `0xA0`, or the page index
  flipping between the event buffer and the trim area), INT held low, one IRQ. It reaches
  normal run (`0xA3`, IRQ ~60 Hz) only after the screen is turned off and on once (a
  bootloader reset, `00 69` to `0x62`, sends it back to init). Stock resets it from the panel
  power path, so TDDI touch waits for a display event.
- In normal run every frame holds the same report: one palm (state 5) at (751, 487),
  unchanged by fingers anywhere (400 reads over 40 s while the user touched), by resets and
  by screen cycles. Android sees one held palm.
- The sensor does scan: raw data (`RAW_PIPE0` 0x30FA0) ~4600 per node, with noise. But the
  baseline (`0x36510`) is ~787, so diff (`DIFF_PIPE0` 0x373E8) ≈ raw − baseline ≈ 3800 on
  every node: the firmware sees the whole screen as covered and reports a palm.

Most likely cause: the panel. TDDI touch scans in the display's blanking periods, and the
panel is not driven correctly yet (only ~160 lines from DSI0, no video on DSI1, §12
"kernels #10-#13"). Retest touch once the panel shows a full picture; if the raw values are
still far from the baseline then, compare the panel init table's touch-related registers
with stock. Read pages with `i2ctransfer -f -y 5 w3@0x01 0xff <addr[23:16]> <addr[15:8]>`, then
`w1@0x01 <addr[7:0]> rN@0x01`, and set the page back to the event buffer (`0xff 0x02 0xfe`)
afterwards: the driver reads 0x00 of whatever page is set.

USB: the gadget drops whenever Android suspends (screen off); `svc power stayon true`,
`settings put system screen_off_timeout 2147483647` and `locksettings set-disabled true`
keep it up (set on the tablet now). Long `adb shell` commands should run detached (`nohup`)
and write a log.

## 18. Panel: bootloader registers vs ours, kernels #19-#23 (2026-11-24 evening)

Still open: the DSI0 half shows only the first ~160 portrait lines right and the DSI1 half
stays dark. This round ruled out the DSI/PHY programming. The tablet is back on kernel #18
(touch session) in `boot` and our `recovery.img`; `dtbo` stays zeroed.

**Correction (§19): this dump is not the bootloader's.** The kernel has `CONFIG_CMDLINE_FORCE`, so
`msm.modeset=0` from the image header was ignored and msm had already reprogrammed the link.

**The bootloader's registers, read while it drives the panel.** `recovery` held a bring-up
image with kernel #20 (`CONFIG_DEVMEM=y` by hand) and `msm.modeset=0 clk_ignore_unused
pd_ignore_unused` (`gts7l-stock/s6/recovery-k20-modeset0.img`), so msm never touches the
display. `gts7l-stock/s6/pt/abldump.sh` first checks the MDSS GDSC and the dispcc AHB, byte,
pclk and esc branches (all on), then reads only DSI registers: ctrl (0x400), PHY common
(0x200), lanes (0x280) and PLL (0x280) of both links (`gts7l-stock/s6/abl-dsi-regs.txt`). No
DPU register was read. The same script on Android with kernel #20 gives
`k20-dsi-regs.txt`.

Result: **every DSI controller, PHY common and lane register matches the bootloader's**,
including the slave PHY's `CLK_CFG0` 0x11, `CLK_CFG1` 0x34 (BITCLK_SEL 1), `LANE_CTRL1` 0x60
(continuous clock), the host `LANE_CTRL` bit 28, `CPHY_MODE_CTRL` 1 and the C-PHY timings
(the bootloader uses the computed `27 2b 0a 1a 0b`, not the DT bytes). Only PLL status and
calibration readbacks differ (`FRAC_DIV_START_LOW_1` 0x51 vs 0x50, `ICODE_ACCUM`, `FD_OUT`,
`LOCKTIME0`) and `CLK_STATUS` (dynamic gating). The register dump in §12/§14
(`regs-now.txt`) was taken during a non-continuous-clock test and is misleading.

**Also checked:**
- LineageOS recovery (4.19): no debugfs and no `/dev/mem`. It keeps the bootloader's
  splash (`splash booting.. do not send ON_PRE_SEQ`), so it never initialises the link
  itself. Its live DT has the same panel timings and the same supplies as ours: DSI
  `vdda-1p2` = L9A 1.2 V, PHY `vdda-0p9` = L5A 0.88 V, panel `vddlcd` + `buck`.
  (To reach root adb there, its ramdisk got our `adb_keys` and was re-signed with the AVB
  test key it already uses. Odin refused even a zeroed `DTBO` with `FAIL! Auth`, and a
  stock `dtbo` plus our boot drops the tablet into the Download screen. Never write the
  stock `dtbo` again while `boot` holds our kernel.)
- DSI1 HS link: `dcs_write` `51 00 00` / `51 01 ce` in HS to DSI1 only changes the
  brightness (photo mean 31 vs 210), so DSI1 HS transmission and the panel's DSI1 port work.
  Only video on DSI1 is not shown.
- DPU snapshot (`cat /sys/kernel/debug/dri/1/kms`, the driver's own dump; `k20-dpu-snapshot.txt`):
  INTF_1 and INTF_2 are programmed alike (h 916, v 2594, display hctl 32-831, prog fetch
  on), both in CTL0 (`INTF_ACTIVE` 0x6), same frame count; SSPP 8/9 fetch x 0 and x 800;
  LM1 is the right mixer. The vsync rate is 120 Hz while content changes. The MDP clock is
  460 MHz.
- `SPLIT_DISPLAY_EN` / `UPPER` / `LOWER_PIPE_CTRL` read back 0 right after the master
  encoder writes 1 / 0x10 / 0x100; on DPU 6 both INTFs run from one CTL anyway.

| Kernel (debug, `gts7l-stock/s6/k19-k23-debug.diff`) | Change | Result |
|---|---|---|
| #19 | slave PHY `CLK_CFG0` = master's pixel divider, bit divider 1 (as downstream) | no change |
| #19 | `DSI_TIMING_DB_MODE` = 0 | already 0 |
| #22 | `INTF_CONFIG2` without `DATA_HCTL_EN` (downstream writes 0 for DSI) | no change |
| #22 | `INTF_PANEL_FORMAT` 0x213f | already written (reads back 0x2100) |
| #23 | both bonded DSI controllers enabled back to back (46 µs apart instead of 24 ms) | no change |

Open: with the DSI side identical, the difference must be in the DPU INTF timing (not read
from the bootloader: `/dev/mem` on DPU registers once rebooted the tablet) or in how the
link starts relative to the panel. Next candidates: a driver-mediated dump of the
bootloader's INTF/CTL/TOP registers before `msm_mdss` resets MDSS (the reset runs at MDSS
probe), and the DSI/INTF start order of the downstream driver on a cold boot.

`input keyevent SLEEP` suspends Android and drops USB. Take a wake lock first
(`echo gts7lpanel > /sys/power/wake_lock`).

## 19. Sensors: accelerometer through the sensor DSP (2026-11-25)

Auto-rotate works: `dumpsys sensorservice` lists `LSM6DSO Accelerometer` (STMicro, the
SLPI's own driver), samples near 9.8 m/s² that follow the tablet, and with auto-rotate on the
window manager's `mProposedRotation` follows them.

**No kernel modules were in the image.** `BOARD_VENDOR_KERNEL_MODULES` is dropped by the
Soong-only build unless the product has a `vendor_dlkm` partition, so `/vendor/lib/modules`
did not exist and init's `modprobe` did nothing: every `=m` driver was missing (remoteprocs,
FastRPC, and everything else built as a module). `aosp-device/gts7l/kernel/modules.bp`, which
`tools/aosp-device-link.sh` installs as `device/android-pc/gts7l-kernel/Android.bp`, makes
them a `prebuilt_kernel_modules` vendor package (`gts7l_kernel_modules`): all 1114 modules
plus `modules.dep`, but `modules.load` lists only what is known to work. It holds the
remoteproc and FastRPC set (`qmi_helpers` ... `qcom_q6v5_pas`, `fastrpc`). Add a driver there
once it has been tried on the tablet.

The chain, as on gts7xl-mainline (`docs/sensors.md` there):

| Piece | What |
|---|---|
| `qcom_q6v5_pas` | boots the SLPI (and the ADSP; the CDSP fails authentication, harmless) from `gts7l_fw` |
| `fastrpc` | `/dev/fastrpc-sdsp` (ueventd: 0660 system) |
| `gts7l-sensors-setup` (post-fs-data) | lays out `/data/vendor/sensors/root`: the stock `/vendor/etc/sensors` from `gts7l_fw` (`tools/gts7l-firmware.sh` now copies it, 57 files), the registry seeded once from `persist`, soc/revision files (`androidboot.revision=7` from the DT's bootargs) |
| `gts7l-sensord --serve` | gts7xl-mainline's sensord (MIT, `sensors/sensord/`, unchanged): the files the DSP asks for over FastRPC; the DSP rewrites its registry there |
| `android.hardware.sensors-service.gts7l` | Rust AIDL ISensors (V3): one non-wake-up accelerometer, 26-208 Hz; runs `gts7l-sensord --stream accel --rate N` (domain `gts7l_sensord`) and writes each line as an event into the framework's FMQ |

The SSC reports in Android's device frame already (its registry holds the board's axis
mapping), so the HAL passes the values through: standing in landscape with `ROTATION_90`,
x reads +9.5.

Rust AIDL pitfall: the generated `WriteTo` of a `@FixedSize` union
(`Event.EventPayload`) takes the payload's offset from the addresses of two local
references, so `WriteCompletion::write` put x/y/z in the wrong place and the framework saw
only zeros. The HAL copies the whole `Event` into the FMQ slot instead (`ptr()` +
`assume_written`); the Rust and NDK layouts are both 96 bytes, checked at compile time.

`tools/gts7l-android-flash.sh --keep-data` updates a build that already booted without
wiping `userdata`/`metadata`.

Open:
- Gyroscope, magnetometer, light and the other SSC sensors (the DSP lists them): more
  sensor types in the HAL, each a `--stream TYPE`.
- The tablet rebooted on its own three times today: twice to Android (once with
  `gts7l-sensord --serve` started by hand, once with no sensord running), once into
  recovery from Android (misc `boot-recovery` with no reason, pstore empty). The cause is not
  known yet; continuous logcat/dmesg captures on the host are the next step.

## 20. Bluetooth: QCA6390 over uart6 (2026-11-25)

**The kernel side was there already.** The DT (`0094`, from gts7xlwifi's `0021`) has the
`qcom,qca6390-pmu` power sequencer (BT_EN GPIO 76, WLAN_EN GPIO 90) and a `qcom,qca6390-bt`
serdev child of `uart6` (`998000.serial`, alias `serial1`); the defconfig has `hci_uart` with
`CONFIG_BT_HCIUART_QCA`, `btqca` and `pwrseq-qcom-wcn` as modules. Nothing loaded them:
`modules.load` (§19) listed only the remoteprocs. Loaded by hand, `hci_qca` brings the chip up
at once: `QCA SOC Version 0x400a0200`, ROM 0x200, `qca/htbtfw20.tlv` and `qca/htnv20.bin`
from `gts7l_fw` (firmware_class.path, §13), `QCA setup on UART is completed`. A raw HCI test
over the user channel (reset, LE scan, inquiry) found six LE advertisers within 8 s.

**Unconfigured controller.** The NVM file carries Qualcomm's default address
`00:00:00:00:5a:ad`, so `btqca` sets `HCI_QUIRK_USE_BDADDR_PROPERTY` and, with no
`local-bd-address` in the DT, the kernel keeps hci0 *unconfigured*: it shows up in the mgmt
`Read Unconfigured Controller Index List`, not in `Read Controller Index List`, which was all
the HAL looked at. Samsung's address is in `efs`: `/mnt/vendor/efs/bluetooth/bt_addr`
(text `xx:xx:xx:xx:xx:xx`, group `bluetooth`). mgmt `Set Public Address` (0x0039) gives it to
the kernel, which writes it with `qca_set_bdaddr` (vendor command 0xfc14) after every setup,
user-channel opens included; the controller becomes configured and `Read BD_ADDR` returns it.

| Piece | What |
|---|---|
| `aosp-device/gts7l/kernel/modules.bp` | loads `pwrseq-qcom-wcn` (shared with ath11k's PCIe power control), `rfkill`, `ecc`, `ecdh_generic`, `bluetooth`, `btbcm`, `btqca`, `hci_uart` |
| `hals/bluetooth` | android-pc's Rust IBluetoothHci over the HCI user channel, reused. New: it also accepts an unconfigured controller, and `--bdaddr-file FILE --bdaddr-seed SERIAL` sets the public address at startup (the file's, else a stable one derived from the serial number with FNV-1a) when the kernel holds the controller unconfigured |
| `init.gts7l.rc` | `vendor.bluetooth-gts7l`: the HAL with `--bdaddr-file /mnt/vendor/efs/bluetooth/bt_addr --bdaddr-seed ${ro.serialno}`, class hal (the PC's `vendor.bluetooth-pc` stays disabled) |
| `manifest.xml` | declares `IBluetoothHci/default` (on the PC Cuttlefish's APEX does, so the HAL has no fragment of its own) |
| `hals.list`, `sepolicy/hal_pc_bluetooth.te` | the HAL's sepolicy, plus reading `vendor_efs_file` |
| `aosp_gts7l.mk` | `android.hardware.bluetooth.xml`, `android.hardware.bluetooth_le.xml` |

Verified 2026-11-25 (build `eng.caoanh.20261124.1208` repacked with these changes, flashed with
`--keep-data`): at boot `modprobe` loads the Bluetooth set, hci_qca downloads the firmware, the
HAL gives hci0 the efs address and the stack opens it (`hci0: user channel open`,
`initialization complete: SUCCESS`). The stack turns Bluetooth on by itself (persisted state):
`dumpsys bluetooth_manager` shows `State: ON`, address `..:D5:DF` (efs), `Bluetooth crashed 0
times`. Settings → Pair new device found six nearby devices (LE and dual-mode), and a Logitech
M196 mouse paired over LE from the Settings UI (`Bonded devices: 1`).

Open:
- `Received HCI_IBS_WAKE_ACK in tx state 0` at every setup is harmless (hci_qca's in-band sleep
  starting up).
- Setup (firmware download, about 1.5 s) reruns at every user-channel open; Bluetooth off/on
  in Settings pays it again.
- Bluetooth audio: the audio HAL's `IModule/bluetooth` exists (§16, attempt 6), A2DP/LE audio
  untested.

### Keyboards and mice (HID over GATT)

A Logitech M196 mouse and a MeetionKB1 keyboard paired but never produced input. The kernel
side was fine (`CONFIG_UHID=y`, `CONFIG_HID_GENERIC=y`, `/dev/uhid` 0660 `uhid` labelled
`uhid_device`); `dumpsys bluetooth_manager` showed only `GattService` and
`HearingAidService` and `shim::legacy::hid status:BTIF_HH_DISABLED`. The Bluetooth app starts
only the profiles whose `bluetooth.profile.*.enabled` property is set, and the product set
none (Cuttlefish and goldfish set them in their own product props). `aosp_gts7l.mk` now sets
`bluetooth.profile.gatt.enabled` and `bluetooth.profile.hid.host.enabled` (product
properties); profiles that need a Bluetooth audio HAL (A2DP, HFP, LE audio) stay off.

Devices bonded while HID Host was off have no HID connection yet: tap them once in
Settings → Connected devices (Settings itself must be restarted if it was running when the
profile came up: `No profiles. Maybe we will connect later`), then wake the device (move the
mouse, press a key) within the 30 s connect window. Verified 2026-11-25: `hh_open_handler:
Connected` for both, `Logi M196 Mouse` and `MeetionKB1 Keyboard` input devices through uhid,
`getevent` shows `REL_X`/`REL_Y`/`BTN_MOUSE` and key presses. LE HID devices reconnect by
themselves afterwards.

## 21. Panel: the real bootloader state, kernels #24-#28 (2026-11-24 night)

Still open: same picture (DSI0 half right for the first ~80-160 portrait lines, DSI1 half
dark). The tablet is back on kernel #18 in `boot` and our `recovery.img` (`2f71f318...`).

**§18's "bootloader" dump was ours.** `CONFIG_CMDLINE_FORCE=y` drops the boot image cmdline, so
`recovery-k20-modeset0.img` ran msm normally (`/sys/module/msm/parameters/modeset` = Y, fbdev
console on screen). Debug kernels must put such arguments into `CONFIG_CMDLINE` (the fragment).

**Real bootloader dumps** (`gts7l-stock/s6/`, all read with the display clocks on):
- `abl-dsi-regs.txt`: kernel #24/#25, `msm.modeset=0 clk_ignore_unused pd_ignore_unused` in
  `CONFIG_CMDLINE`, `abldump.sh` (DSI ctrl/PHY/lanes/PLL only). The old file is
  `k20-recovery-dsi-regs-NOT-abl.txt`.
- `abl-dispcc-regs.txt`: dispcc through `/sys/kernel/debug/regmap/af00000.clock-controller/registers`
  (ours: `k18-dispcc-regmap.txt`). pclk/byte RCGs, dividers and branches match.
- `abl-dpu-snapshot.txt`: kernel #27 with two DEBUG params, `msm.mdss_noreset=1` (skip the MDSS
  reset at probe) and `msm.boot_snapshot=1` (the DPU driver's own snapshot at the start of
  `dpu_kms_hw_init`, before any DPU write, to devcoredump). No `devmem` on DPU registers.

**What the bootloader does differently (C-PHY):**
- PHY: `LANE_CTRL0` 0x07 (three trios, no clock lane; mainline 0x17), `LANE_CFG1` 0x04 (0x84),
  `LANE_CTRL1` 0 (no continuous clock), lanes 3 and 4 not configured, timings = the stock DT
  bytes (`TIMING_CTRL_4..8` 11 3e 0d 1e 0a), PLL `SYSTEM_MUXES` 0xc0 on both PHYs (ours reads 0:
  the PLL bias refcount drops to zero while the PLL runs), VCO 998.0 MHz (`FRAC_DIV` 0x3f506;
  ours 997.96).
- DSI host: `DSI_CTRL` 0x77 (no `CLK_EN`, `CMD_MODE_EN` set), `LANE_CTRL` 0 (no
  `HS_REQ_SEL_PHY`/`CLKLN_HS_FORCE_REQUEST`), `TRIG_CTRL` 0x4 (no TE), `CLKOUT_TIMING_CTRL` 0,
  `HS_TIMER_CTRL` 0x4ea60.
- DPU: INTF_1/INTF_2 timing, prog fetch, panel format, pingpong, LM and DSC blocks **match ours**.
  Only `INTF_CONFIG2` 0x100 (ours 0x10), `INTF_DISPLAY_DATA_HCTL` 0 and `INTF_MUX` bits 16-19 = 0
  (ours 0xf) differ.

| Kernel (in `recovery`, bring-up shell) | Change | Result |
|---|---|---|
| #26 | C-PHY: no clock lane (`LANE_CTRL0` 0x07, `LANE_CFG1` 0x04, lanes 0-2 only, no `CLK_EN`, no continuous clock) | same picture at boot; after a reset + full init DCS commands time out and DSI1 stays busy |
| #28 | #26 + every DSI host value above + stock C-PHY timings + the bootloader's `INTF_CONFIG2`/`DATA_HCTL`/`INTF_MUX` (`msm.abl_dsi`, `msm.abl_intf`, `msm.cphy_timing`) | same picture |
| #28 live | PLL `SYSTEM_MUXES` 0xc0 on both PHYs (`devmem`, DSI PHY) | same |

Debug code: `gts7l-stock/s6/k24-k28-debug.diff` (snapshot/no-reset params, ABL-match params, the
C-PHY lane change). The kernel tree is clean again.

**New observations (webcam):**
- The bootloader's unlock warning and the "SAMSUNG Galaxy Tab S7" logo fill **both halves**,
  including text far below line 160, so its link setup works on this panel.
- With our kernel, a new frame updates only the first ~80-160 lines of the DSI0 half; the rest
  of the panel **keeps the previous image** (a sparse white-on-black frame leaves the old
  console stripes in place). The fault does not depend on the content: the panel stops
  writing lines a fixed time or amount of data into each frame.

Open: with the static DSI, PHY, dispcc and DPU state now matching the bootloader, the remaining
differences are dynamic: the start order of PLL/PHY/controllers/INTFs, the panel's state after
our link restart, and the SSPP fetch path (the bootloader uses other pipes on LM0/LM1). Next
candidates: start the link in the bootloader's order (PHY + both controllers, then both INTFs
in one flush, display-on after video starts), and read the panel's error report (`0x05`,
`0x0a`, `0x0e`) over DSI1 right after one frame.

### Kernel #29 (same night)

`msm.intf_reg` (DEBUG, in `k24-k29-debug.diff`): `echo IDX,OFF[,VAL]` reads/writes an INTF
register through the DPU driver once it set the INTF up (IDX = enum: 2 = INTF_1, 3 = INTF_2).
- INTF `0x1c4` (bootloader 0xe00, ours 0xa00) is read-only status.
- Panel registers over DSI1 with video running: `0x05` 0 (one error at the first read only),
  `0x0a` 0x9e, `0x0c` 0x07, `0x0e` 0x80, `0x0f` 0xc0 (self-diagnosis fine). The panel does not
  see packet errors.
- DSI0 video engine off (`DSI_CTRL` 0x71): the DSI1 half still shows nothing.
- Display off/on (`28`/`29`) with video running, and the stock `TX_DFPS` sequence
  (`ff 2a, fb 01, 23 0d|0c, ff 10, fb 01`; Samsung sends it in `samsung_panel_on_post`):
  no change. The Samsung panel source (`ss_dsi_panel_NT36523_PPA957DB1.c` and its dtsi, copied
  to `gts7l-stock/s6/`) does nothing else in on_pre/on_post besides the LCD bias buck.
- With the quadrant pattern (`k29-dfps120-pattern.jpg`) the yellow 8 px border at x 0-7 and the
  centre line at x ~799 are right on **every** line of the DSI0 half; past the first ~160 lines
  only the middle of each line is not updated.

Photos with a person in them were deleted at once; only screen photos without people are kept.

## 22. Panel fixed: bonded C-PHY slave dividers, PLL bias, no clock lane (2026-11-24 night)

**The panel works:** full picture on both halves, from the bootloader's state and from a cold
reset + stock init, in the bring-up shell and in Android (kernel #35, `boot`
`gts7l-stock/s6/boot-k35.img`, SHA-256 `3f7d2535...`). Photos: `k33-clean-fix.jpg`,
`k33-cold-init.jpg`.

**How it was found.** A debug build of the LineageOS 4.19 kernel
(`LineageOS/android_kernel_samsung_sm8250`, lineage-24.0, `kona-perf_defconfig` +
`kona-sec-common.config` + `gts7l.config` + DEBUG_FS/DEVMEM, LTO/CFI and module signing off,
`HOSTCFLAGS=-DOPENSSL_IS_BORINGSSL` because the host OpenSSL has no ENGINE API) went into the
Lineage recovery image (`gts7l-stock/s6/lineage-rec/`). Its downstream driver drives the panel
correctly. Its `sde_dbg` debugfs (`/sys/kernel/debug/dri/0/debug/<blk>_off|_reg`, one block
range per read) gave the working DSI/PHY/DPU state (`gts7l-stock/s6/lineage-dump/`). The DPU
matched ours; on the DSI side the slave PHY's `CLK_CFG0` (0x11 vs our 0xf1), the PLL bias
(`PLL_SYSTEM_MUXES` 0xc0 vs our 0) and lane 3's `TX_DCTRL` (0x06) differed. Written live in
that order the picture came up; the order matters (the link has to come up with the right
dividers and bias, a later write does not repair it).

**Fixes** (new patches, `aosp-device/gts7l/kernel/patches`):
- 0105 `drm/msm/dsi: C-PHY: do not drive a clock lane`: `LANE_CTRL0` 0x07, `LANE_CFG1` 0x04,
  lanes 3/4 not configured except lane 3 `TX_DCTRL` 0x06, no `CLK_EN`, no continuous clock.
- 0106 `drm/msm/dsi/phy: 7nm: give the bonded slave PHY the master's dividers`: the slave's
  `CLK_CFG0` is never set by any clock in the tree; copy the master's in `vco_prepare`, before
  the bias, as downstream does.
- 0107 `drm/msm/dsi/phy: 7nm: program the PLL bias on every enable`: the first `vco_prepare`
  runs at probe before the PHY is powered, the write is lost and the refcount never writes it
  again.

**Series cleanup.** The DEBUG patches are gone: 0099 (`drm/panel: nt36523: gts7l: stock
on-commands as a table, backlight on both links`) replaces the old 0097-0099, 0102-0104; the
panel is always reset and initialised and gets display off/sleep in on disable. 0097/0098
(C-PHY tuning from DT) and the reboot-reason, a650 and touchscreen patches keep their content
(renumbered 0100-0104). The old branch is `gts7l-debug-backup` in `gts7l-kernel`;
`gts7l-stock/s6/k24-k32-debug.diff` keeps the debug switches (ABL-match params, `intf_reg`,
boot DPU snapshot, MDSS no-reset).

**Image retention.** The DSI0 half shows faint horizontal stripes in the shape of the old
garbage (`k34-android-ghost-*.jpg`, `k35-android-ghost-recents-t3min.jpg`). They stay put
across a panel reset and power cycle while new content (card outlines, wallpaper) draws
correctly through them, so they are LCD image retention from the hours the broken link held
the same lines, not a signal fault. They should fade with normal use; recheck later.

Checked again on 2026-11-25 00:06: the debug LineageOS recovery (downstream driver, none of our
code, panel reset and initialised by the bootloader) shows the same stripes in the same place
on its black background (`lineage-downstream-stripes-0006.jpg`), so they live in the panel.
Android at 00:04 (`k35-android-stripes-0004.jpg`) had not visibly faded yet. Bright, changing
content or the panel switched off for a few hours usually clears this kind of retention.

Solid-colour test (bring-up shell, kernel #35, `k35-solid-colors.jpg`): black, white, green, red
and grey frames fill the whole panel, including the striped area, so every pixel is refreshed.
The stripes show only on black and grey. After 10 minutes of a white frame they were visibly
fainter on grey (`k35-gray-white10-compare.jpg`). The tablet then rebooted into Android (boot
reason `recovery`) and the stripes were still there. They come from the hours the broken link
held the same lines; whether they clear fully is not known yet. `recovery` now holds the
bring-up image with kernel #35 (`4b26de41...`), which drives the panel correctly.

Next: rebuild the Android images with this kernel; retest touch (§17: the firmware reported a
fixed palm, likely because the panel was not running).

2026-11-25: the tablet was reflashed with build eng.caoanh.20261124.1208 (`--keep-data`) after
another session's flash was cut off; `boot` has kernel #35, but that build's `recovery.img`
still has kernel #15 (broken panel), so `recovery` was rewritten with the bring-up image on
kernel #35 again (`4b26de41...`). In Android the stripes were fainter, and on a black console
they were no longer visible. A white frame then ran for 60 minutes (01:40-02:39); the webcam had
moved and the grey check photo is out of focus (`k35-gray-after-white60.jpg`), so the result
is not measured. The tablet was then powered off.

## 23. Wi-Fi: QCA6390 on ath11k (2026-11-25)

No DT change was needed. The `qcom,qca6390-pmu` node (pwrseq-qcom-wcn) and the `wifi@0`
node under `pcieport0` from the S7+ series are right for this board: the stock dtbo (entry
04, fragment 167 on the `cnss` node) has the same WLAN_EN GPIO 90, BT_EN GPIO 76 and S6A as
the digital supply.
`qcom-pcie: cannot initialize host` and the host that never bound were only the missing
power control driver: on v7.3 the controller creates a platform device for each child node
with supplies (`1c00000.pcie:pcie@0:wifi@0`) and brings the link up only once a pwrctrl
driver has powered that device. Its driver, `pci-pwrctrl-pwrseq`, is a module
(`CONFIG_PCI_PWRCTRL_PWRSEQ=m`) and was never loaded. Loading it on the running Android:
the link trains at 5 GT/s x1, `17cb:1101` enumerates, `ath11k_pci` loads `amss.bin` and
`m3.bin` from `gts7l_fw` (MSI, 32 vectors), and `wlan0` scans.

```
qca6390 hw2.0, chip_id 0x0 chip_family 0xb board_id 0xff soc_id 0xffffffff
fw_version 0x10178624 fw_build_timestamp 2024-01-15 22:34
fw_build_id WLAN.HST.1.0.1.r1-01571-QCAHSTSWPL_SILICONZ-1.615178.11.632599.7
```

`board.bin` is the tablet's own `bdwlan.elf`; with OTP `board_id 0xff` ath11k takes
`board.bin` directly (no `board-2.bin` lookup). The chip
offers managed, AP, P2P-client/GO and P2P-device (`iw list`).

Image side:

- `kernel/modules.bp`: `pwrseq-qcom-wcn`, `pci-pwrctrl-pwrseq`, `rfkill`, `cfg80211`,
  `libarc4`, `mac80211`, `ath11k`, `ath11k_pci` in `modules.load` (loaded after
  `mount_all --early`, so the firmware is there at probe).
- `hals.list`: `wifi`, the Rust nl80211 HAL of the PC product (`hals/wifi`).
- `BoardConfig.mk`: `BOARD_WPA_SUPPLICANT_DRIVER`/`BOARD_HOSTAPD_DRIVER := NL80211`, which
  enables AOSP's generic `com.android.hardware.wpa_supplicant` APEX (wpa_supplicant and
  hostapd, their rc files and VINTF fragments); no private driver library (the stub), 11ax on.
- `aosp_gts7l.mk`: the APEX, `wifi.interface=wlan0`, `android.hardware.wifi.xml`. No
  `wifi.direct` feature until P2P is tested.
- `init.gts7l.rc`: `/data/vendor/wifi/{wpa,hostapd}/sockets`.
- `sepolicy/kernel.te`: `firmware_load` on `firmware_file` (the denial `ath11k_pci` hit).

Result (combined Bluetooth + Wi-Fi vendor image, flashed with `--keep-data`): the modules load
at boot, `vendor.wifi-pc` runs, `cmd wifi set-wifi-enabled enabled` brings up the client
mode manager on wlan0 and starts wpa_supplicant from the APEX, and `cmd wifi start-scan` /
`list-scan-results` list the 2.4 and 5 GHz networks around. No SELinux denials from the
Wi-Fi stack (permissive boot, `dmesg`).

Open:
- MAC address: the chip has no MAC in OTP (`board_id 0xff`), so ath11k makes up a
  `00:03:7f:xx:xx:xx` address that changes at every boot (Android's factory MAC with it;
  connections use randomized MACs anyway). The tablet's own address is in
  `efs:/wifi/.mac.info`; set it on wlan0 before the HAL starts, as the Bluetooth HAL does
  with the efs Bluetooth address (§20).
- Connecting to a WPA2 network (needs a password from the user), then SoftAP/hotspot through
  hostapd and P2P (`wifi.direct` feature, `p2p_supplicant.conf`).
- The Rust HAL's clippy failed only on arm64 (`c_char` is `u8` there): `netdev.rs` reads
  `sa_data` bytes with `to_ne_bytes` now.


## 24. microG, Play Store and Aurora Store, as user apps (2026-11-25)

The ROM carries no Google apps (the user's choice): unlike the PC product, `aosp_gts7l.mk`
does not include `microg.mk` and has no Play Store. `tools/gts7l-google-apps.sh` installs them
over adb, into userdata, so they survive `gts7l-android-flash.sh --keep-data`:

- microG GmsCore (`com.google.android.gms` 0.3.18), the APK of `tools/fetch-apps.sh microg`
  (one APK for every ABI), with its runtime permissions granted, on the Doze allowlist and on
  the Data Saver allowlist (what `sysconfig-microg.xml` gives a preinstalled microG).
- Google Play Store (`com.android.vending` 50.7.34-31), the arm64 `Phonesky.apk` of
  `google_apis_playstore/arm64-v8a-37.0_r08.zip` (the PC's copy has only `lib/x86_64/`
  libraries), pinned by SHA-256 and signer; extracted into `out/downloads/playstore/`, never
  into git.
- Aurora Store (`com.aurora.store` 4.8.4), F-Droid's build pinned by SHA-256 and signer: an
  open-source client of Google Play's catalogue that needs no privileges, kept as a fallback.

Two framework patches in `tools/patches/` make the first two work without being system apps:

- `aosp17-microg-signature-spoofing.patch`: a preinstalled microG may report Google's signature
  (its `fake-signature` meta-data), and so may an installed `com.google.android.gms` signed with
  microG's release key (certificate SHA-256 `9bd06727…d14165`). Without it Play Store sees no
  Google signature on GmsCore and microG's self-check fails the spoofing test.
- `aosp17-play-store-user-app.patch`: an installed, non-system `com.android.vending` signed
  with Google Play's key (certificate SHA-256 `f0fd6c5b…60db83`) gets what a preinstalled Play
  Store gets on a GMS device:
  - `AppIdPermissionPolicy.kt`: the signature permissions the emulator image's Play Store
    holds, limited to what Phonesky 50.7 requests: its privileged allowlist
    (`privapp-permissions-google-p.xml`), the `verifier` permissions it holds as the package
    verifier (`GRANT/REVOKE_RUNTIME_PERMISSIONS`, `INSTALL_GRANT_RUNTIME_PERMISSIONS`,
    `GET_APP_METADATA`, `MANAGE_APP_OPS_MODES`, `SET_HARMFUL_APP_WARNINGS`,
    `WRITE_DEVICE_CONFIG`, `ACCESS_INSTANT_APPS`) and the `preinstalled` ones
    (`READ_DEVICE_CONFIG`, `HIDE_NON_SYSTEM_OVERLAY_WINDOWS`, `WRITE_SETTINGS`). Without
    `MANAGE_USERS` it crashes at start on `UserManager.getUserCount()` → `getUsers()`;
    `pm grant` cannot give any of them. `INSTALL_PACKAGES` and `DELETE_PACKAGES` make its
    installs, updates (its own too) and uninstalls silent; `PackageInstaller` checks only the
    permissions there, not `isSystem`.
  - `AppsFilterImpl.java`: force-queryable, like `config_forceQueryablePackages` of the
    emulator's GoogleConfigOverlay (honoured for system apps only), so apps without a
    `<queries>` entry (billing, licensing, `market://` checks) still see it.
  - `DomainVerificationService.java`: its verified app links (`play.google.com`) are approved
    like google.xml's `<app-link>` (also honoured for system apps only).

Both are keyed on the signing certificate, so another app with the same package name gets
nothing.

What stays different from a preinstalled Play Store, on purpose:

- It is not the package verifier (`PackageManagerService` picks verifiers among system apps
  only), so installs never wait on Play Protect; it gets the verifier's permissions instead.
- google.xml's `allow-in-power-save-except-idle` is replaced by `cmd deviceidle whitelist`
  in the script (the stronger user allowlist, kept across reboots and self-updates).
- `whitelisted-staged-installer` (Mainline/APEX trains), `rollback-whitelisted-app` and
  `update-ownership` of `com.google.android.gms` are left out: Google's module trains do not
  match this ROM's APEX keys, and Play must not own updates of microG. Play may offer Google's
  Play services as an update to microG; it fails on the signature (microG's real certificate
  differs), so turn off auto-update for it if Play keeps retrying.

After a flash with the patch, check on the tablet (after running the script):

```bash
adb shell dumpsys package com.android.vending | grep -E 'MANAGE_USERS|INSTALL_PACKAGES|READ_DEVICE_CONFIG|GRANT_RUNTIME_PERMISSIONS'
```

Every line should say `granted=true`. Then `adb shell pm get-app-links com.android.vending`
(`play.google.com: system_configured`), `adb shell dumpsys package queries | sed -n '/via forceQueryable/,/via package/p'`
(lists `com.android.vending`), Play opens without crashing, installs, updates and uninstalls
an app without a prompt, and `adb shell dumpsys package <app> | grep installerPackageName`
names `com.android.vending`. Google's own Play services do not work as a user app either: they crash in
`UsageReportingChimeraService` on `getUsers()` and on `isSafetyCenterEnabled` (Safety Center
permissions), so microG stays the GmsCore.

After installing: open microG settings, turn on device registration (checkin) and cloud
messaging, add the Google account, then open Play Store.

**Sign-in from other apps needs microG 0.3.18.** With 0.3.16.1 and 0.3.17, signing in to Aurora
Store with its "microG" option spins forever: logcat shows `Permission Denial: starting Intent {
cmp=com.google.android.gms/org.microg.gms.auth.AskPackageOverrideActivity … } … not exported`.
This is microG's bug, not the user-app setup (a system microG fails the same way,
microg/GmsCore#3853): the fix for GHSA-rp4p-8x89-jhv5 made `AskPermissionActivity` and
`AskPackageOverrideActivity` non-exported, but `AccountAuthenticator.getAuthToken()` still put
them raw into `KEY_INTENT`, and `AccountManager` starts that intent from the calling app's
process. 0.3.18 (commit 342c280) wraps both in the exported `UnpackingRedirectActivity`, which
starts them through a PendingIntent of microG's own, so AOSP's `AccountManagerService`
(`checkKeyIntent`: the activity belongs to the authenticator's own package) and the activity
start both pass. No framework change is needed for it. Upgrading in place (`adb install -r`)
keeps the account and settings.

What a user-app microG still lacks next to a preinstalled one (`privapp-permissions-microg.xml`):
the privileged permissions are not granted, since `pm grant` cannot grant them. The one that
matters for push messaging is `CHANGE_DEVICE_IDLE_TEMP_WHITELIST`: microG's `McsService` puts
the app a message is for on the temporary power allowlist for 10 s (`PowerExemptionManager.
addToTemporaryAllowList`), so it may start a service from the background and reach the network
in Doze; without it the message is still delivered, but an app that starts a background
service to handle it can fail. `START_ACTIVITIES_FROM_BACKGROUND` lets microG open its consent
dialogs from the background. The others (`INSTALL_LOCATION_PROVIDER`, `LOCATION_HARDWARE`,
`UPDATE_APP_OPS_STATS`, `INTERACT_ACROSS_USERS`, `MANAGE_USB`, `MODIFY_PHONE_STATE`,
`NETWORK_SCAN`, `DUMP`, `WATCH_APPOPS`, `UPDATE_DEVICE_STATS`, the credential-service ones) are
for the system network location provider, multi-user and the credential manager, none of which
Google sign-in, checkin, push or Play Store need. Account authenticator (`com.google`), the
`com.google.android.gsf.gservices` provider and checkin need no privileges. A user-app microG
reads no `/product/etc/microg.xml`, so checkin and cloud messaging start off (turn them on as
above), and SafetyNet/DroidGuard too (turn them on in microG settings if an app needs Play
Integrity).
