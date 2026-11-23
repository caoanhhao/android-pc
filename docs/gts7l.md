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
| Co-processor firmware (ADSP, CDSP, SLPI, modem, GPU zap/GMU, Venus, Wi-Fi/BT, CS35L41 protection) | Samsung/Qualcomm, signed | **Not in our build.** Read at runtime from the tablet's own `apnhlos`, `modem` and vendor partitions; they run on the DSPs, not on Android |
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
| S6 | T2 | Boot S2's kernel with a USB-gadget-shell initramfs, then port drivers in order | S1, S2, S5 | shell over USB on the tablet |
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
- S4: the device firmware in the stock vendor (GPU sqe/gmu, ath11k, BT, CS35L41) needs a copy
  from the tablet (never in git) that `/mnt/vendor/firmware/gts7l/vendor` can reach.
- S2/S6: the DTS names firmware `gts7l/modem/<file>` / `gts7l/apnhlos/<file>`; the kernel
  needs ext4, f2fs, vfat; check `androidboot.boot_devices=soc@0/1d84000.ufshc` and
  `androidboot.usbcontroller=a600000.usb`.
- T3: Mesa freedreno/Turnip build (`tools/gts7l-mesa.sh`, AOSP 17's `external/mesa3d` builds
  only lavapipe and gfxstream); minigbm AIDL allocator/mapper with the msm backend; power HAL
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

Not in this image yet: Mesa (no `gts7l-mesa` prebuilts, so no GPU), the device firmware copy
from the stock vendor, panel/touch drivers (S6). It is the first-boot test image for S6/S7,
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
