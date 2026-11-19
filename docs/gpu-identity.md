# GPU identity: apps see a Qualcomm Adreno

Mobile games pick their quality presets and resource packs from the GPU they see. They know
Qualcomm Adreno, Arm Mali and Apple GPUs, not an RTX 4070: Where Winds Meet, for one, ranks the
RTX 4070 `middle` and caps its best preset (the example below). So NVIDIA's Vulkan driver on
Android (`vulkan.nvidia`) tells app processes that the GPU is a Qualcomm Adreno instead: by
default an **Adreno 850**, the Snapdragon 8 Elite Gen 6 Pro's, the fastest Android GPU.

What an app sees:

| | NVIDIA (identity off) | Adreno (default) |
|---|---|---|
| Vulkan device name | `NVIDIA GeForce RTX 4070` | `Adreno (TM) 850` |
| Vendor ID | `0x10de` | `0x5143` (Qualcomm) |
| Device ID | the RTX 4070's | Qualcomm's chip ID |
| Device type | discrete | integrated |
| Driver ID and name | NVIDIA proprietary | `QUALCOMM_PROPRIETARY`, Qualcomm's driver name and info |
| Limits, features, extensions, formats | NVIDIA's | NVIDIA's (unchanged) |

Only the identity changes; the GPU, its speed and what it can do stay the RTX 4070's. System
processes (SurfaceFlinger, system_server, gpuservice) always see NVIDIA. GLES apps go through
ANGLE on the same Vulkan driver, so ANGLE should report the Adreno too (not checked yet).

## Status

| Build | Default |
|---|---|
| `eng.caoanh.20261119.1409` (commit 276bf5e, image `out/aosp17-usb-adreno850.img`) | Adreno 850, every app |
| `eng.caoanh.20261119.1548` (commit 2528884, image `out/aosp17-usb-adreno-off.img`) | NVIDIA; `persist.vendor.nvvk.adreno=1` turns the Adreno on |
| later builds (this commit) | Adreno 850, every app |

The stick runs build 1409 (since 2026-11-19). The user tested it on the PC with the Adreno 850
for every app and it works: games and apps recognise the Adreno. An earlier boot of the same
build had Where Winds Meet stop before its first textures and TikTok's login hang, which is why
build 1548 had it off; with the user's test passing, the Adreno 850 is the default again.

The properties below are persistent (`/data/property`), so a value set with `setprop`
survives stick updates (`tools/update-usb-aosp17.sh` keeps `userdata`) and wins over the
build's default.

## Properties

| Property | Values | Meaning |
|---|---|---|
| `persist.vendor.nvvk.adreno` | `1` on (default), `0` off | The Adreno identity for every app process (uid 10000 and up) |
| `persist.vendor.nvvk.adreno_model` | `850` (default), `740`, `750`, `830`, `840`, any 3-digit number; optional `:CHIPID` | The Adreno model; `850:0x44070041` also sets the chip ID |
| `persist.vendor.nvvk.adreno_exclude` | comma-separated process names | These processes see NVIDIA |
| `persist.vendor.nvvk.adreno_apps` | comma-separated process names | Non-app processes that see the Adreno too |

Chip IDs come from Mesa's `freedreno_devices.py`: 740 `0x43050a01`, 750 `0x43051401`, 830
`0x44050001`, 840 `0x44050a31`. The 850 is not in Mesa yet and takes the 840's.

A process reads the properties when it first uses Vulkan, so a change applies to apps started
after it: force-stop the app (or reboot) after a `setprop`.

## Getting root

The image is a `userdebug` build: `adb root` is allowed, and a root adb shell can set any
property. Apps on the PC have no root (Termux has no `su`; userdebug's `su` works only in the
adb shell), so root goes through adb. Two ways to connect:

### From another computer (Linux, Windows, macOS)

1. On the PC (Android): Settings → About → tap **Build number** seven times to unlock
   Developer options.
2. Settings → System → Developer options → turn on **Wireless debugging**. The PC and the
   other computer must be on the same network.
3. In Wireless debugging, tap **Pair device with pairing code**. It shows an IP, a port and a
   six-digit code.
