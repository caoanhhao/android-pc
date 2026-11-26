# gts7l kernel

Mainline Linux **v7.3** for the Galaxy Tab S7 LTE (SM-T875), built by
`tools/gts7l-kernel.sh` (see `docs/gts7l.md`, step S2):

```
tools/gts7l-kernel.sh            # fetch, patch, config, build, install
tools/gts7l-kernel.sh build install
```

- Source tree: `gts7l-kernel/` at the main checkout root (gitignored), branch `gts7l` =
  tag `v7.3` + `patches/*.patch` applied with `git am`. Build output in `gts7l-kernel/out/`.
- Config: arm64 `defconfig` + `gts7l.fragment`; the script fails when a fragment value
  does not survive `olddefconfig`.
- Toolchain: clang/LLVM (`LLVM=1` when the host has `ld.lld`, otherwise the newest AOSP
  prebuilt clang under `aosp/prebuilts` or `gki/prebuilts`; the Arch host lacks `lld`).
  Built through `tools/limited-build.sh`, `-j8` on the E-cores.
- Output in `out/gts7l/` at the main checkout root: `Image.gz`, the DTBs,
  `Image.gz-dtb` (Image.gz + `sm8250-samsung-gts7l.dtb`, what Samsung's ABL boots from a
  v1 boot image), `Image.gz-dtb.gts7xlwifi` (the reference board), `modules/` (stripped,
  with `modules.load`), `config`, `kernelrelease`.

## Patch series

`patches/0001`-`0093` are imported unchanged from
[JeyKul/gts7xl-mainline](https://github.com/JeyKul/gts7xl-mainline) `linux-patches/`
at commit `96eb692fd477a341e5180fba4defb1003adac5e2` (2026-11-17). Upstream has no 0067.
Authorship stays in each patch's `From:` line (JeyKul, Claudius, Yumi Yukimura). The
patches change the Linux kernel and are GPL-2.0 like the files they touch; the
gts7xl-mainline repository itself is MIT (`patches/LICENSE.gts7xl-mainline`). Not
imported: upstream's `optional/` (RTC write) and `wip/` patches.

Ours start at `0094`:

| Patch | What |
|---|---|
| 0094 | `sm8250-samsung-gts7l.dts`: stub derived from the gts7xlwifi DTS |
| 0095-0099 | panel driver entry and stock on-commands, S Pen supply, C-PHY analog tuning from the DT (docs/gts7l.md §12) |
| 0100 | DTS: `mode-normal` in the IMEM reboot-mode, so a plain reboot clears a stale recovery reason (docs/gts7l.md §15) |
| 0101 | a6xx: accept the tablet's a650 SQE 0.93 firmware |
| 0102-0104 | NT36523 touchscreen in `novatek-nvt-ts`, enabled in the DTS (docs/gts7l.md §17) |
| 0105-0107 | panel fix: C-PHY without a clock lane, bonded slave PHY dividers, PLL bias on every enable (docs/gts7l.md §22) |
| 0220-0221 | Fuel gauge: DTS wires the MAX77705 FG alert interrupt; max17042 takes an optional platform IRQ (defers on `-EPROBE_DEFER`) and reports `cycle_count` in whole cycles (docs/gts7l-hal/power.md) |
| 0280 | ath11k: stamp beacons and probe responses with the boot time, so nl80211 reports `NL80211_BSS_LAST_SEEN_BOOTTIME` and Android rejoins an AP that restarted (docs/gts7l.md §23) |

To change a patch: edit in `gts7l-kernel/` on branch `gts7l`, commit, and export with
`git format-patch -1 --start-number N -o aosp-device/gts7l/kernel/patches`. The script
re-applies the series from `v7.3` whenever the patch files change (and refuses while the
tree has uncommitted changes).

To follow upstream: diff their `linux-patches/` against ours from the commit above,
import new patches with the next free numbers, and update the commit here.

## Config fragment

`gts7l.fragment` has three sections:

1. gts7xl-mainline's `android/kernel/gts7xl-android_defconfig` (same commit) reduced to
   what differs from arm64 defconfig. That config is their Linux config plus AOSP's
   `kernel/configs/v/android-6.6/android-base.config` and LineageOS's mainline fragments
   (binder/binderfs, cgroup v1 cpuset/memcg, uclamp, legacy iptables, f2fs + casefold,
   fscrypt, fs-verity, dm-verity, erofs, fuse, zram, uinput, QRTR/MHI built in, ...).
   Merged onto defconfig it reproduces their resolved config.
2. The other arm64 platforms of defconfig switched off (one SoC, much shorter build).
3. gts7l: `LOCALVERSION=-gts7l`, built-in command line with `androidboot.hardware=gts7l`
   and without the S7+ panel option.

## DTS status (`sm8250-samsung-gts7l.dts`)

A copy of the gts7xlwifi board, updated with what S1 read from the stock gts7l dtbo
(board revision 7, see `docs/gts7l-inventory.md`); everything else is the S7+'s wiring,
marked `TODO`. It builds (no new dtc warnings at `W=1`). Filled in from stock:

- `qcom,msm-id <356 0x20001>`, `qcom,board-id <8 7>`
- panel: Novatek NT36523 (CSOT PPA957DB1), 1600x2560 portrait, dual DSI, C-PHY 3 trios,
  reset gpio82, 1.8 V enable gpio135, bias-buck enable gpio93; DSI wiring after the
  Xiaomi Pad 5 Pro (`sm8250-xiaomi-elish-csot`); physical size 148x236 mm
- simple-framebuffer at 1600x2560
- Novatek touch at i2c5 0x62, IRQ gpio15; S Pen (0x56), pogo keyboard (0x2a) and the four
  CS35L41 (0x40-0x43) at the same addresses as the S7+

Still to do (S6, mostly drivers):

- panel driver: an entry in `panel-novatek-nt36523.c` with the stock on-commands and
  timings (clock 997999999 Hz, h 2/30/84, v 1/7/26, 120 Hz), then enable both DSI hosts
  and PHYs (disabled for now, the bootloader framebuffer is the display); DSI vdda supply
  (stock `pm8150a_l11`), ESD IRQs (gpio66, gpio46), the ISL98608 bias chip
- touch driver: mainline `novatek-nvt-ts` knows only NT11205/NT36672A (node disabled)
- RPMh regulators and always-on rails checked against stock
- S Pen PDCT (gpio7) and supply, speaker amp reset/IRQ GPIOs and TDM slots, pogo GPIOs
- the X55 LTE modem (PCIe/MHI, GPIOs, reserved memory, SIM detect)
- battery limits (capacity set to 8000 mAh)
