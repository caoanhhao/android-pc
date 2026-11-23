# gts7l inventory (step S1, phase T0)

Read-only inventory of the SM-T875 on the desk (serial `R52R504WFTK`, board revision 7) and
its stock firmware. Nothing was written to the tablet. Source data lives in `gts7l-stock/`
of the `gts7l` worktree (gitignored); regenerate it with the commands in "How it was made".

## Stock firmware (the way back)

`samloader` (github.com/ananjaser1211/samloader), model `SM-T875`, region `XXV`, the TAC of
the tablet's IMEI as `-i`:

| File | Size | Content |
|---|---|---|
| `SM-T875_4_20240807153400_jo1w0rgrtr_fac.zip` | 7.4 GB | decrypted Odin package, version `T875XXS8DXH1/T875OXM8DXH1/T875XXS8DXH1/T875XXS8DXH1` |
| `BL_T875XXS8DXH1_..._MULTI_CERT.tar.md5` | 96 MB | xbl, abl, tz, hyp, aop, devcfg, `NON-HLOS.bin` (apnhlos), `dspso.bin` (dsp), vbmeta |
| `AP_T875XXS8DXH1_..._meta_OS13.tar.md5` | 8.2 GB | boot, recovery, dtbo, vbmeta, vbmeta_samsung, super, persist, userdata, misc |
| `CP_T875XXS8DXH1_..._MULTI_CERT.tar.md5` | 66 MB | `modem.bin` (X55) |
| `CSC_OXM_...`, `HOME_CSC_OXM_...` | 725 MB | CSC (`HOME_` keeps data) |

Odin (or `odin4`/Heimdall) with these four tars restores the tablet; the bootloader is
revision 8 (`S8` in the version), never flash older ones.

## Partitions (UFS, `/proc/partitions` + `/dev/block/by-name`)

| Name | Block | Size | Notes |
|---|---|---|---|
| boot | sda23 | 68 MiB (71303168 B) | our kernel + ramdisk |
| recovery | sda24 | 82.9 MiB | TWRP goes here |
| dtbo | sda22 | 10 MiB | stock: 9 board overlays; ours: blanked/own |
| super | sda29 | 9932800 KiB (10171187200 B) | dynamic partitions, layout below |
| vbmeta / vbmeta_samsung | sdd25 / sda27 | 64 KiB each | disabled-verity vbmeta after unlock |
| param | sda10 | 10 MiB | Samsung boot params (TWRP/custom flag) |
| metadata | sda28 | 32 MiB | |
| cache | sda32 | 600 MiB | |
| userdata | sda37 | 107 GiB | |
| apnhlos | sda19 | 172 MiB | vfat, = `NON-HLOS.bin`: DSP/GPU-zap/Venus firmware |
| modem | sda20 | 195 MiB | X55 modem firmware |
| dsp | sda21 | 64 MiB | ext4, = `dspso.bin`: DSP shared objects (FastRPC) |
| persist | sda8 | 32 MiB | sensor calibration, back up |
| efs / sec_efs | sda9 / sda12 | 20 MiB each | IMEI, MAC/BT address, calibration, back up |
| modemst1/2, fsg, fsc | sda1/sda2, sdd3, sda3 | 2 MiB / 2 MiB / — | modem NV, back up |
| mdm1m9kefs1/2/3/c | sda4, sda5, sdd4, sda6 | 2 MiB | X55 EFS, back up |
| bluetooth | sdd10 | 1 MiB | |

The full map is `gts7l-stock/partmap.txt`.

### super layout

`lpdump` of the stock `super.img`: metadata version 10.0, max size 65536, **2 slots**, block
device `super` 10171187200 B, group `qti_dynamic_partitions` max **10166992896 B**
(device size minus 4 MiB), all partitions read-only:

| Partition | Sectors (512 B) | Size |
|---|---|---|
| system | 13714800 | 6.5 GiB |
| vendor | 2652032 | 1.26 GiB |
| product | 2475544 | 1.18 GiB |
| odm | 8496 | 4.1 MiB |

