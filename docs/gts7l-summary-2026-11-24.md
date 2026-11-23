# gts7l status, 2026-11-24 morning

Summary of the 2026-11-23 work and the night run on `aosp_gts7l`, android-pc's build for the
Samsung Galaxy Tab S7 LTE (SM-T875). Details: `docs/gts7l.md` (§8 work breakdown, §10-§14).
Everything is on branch `gts7l` (worktree `/home/caoanhhao/projects/android-pc-gts7l`), not
yet merged into `main`.

## First thing: the tablet is hung

The night run's debug kernel #9 (a register dump from the first DSI PHY probe) never came
back; the tablet has shown no USB device since 00:04:45. To recover:

1. Hold **Power + Volume Down** about 7 s until the screen goes off, then immediately hold
   **Volume Up + Power** to enter the recovery (LineageOS recovery, used only as a root adb
   tool), and enable **Advanced → Enable ADB**.
2. Write the last known-good bring-up image `gts7l-stock/s6/boot-k10-good.img`
   (16314368 bytes, SHA-256 `1bbd9e45a204868a28053bae944715bad86f5ec19831cc7d64859c7e1b4de075`)
   to `boot` the way §12 describes: zero `boot`, dd the image, compare SHA-256, clear byte 0
   of `param`, then reboot with sysrq (`echo b > /proc/sysrq-trigger`). `adb reboot` from the
   recovery boots the recovery again.
   Alternative: Download mode and `samloader flash -p BOOT boot-k10-good.img`.
3. After that, `tools/gts7l-netflash.sh` works again over the USB network (telnet
   172.16.42.1, host connection `gts7l-bringup`).

Nothing else on the tablet was touched: `efs`, `sec_efs`, `persist`, modem and firmware
partitions are as stock, and all 72 partitions are backed up in
`gts7l-backup/20261123-2230/` (copy it off this machine).

## Done

| Step | Result |
|---|---|
| S1 inventory | Stock `T875XXS8DXH1` firmware downloaded; partitions, boot header v2, DT (panel NT36523 CSOT, Novatek touch, Wacom, CS35L41 x4, MAX77705, QCA6390) in `docs/gts7l-inventory.md` |
| S2 kernel | Mainline v7.3 + gts7xl-mainline series + our gts7l DTS, `tools/gts7l-kernel.sh` |
| S3 product | `aosp_gts7l` (arm64, no Cuttlefish), PC product unchanged |
| S4 images | `tools/gts7l-image.sh`: boot (v2), super, vbmeta, dtbo, Odin AP tar |
| S5 unlock | Bootloader unlocked (`OEM LOCK: OFF`), our vbmeta, full backup with read-back check |
| S6 boot | Mainline 7.3 boots: UFS, 8 CPUs, USB gadget shell, Book Cover keyboard/touchpad, keys, charger, S Pen (after the L13 supply fix), CS35L41 probe |
| Firmware | `tools/gts7l-firmware.sh` builds a local-only `gts7l_fw` partition (GPU sqe/gmu/zap, DSPs, ath11k, BT, CS35L41 incl. calibration from `sec_efs`) from the tablet's own data, never in git (§13) |
| Mesa | freedreno (EGL/GLES) + Turnip (Vulkan) built from AOSP's `external/mesa3d` by `tools/gts7l-mesa.sh`, in `vendor.img` (§14); not run on the tablet yet |

## Not done

- **Panel**: our frames reach the panel with the right colours and orientation, but only on
  the DSI0 half, and full lines only for the first ~150 lines; DSI1's half is black; a cold
  reset + stock init gives a black screen. The stock C-PHY analog values are now a clean
  patch (0100/0101, from the DTS); 0097-0099 and 0102 are still DEBUG switches. The night run
  had no view of the screen (the webcam faced the room), so its findings are register/log
  based. Next tests are listed at the end of "Panel bring-up, kernels #8-#10" in §12.
  To let the panel work run alone next time, aim the UGREEN webcam at the tablet screen.
- Touch (NT36523 in `novatek-nvt-ts`), Wi-Fi (PCIe host init fails), fuel gauge (deferred),
  remoteprocs (firmware now available) — not started.
- No Android boot on the tablet yet (S7): needs the panel first.

## Branches merged into `gts7l` this morning

`claude/gts7l-firmware-d5a30a` (8e5dc58), `claude/gts7l-mesa-40ce87` (77b3469, 4c1f283),
`claude/gts7l-panel-6b377d` (851cb22). Conflicts in `docs/gts7l.md` (both added a §13; Mesa
is now §14) and `aosp-device/gts7l/sepolicy/file_contexts` (kept the GPU labels, dropped the
old firmware-link label that `gts7l_fw` replaces).
