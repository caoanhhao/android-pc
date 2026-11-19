# GPU identity: apps see a Qualcomm Adreno

Mobile games pick their quality presets and resource packs from the GPU they see. They know
Qualcomm Adreno, Arm Mali and Apple GPUs, not an RTX 4070: Where Winds Meet, for one, ranks the
RTX 4070 `middle` and caps its best preset. So NVIDIA's Vulkan driver on Android
(`vulkan.nvidia`) can tell app processes that the GPU is a Qualcomm Adreno instead.

What an app sees with the identity on:

| | NVIDIA (off) | Adreno (on) |
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
| `eng.caoanh.20261119.1409` (commit 276bf5e, image `out/aosp17-usb-adreno850.img`) | on, Adreno 850, every app |
| `eng.caoanh.20261119.1548` and later (commit 2528884) | off; `persist.vendor.nvvk.adreno=1` turns it on |

The stick runs build 1409 (since 2026-11-19). The user tested it on the PC with the Adreno 850
for every app and it works: games and apps recognise the Adreno. An earlier boot of the same build had
Where Winds Meet stop before its first textures and TikTok's login hang, which is why later
builds have it off by default. The setting is a persistent property in `/data`, so it
survives stick updates (`tools/update-usb-aosp17.sh` keeps `userdata`): set
`persist.vendor.nvvk.adreno 1` once and a later build that is off by default keeps it on.

## Properties

| Property | Values | Meaning |
|---|---|---|
| `persist.vendor.nvvk.adreno` | `1` on, `0` off | Turns the identity on for every app process (uid 10000 and up) |
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

With a root adb shell (`adb shell`, prompt ending in `#`):

```bash
setprop persist.vendor.nvvk.adreno 1
setprop persist.vendor.nvvk.adreno_model 850
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

## Where it lives

- Code: `ReportsAdreno()` in `aosp-device/hals/nvgpu/vulkan/nvvk_wsi.cpp`.
- Background, Where Winds Meet's ranks and the Waydroid tests:
  [waydroid-nvidia.md](waydroid-nvidia.md), "Apps can see an Adreno 850".
- The driver itself: [nvidia-native.md](nvidia-native.md).