Our `BoardConfig.mk`: `BOARD_SUPER_PARTITION_SIZE := 10171187200`,
`BOARD_SUPER_PARTITION_METADATA_DEVICE := super`, one group (e.g. `gts7l_dynamic_partitions`)
of at most `10166992896`, non-A/B (2 metadata slots, as stock).

## Boot image

Stock `boot.img`: **header v2**, page size 4096, kernel 4.19 `Image.gz` (55 MB) at
`0x8000`, ramdisk at `0x02000000`, tags at `0x01e00000`, **DTB in the header** (1.5 MB at
`0x01f00000`, four concatenated DTBs), `os_version` 11.0.0, patch level 2024-08. Command line:

```
console=null androidboot.hardware=qcom androidboot.memcg=1 lpm_levels.sleep_disabled=1
video=vfb:640x400,bpp=32,memsize=3072000 msm_rtb.filter=0x237 service_locator.enable=1
androidboot.usbcontroller=a600000.dwc3 swiotlb=2048 printk.devkmsg=on
firmware_class.path=/vendor/firmware_mnt/image loop.max_part=7
```

So ABL takes a header-v2 image with the DTB in the `dtb` section; header v1 with an appended
DTB (what gts7xl-mainline uses) should also work, but our build can use v2 like stock (S4).
ABL picks the base DTB by `qcom,msm-id` and the overlay from `dtbo` by `qcom,board-id`.

| Base DTB | Model | msm-id |
|---|---|---|
| dtb/00 | kona v2.1 SoC | `<0x164 0x20001>` (SM8250 v2.1) |
| dtb/01 | kona v2 SoC | `<0x164 0x20000>` |
| dtb/02 | kona v1 SoC | `<0x164 0x10000>` |
| dtb/03 | (empty) | |

| dtbo entry | Model | board-id |
|---|---|---|
| 04 | **Samsung GTS7L PROJECT - PV REV0.4 (board-id,7)** — this tablet | `<0x08 0x07>` |
| 00-03, 05-08 | earlier DV/PV boards 0-6 | `<0x08 0..6>` |

Our DT must carry `qcom,msm-id = <0x164 0x20001>` (or 0x20000) and `qcom,board-id =
<0x08 0x07>` so ABL accepts it.

## Hardware (from dtbo entry 04, board-id 7)

| Function | Part | Bus / address | Mainline |
|---|---|---|---|
| Display panel | **Novatek NT36523**, CSOT `PPA957DB1`, 1600x2560 portrait (2 x 800x2560 halves), 120 Hz video mode, burst, 24 bpp, **C-PHY 3 lanes per DSI, dual DSI (ctrl 0+1)**, 148x236 mm | `mdss_dsi0` + `mdss_dsi1` | `panel-novatek-nt36523` exists (D-PHY panels); needs our init sequence and **DSI C-PHY on the 7 nm PHY** — the main display risk |
| Panel power | ISL98608 display buck, `fixed_reg_lcd_1p8`, `fixed_reg_buck_en` | GPIO regulators | ISL98608: no mainline driver, small regulator driver (ours) |
| Backlight | in panel (DCS), levels 4..462 | DSI | panel driver |
| Touch | **Novatek NT36xxx** (`novatek,nvt-ts`) | `qupv3_se5_i2c` @ 0x62 | `novatek-nvt-ts` (NT11205) — check NT36523 touch protocol fit |
| S Pen | **Wacom W90xx** (`wacom,w90xx`) | `qupv3_se17_i2c` @ 0x56 | `wacom_i2c` (needs W9xx protocol bits, as gts7xl) |
| Book Cover keyboard | STM32 pogo MCU (`stm,stm32_pogo`, `stm,keypad`, `stm,touchpad`) | `qupv3_se9_i2c` @ 0x2a | gts7xl series |
| Speakers | 4x **Cirrus CS35L41** | `qupv3_se11_i2c` @ 0x40-0x43 | `cs35l41` (ASoC) |
| Charger / fuel gauge / Type-C | **MAX77705** | `qupv3_se0_i2c` @ 0x66 | max77705 charger/fg (mainline since 6.x) |
| Direct charger | **NXP PCA9468** | `qupv3_se0_i2c` @ 0x57 | none, later |
| USB-C redriver | Parade PS5169 | `qupv3_se14_i2c` @ 0x28 | none (needed for DP alt mode only) |
| Grip sensors | A96T3x6 x2 | bit-banged I2C @ 0x20 | none, skip |
| Wi-Fi / BT | **QCA6390** | PCIe + UART (`bt_qca6390`) | `ath11k_pci`, `hci_qca` |
| Haptics | `qcom,haptics` (PMIC) + `vibrator` | PMIC | spmi haptics (gts7xl) |
| Hall | `hall` (flip cover), `hall_wacom` (S Pen dock) | GPIO | `gpio-keys` switch events |
| Fingerprint | Goodix (`goodix,fingerprint`) on SPI, TrustZone-owned | — | out of scope |
| Cameras | 3 sensors + actuator + EEPROMs on CCI | CAMSS | later (T7) |
| NFC | `nq@28` (NXP) in the Qualcomm base, likely unused on LTE board | — | skip |