4. On the other computer (with Android's platform-tools installed):

   ```bash
   adb pair 192.168.1.50:37123
   ```

   (the IP and port from the pairing dialog), then type the six-digit code.
5. Connect to the address shown on the Wireless debugging screen itself (a different port):

   ```bash
   adb connect 192.168.1.50:41235
   ```

6. Restart adbd as root, then reconnect:

   ```bash
   adb root
   adb connect 192.168.1.50:41235
   adb shell id
   ```

   `uid=0(root)` means root works. If `adb root` changes the port, read the new one on the
   Wireless debugging screen.

### On the PC itself, from Termux

Termux can run adb against the PC's own adbd:

```bash
pkg install android-tools
```

Open Wireless debugging and Termux side by side (desktop mode windows), then in Termux:

```bash
adb pair 127.0.0.1:37123
adb connect 127.0.0.1:41235
adb root
adb connect 127.0.0.1:41235
```

with the ports from the pairing dialog and the Wireless debugging screen. Wireless debugging
needs a network connection (Ethernet or Wi-Fi) to turn on, even for `127.0.0.1`.

## Changing the GPU

Nothing to do for the default (Adreno 850 for every app). To change it, from a root adb shell
(`adb shell`, prompt ending in `#`):

On build 1548, where the default is NVIDIA:

```bash
setprop persist.vendor.nvvk.adreno 1
```

Another model, e.g. the Snapdragon 8 Gen 2's Adreno 740:

```bash
setprop persist.vendor.nvvk.adreno_model 740
```

Keep one app on NVIDIA (several: comma-separated, no spaces):

```bash
setprop persist.vendor.nvvk.adreno_exclude com.zhiliaoapp.musically,com.google.android.webview
```

Back to NVIDIA for every app:

```bash
setprop persist.vendor.nvvk.adreno 0
```

Read the current values:

```bash
getprop | grep nvvk
```

Then force-stop the app (Settings → Apps → the app → Force stop) or reboot.

## Checking what an app sees

`vulkan.nvidia` logs one line per process that gets the Adreno:

```bash
adb logcat -d | grep 'sees the GPU as'
```

```
com.netease.yyvn sees the GPU as an Adreno 850 (0x44050a31)
```

`cmd gpu vkjson` runs in gpuservice, a system process that sees NVIDIA; to see the Adreno
there, list it first:

```bash
setprop persist.vendor.nvvk.adreno_apps /system/bin/gpuservice
cmd gpu vkjson | grep -E '"deviceName"|"vendorID"|"deviceID"'
setprop persist.vendor.nvvk.adreno_apps ''
```

Apps such as AIDA64 or CPU-Z (Play Store) show the GPU the same way an app sees it.

## If an app misbehaves

Some apps act on the GPU name: a WebView page, Google Play services or a game's own login may
take a different path for an Adreno. If an app hangs or crashes with the identity on and
works with it off, exclude that app's process (`adreno_exclude`, above). The process name is
the package name for most apps; `adb shell ps -A | grep <package>` lists the exact names
(`com.google.android.gms:persistent`, `com.google.android.webview:sandboxed_process0` ...).

## Example: Where Winds Meet

### Why the best preset looks soft

2026-11-19, AOSP 17 image in Waydroid, NVIDIA 620.70.06, the user's data: the best preset
("Tuyệt phẩm") still looks soft. The game's own choice, not the graphics stack:

- **Not the stack.** The game's swapchain is 2560x1068 RGBA8 (the Android display, the
  `persist.waydroid.width/height` pin), and SurfaceFlinger composes it 1:1 (`dumpsys
  SurfaceFlinger`: source crop = display frame, no scaling). The game runs Vulkan on
  `vulkan.nvidia` (`last_graphics_api.tag` = `vlk`, `VkPipeline.cache`, the process maps
  NVIDIA's libraries); the device has `samplerAnisotropy` (16x), BC, ASTC/ETC2 (decoded
  losslessly to RGBA8), Vulkan 1.4.
- **The game ranks the device "middle".** Its log (`LocalData/game_account.log`, every byte
  +1) records a `default_render_level` per login: `middle` for this device. It was `high` only
  on 2026-11-15 18:11-19:51, while the earlier Venus setup reported the GPU as "Adreno (TM)
  740" (`~/projects/waydroid-nvidia`, `venus/notes-wwm.md`); NVIDIA's own name gets `middle`.
  `LocalData/crash_extra`: `default level: middle`, `setting level: high`.
- **"Tuyệt phẩm" is the `high` level, render set `AndroidHigh`.** `Engine/Config/
  AllRenderOptions.bin` (Messiah's binary config: u16-length keys, a type byte, `p` = u32 length
  + blob) has `AndroidFastLow2` ... `AndroidHigh`, `AndroidHigh2`. `AndroidHigh` against
  `AndroidHigh2` (and `PCHigh`):

  | option | AndroidHigh | AndroidHigh2 | PCHigh |
  |---|---|---|---|
  | `ScreenScale` (3D render scale) | 0.7 | 0.85 | 1.0 |
  | `ScreenMinSize` / `ScreenSize` (render width clamp) | 1300 / 1920 | - | - |
  | anti-aliasing | SMAA | TSAA (TAA quality 3) | TSAA |
  | `EnableVirtualTexture565` (terrain pages in RGB565) | 1 | 0 | 0 |
  | `MaxTextureSize` | 8 | 7 | 7 |
  | realtime reflections, transparency scale | 0.5, 0.5 | 1.0, 1.0 | 1.0, 1.0 |
  | bloom downsample, CSM tile size | 4, 15 | 2, 100 | 2, 100 |

  So the 3D scene is drawn at about 1792x748 and stretched to 2560x1068 with RCAS sharpening;
  screenshots show it (soft rocks and grass, sharp HUD).