Differences from gts7xl (SM-T970): LCD NT36523 C-PHY panel instead of the AMOLED, Novatek
touch instead of STM FTS, X55 LTE modem, PCA9468 direct charger. The common parts (SoC,
MAX77705, CS35L41, QCA6390, Wacom, STM32 pogo) can follow the gts7xl series.

## Firmware the kernel loads

From `apnhlos` (`NON-HLOS.bin`, vfat, `image/`): `a650_zap.{mdt,b0*}`, `adsp.{mdt,b*}`,
`cdsp.{mdt,b*}`, `slpi.{mdt,b*}`, `venus.{mdt,b*}`, `cvpss.*`, the `.jsn` PD maps
(`adspr.jsn`, `adspua.jsn`, `cdspr.jsn`, …), plus Samsung TrustZone apps we do not use. Full
list: `gts7l-stock/blcp/nonhlos-files.txt`.

From `modem` (`modem.bin`): X55 firmware (T7).

From `dsp` (`dspso.bin`, ext4): FastRPC shared objects for the ADSP/CDSP/SLPI (sensors
registry, audio modules).

From the stock `vendor` (in `super`, `/vendor/firmware`): `a650_sqe.fw`, `a650_gmu.bin`
(GPU), `qca6390/{amss20.bin,m3.bin,bdwlan.e05,bdwlan.elf*,regdb.bin}` (ath11k), `htbtfw20.tlv`
+ `htnv20.bin` (QCA6390 Bluetooth), `cs35l40-spk-dsp1-spk-prot{.wmfw,.bin,-calib.bin}` (speaker
protection DSP for the CS35L41s), `ipa_fws.*`, `ipa_uc.*` (IPA, LTE data path), `CAMERA_ICP.elf`.
Our `super` replaces the stock vendor, so `tools/gts7l-firmware.sh` copies the ones the
kernel needs into a `gts7l_fw` logical partition in our `super`, built on this host only and
never in git (docs/gts7l.md §13).

## How it was made

```
samloader -m SM-T875 -r XXV -i <TAC> checkupdate
samloader -m SM-T875 -r XXV -i <TAC> download -v <version> -O gts7l-stock
adb shell cat /proc/partitions; adb shell ls -l /dev/block/by-name
unpack_bootimg.py --boot_img boot.img --out boot-unpacked
split concatenated DTBs and the dt_table of dtbo.img, dtc -I dtb -O dts
mdir -/ -b -i NON-HLOS.bin ::
simg2img super.sparse super.raw; lpdump super.raw; lpunpack -p vendor super.raw
```