- **The resources are the "mid" pack.** `LocalData/patch_conf.json`: `"patch_level":"mid"`, and
  every downloaded list is `patchlist_mid_*`: the textures themselves are the mid-range phone
  set, chosen at the first start (2026-11-15 14:03, rank `middle`).
- The game also flags the device as an emulator (`LocalData/Patch/emulator.tag`); the earlier
  notes found that NetEase's Pharos reports `is_ad_simulator=1` whatever the properties say.

`libGame.so` and the scripts are encrypted, so which rank unlocks `AndroidHigh2` or the high
pack could not be read.

### With the Adreno identity

With `/system/bin/gpuservice` in the list, `cmd gpu vkjson` reports "Adreno (TM) 740",
`0x5143`/`0x43050a01` and driver ID 8. In the game (2026-11-19 10:42): logcat has `com.netease.yyvn
sees the GPU as an Adreno 740`, and the game's rank goes from `middle` to `high`
(`game_account.log` `default_render_level: high`, `crash_extra` `default level: high`); it
loads, reaches the world and plays on Vulkan, no crash in the first minutes.

The graphics menu (Hình ảnh > Chi tiết hình ảnh) has the presets Tiết kiệm pin, Cân bằng, Chất
lượng, Tuyệt phẩm, **Nguyên bản** and Tùy chỉnh. "Nguyên bản", above "Tuyệt phẩm", stays greyed
out with the `high` rank; TAA is a separate switch (on). The resource pack is still `mid`. Open:
what unlocks "Nguyên bản" (a lock icon on it: the device, by GPU or model) and whether the game
offers the high pack now. Adreno 850 (11:04, the user's request): `cmd gpu vkjson` with
gpuservice listed reports "Adreno (TM) 850", `0x44050a31`, "Driver Build: 800.0"; the game logs
`sees the GPU as an Adreno 850`, ranks it `high` as with the 740, and "Nguyên bản" keeps its
lock. So the lock does not follow the GPU name or chip ID alone; the model (`SM-S918B`), the
emulator flag or a server-side list are left. Changing the model would show the account a new
device, so it was not tried.

### The SoC is not changed

The SoC stays the PC's (`ro.soc.*`, `ro.board.platform`, `/proc/cpuinfo`). WWM's Java code
reads `SOC_MODEL`, `SOC_MANUFACTURER`, `ro.board.platform`, `ro.hardware` and `/proc/cpuinfo`,
but in NetEase's device-info and anti-cheat collectors (`DiInfo`, `BUILD_SOC_MODEL_SET`) and
Glide, not where the rank is logged (`device_model` = brand, model, CPU name, cores, clock);
the quality logic itself is in encrypted native code. A Snapdragon SoC on an x86_64 CPU with
x86 `ro.hardware` would be one more inconsistency for those collectors to report, and the
properties are system-wide (every app and the system would see them), so it was not done.

### History

First an opt-in experiment for listed processes
(31f9f35, b43affd), then on for every app (276bf5e), off again in build 1548 (2528884), and on
by default once more after the user's test on the PC. On the PC's first boot of build
`eng.caoanh.20261119.1409` (boot 34) WWM never reached its first texture upload in three
starts, its first start hit an ANR, and TikTok's login (a WebView page) hung; the Waydroid test
had given only WWM's own process the Adreno. In the user's later test of the same build the
games and apps worked with the Adreno for every app. If one hangs, see "If an app misbehaves".

On the stick (2026-11-19 14:2x): build `eng.caoanh.20261119.1409` (commit 276bf5e;
`vendor.img` has the new `vulkan.nvidia`), image `out/aosp17-usb-adreno850.img`
(`tools/mkusb-aosp17.sh --userdata 64`; its ESP has the one NVIDIA entry,
`androidboot.hardware.vulkan=nvidia`, no ranchu), written with `tools/update-usb-aosp17.sh`
over `eng.caoanh.20261119.0009`: "Read back and compared: OK", Google extras written back.
Booted on the PC: the failures above. Replaced (2026-11-19 15:5x) by build
`eng.caoanh.20261119.1548` with the identity off by default, image
`out/aosp17-usb-adreno-off.img` (same way, one NVIDIA entry), written with
`tools/update-usb-aosp17.sh`: "Read back and compared: OK". Not yet booted on the PC.
Then (2026-11-19, the user's request) the stick went back to build 1409 with the Adreno 850 for
every app (`out/aosp17-usb-adreno850.img` over `eng.caoanh.20261119.0009`, "Read back and
compared: OK", Google extras written back); the user tested it on the PC and games and apps
recognise the Adreno.

## Where it lives

- Code: `ReportsAdreno()` in `aosp-device/hals/nvgpu/vulkan/nvvk_wsi.cpp`.
- The same driver in Waydroid: [waydroid-nvidia.md](waydroid-nvidia.md).
- The driver itself: [nvidia-native.md](nvidia-native.md).
