# gts7l graphics HALs: check with evidence

Device: Galaxy Tab S7 LTE (SM-T875), `adb -s R52R504WFTK`, adbd as root.
Build: `Android/aosp_gts7l/gts7l:17/CP2A.260605.016/eng.caoanh.20261124.1208:userdebug/test-keys`.
Checked on 2026-11-25, right after a reboot (uptime 1-5 min). All checks were read-only,
except a small Vulkan test binary pushed to `/data/local/tmp`.

The physical panel shows only about 160 lines of DSI0 (a kernel DSI/C-PHY bug the panel
sessions own), so this file judges the HALs by what Android produces (SurfaceFlinger,
drm_hwcomposer statistics, screencap, frame stats, GPU clocks), not by the panel picture.

| HAL | Result |
|---|---|
| `android.hardware.graphics.allocator` (minigbm msm, `vendor.graphics.allocator`) | **Works** |
| `android.hardware.graphics.composer3` (drm_hwcomposer, `vendor.hwcomposer-3`, card1) | **Partial**: every frame reaches KMS, but nothing is offloaded to DPU planes (all layers CLIENT) |
| Mesa freedreno EGL/GLES 3.2 | **Works** |
| Mesa Turnip Vulkan 1.3 | **Works** (driver loads, compute and AHardwareBuffer import verified); not yet used by any Android client |

## 1. Services and properties

```
$ adb shell 'for p in vendor.graphics.allocator vendor.hwcomposer-3 surfaceflinger; do echo "$p=$(getprop init.svc.$p) pid=$(getprop init.svc_debug_pid.$p)"; done'
vendor.graphics.allocator=running pid=635
vendor.hwcomposer-3=running pid=651
surfaceflinger=running pid=665

$ adb shell ps -A -o PID,USER,ETIME,NAME | grep -E "allocator|composer|surfaceflinger"
  635 system  01:18 android.hardware.graphics.allocator-service.minigbm
  651 system  01:18 android.hardware.composer.hwc3-service.drm
  665 system  01:18 surfaceflinger

$ adb shell 'service list | grep -E "allocator|composer"'
28  android.hardware.graphics.allocator.IAllocator/default: [android.hardware.graphics.allocator.IAllocator]
29  android.hardware.graphics.composer3.IComposer/default: [android.hardware.graphics.composer3.IComposer]

$ adb shell getprop | grep -E "hwc|gralloc|egl|vulkan|hwui.renderer"
[debug.hwui.renderer]: [skiagl]
[ro.hardware.egl]: [mesa]
[ro.hardware.gralloc]: [minigbm]
[ro.hardware.vulkan]: [freedreno]
[vendor.hwc.drm.device]: [/dev/dri/card1]

$ adb shell ls -lZ /dev/dri
crw-rw-rw- 1 root graphics u:object_r:graphics_device:s0  226,   1 card1
crw-rw-rw- 1 root graphics u:object_r:gpu_device:s0       226, 128 renderD128

$ adb shell ls /vendor/lib64/hw /vendor/lib64/egl   (trimmed)
gralloc.minigbm.so  mapper.minigbm.so  vulkan.freedreno.so
libEGL_mesa.so  libGLESv1_CM_mesa.so  libGLESv2_mesa.so
```

Kernel (`adb logcat -d -b kernel`, trimmed):

```
msm_dpu ae01000.display-controller: bound 3d00000.gpu (ops a3xx_ops)
[drm] Initialized msm 1.13.0 for ae01000.display-controller on minor 1
msm_dpu ...: [drm:adreno_request_fw] *ERROR* failed to load a650_sqe.fw      <- at 1.28 s, before /vendor is mounted
msm_dpu ...: [drm:adreno_request_fw] loaded qcom/a650_sqe.fw from new location   <- at 7.27 s
adreno 3d00000.gpu: a650 SQE ucode 93 is older than 95, which has CP security fixes; using it anyway
[drm] Loaded GMU firmware v2.1.8
adreno 3d00000.gpu: [drm:adreno_zap_shader_load] r8q: zap region dma_alloc'd at 0x88400000
```

No GPU fault, hang or recovery message in the kernel log, before or after the GPU load test
(`dmesg | grep -i -E "fault|hang|recover|adreno|a6xx"` shows only the lines above).

## 2. Allocator (minigbm, msm backend): Works

Evidence:

- The service runs with no restarts and is registered (section 1). The process maps
  `libminigbm_gralloc.so` (`/proc/635/maps`).
- Every graphics client gets the allocator from servicemanager: SurfaceFlinger, composer,
  system_server, SystemUI, bootanim and untrusted apps (logcat: `servicemanager: Caller(pid=…)
  Found android.hardware.graphics.allocator.IAllocator/default`). No allocator or minigbm
  error in logcat since boot.
- SurfaceFlinger holds buffers allocated by it, with the Qualcomm UBWC modifier
  (`dumpsys SurfaceFlinger`, trimmed):

```
GraphicBufferAllocator buffers:
            Handle |         Size |     W (Stride) x H | Layers | Format |  Usage | Requestor
0xb400e9449c814b00 | 16000.00 KiB | 1600 (1600) x 2560 |      1 |      1 | 0x1b00 | LegacyFramebufferSurface
0xb400e9449c816730 | 16000.00 KiB | 1600 (1600) x 2560 |      1 |      1 | 0x1b00 | LegacyFramebufferSurface
Imported gralloc buffers:
+ name:VRI[SettingsHomepageActivity]#11(BLAST Consumer)11, id:179, size:7739.11KiB, w/h:1263x1516, usage: 0xb00, req fmt:1, fourcc/mod:875708993/360287970189639681, ...
+ name:VRI[DeskClock]#1(BLAST Consumer)1, id:164, size:3875.11KiB, w/h:1263x753, usage: 0xb00, ...
+ name:AHardwareBuffer pid [1093], id:186, size:215.11KiB, w/h:2x733, usage: 0x100, ...
```

  fourcc 875708993 = `AB24` (ABGR8888); modifier 360287970189639681 = `0x0500000000000001`
  = `DRM_FORMAT_MOD_QCOM_COMPRESSED` (UBWC), so minigbm's msm backend is in use, not linear
  dumb buffers.
- A native client allocates directly, and the mapper locks it for the CPU (the test binary of
  section 5): `AHardwareBuffer_allocate(BLOB 4194304 bytes) = 0`, then GPU writes into it
  and a CPU `AHardwareBuffer_lock` read gets 1048576 correct values.

## 3. Composer3 (drm_hwcomposer on card1): Partial

What works:

- HWC3 service runs, SurfaceFlinger uses it, `h/w composer enabled`, version 5 (`hwc3-drm`).
- One display, `Display 0 (HWC display 0)`, `hwcId=2, resolution=1600x2560, vsyncRate=120.00 Hz`.
  No EDID (`no display identification data`; DSI panel, expected).
- Present fences work (`PresentFences=true`, sync `EGL_ANDROID_native_fence_sync EGL_KHR_wait_sync`).
- Every frame reaches KMS: `Failed to commit frames: 0` out of 2397.

What does not:

- **No layer is ever composed by the display hardware.** drm_hwcomposer's test commit (the
  atomic `TEST_ONLY` check of a plane assignment) fails for nearly every frame, so it falls back
  to client (GPU) composition of the whole screen. All 20 visible layers are `CLIENT`, and
  SurfaceFlinger reports `usesDeviceComposition=false usesClientComposition=true`.

```
$ adb shell dumpsys SurfaceFlinger   (trimmed)
-- drm_hwcomposer --
- Display on: display-36
Statistics since system boot:
 Total frames count: 2397
 Failed cursor test commit frames: 0
 Failed to test commit frames: 2290
 Failed to commit frames: 0
 Flattened frames: 5
 Cursor plane frames: 3
 Pixel operations (free units) : [TOTAL: 36301161046 / GPU: 35878872918]
 Composition efficiency: 0.0116329
OverlayProperties{combinations: [], supportMixedColorSpaces: false, lutProperties: (null)}

Display 0 HWC layers (top to bottom):
 Layer name
     Z | Window Type | Comp Type | Transform | Disp Frame (LTRB)   | Source Crop (LTRB)
 VRI-HyprBar#64
    36 |        2041 |    CLIENT |    ROT_90 | 1540    0 1600 2560 | 0.0 0.0 2560.0 60.0
 ...   (all 20 layers: CLIENT, ROT_90)
     1 |           0 |    CLIENT |    ROT_90 |    0    0 1600 2560 | 98.0 586.0 2061.0 1813.0

Display 0 (physical, "Primary display")
   isEnabled=true isSecure=true usesDeviceComposition=false
   usesClientComposition=true ...
    Total missed frame count: 74
    HWC missed frame count: 15
    GPU missed frame count: 60
```

- logcat spam from the composer on every frame (9517 lines in the first ~4 minutes):

```
651   651 W drmhwc  : Encountered multiple cursor planes for CRTC 95. Ignoring plane 89
```

- The panel is portrait (1600x2560) and the desktop runs landscape, so every layer carries
  `ROT_90`. The likely reasons the test commit fails are this 90° rotation (DPU SSPP planes on
  mainline msm do not do 90° rotation without the inline rotator) and the UBWC modifier; this
  could not be confirmed without the DRM atomic state (debugfs is not mounted, see the manual
  list). Not fixed here (check only).
- SELinux denials for the composer (permissive, so not blocking): it writes the backlight
  `brightness` under `sysfs` and reads its `netlink_kobject_uevent_socket`. They need policy
  before enforcing.

Verdict: the HAL does its job (modes, vsync, fences, every frame committed), so the screen
content is right, but all composition goes through the GPU; no overlay offload.

## 4. Mesa freedreno EGL/GLES: Works

SurfaceFlinger's RenderEngine (SkiaGL) runs on Mesa freedreno and composes every frame
(section 3 shows every layer is CLIENT, so all screen content passes through GLES):

```
$ adb shell dumpsys SurfaceFlinger   (trimmed)
------------RE GLES (Ganesh)------------
EGL_ANDROID_front_buffer_auto_refresh EGL_ANDROID_get_native_client_buffer ... EGL_ANDROID_image_native_buffer EGL_ANDROID_native_fence_sync ... EGL_KHR_wait_sync
GLES: freedreno, FD650, OpenGL ES 3.2 Mesa 26.1.0-devel (git-875160ed32)
RenderEngine supports protected context: 0
```

Apps render with HWUI SkiaGL on the GPU, with GPU-side times measured:

```
$ adb shell dumpsys gfxinfo com.android.systemui   (trimmed)
Total frames rendered: 826
Janky frames: 38 (4.60%)
50th percentile: 13ms   90th: 19ms   95th: 25ms   99th: 117ms
50th gpu percentile: 3ms   90th: 6ms   95th: 8ms   99th: 25ms
Pipeline=Skia (OpenGL)
```

`dumpsys gpu`: `glLoadingCount = 4`, `glLoadingFailureCount = 0`, `glesVersion = 196610` (3.2).

Screencap (`adb exec-out screencap -p > screencap.png`): a 2560x1600 RGBA PNG with correct
content: the HyprBar status bar ("Apps", clock "Tue 24 Nov 05:13"), a Settings search window, DeskClock's
alarm list and Settings "Connected devices" (MeetionKB1, Logi M196). The image comes from the
same GPU-composed client target that SurfaceFlinger sends to the display.

Warnings seen (not failures):

```
834 2315 I MESA    : Using gralloc0 CrOS API
834 2315 W EGL-MAIN: failed to get driver name for fd -1
834 2315 W EGL-MAIN: MESA-LOADER: failed to retrieve device information
834 2315 W HWUI    : Failed to initialize 101010-2 format, error = EGL_SUCCESS
834 2315 E HWUI    : Device claims wide gamut support, cannot find matching config, error = EGL_SUCCESS
```

The `fd -1` lines are Mesa's loader probing before it picks the render node; the HWUI ones mean
no RGBA1010102 / wide-gamut EGL config in system_server (SDR still works; `Device supports
wide color: 0` in SurfaceFlinger).

## 5. Mesa Turnip Vulkan: Works

Driver enumeration through gpuservice:

```
$ adb shell cmd gpu vkjson > vkjson.txt     (120 KB JSON, parsed)
Turnip Adreno (TM) 650  api 1.3.346  driverVersion 0x6800063  vendor 0x5143  deviceID 0x6050002  type 1 (integrated)
extensions 167, queueFamilies 2, memoryHeaps [{flags: 1, size: 0x104e11000}]
driverProperties: driverName "turnip Mesa driver", driverInfo "Mesa 26.1.0-devel (git-875160ed32)",
                  driverID 18, conformanceVersion 1.2.7.1
VK_ANDROID_external_memory_android_hardware_buffer: present

$ adb shell dumpsys gpu   (trimmed)
vulkanVersion = 4206592      (1.3.0, from the vendor feature XML)
vkLoadingCount = 0           (no app uses Vulkan: HWUI is skiagl, RenderEngine is GLES)
```

Real work on the GPU: a small test binary,
[`graphics-vkprobe/vkprobe.c`](graphics-vkprobe/vkprobe.c) (compute shader
[`graphics-vkprobe/sq.comp`](graphics-vkprobe/sq.comp), compiled with `glslc`, the binary built with AOSP
clang against the NDK sysroot of `out/soong/ndk/sysroot`, API 34). It writes `v[i] = i*i` for
1M elements (1) into a host-visible VkBuffer, (2) into a BLOB AHardwareBuffer from the gralloc
allocator imported with `VK_ANDROID_external_memory_android_hardware_buffer` and read back with
`AHardwareBuffer_lock`, then (3) keeps the GPU busy for 8 s and checks the result again:

```
$ adb push vkprobe /data/local/tmp/ && adb shell /data/local/tmp/vkprobe 8
device: Turnip Adreno (TM) 650, api 1.3.346, driver: turnip Mesa driver (Mesa 26.1.0-devel (git-875160ed32))
test1 compute into VkBuffer: 1048576 elements, 0 wrong, 4.38 ms
test2 AHardwareBuffer_allocate(BLOB 4194304 bytes) = 0
test2 import AHB into Vulkan: 0 (allocationSize 4194304)
test2 compute into AHardwareBuffer, CPU lock read: 1048576 elements, 0 wrong, 0.77 ms
load: 135 dispatches of 1048576 x 2000 iterations in 8.0 s, result 0 wrong
vkprobe done
```

GPU clock while it ran (`/sys/class/devfreq/3d00000.gpu`; msm has no `/sys/class/kgsl`):

```
before: cur=305000000
t+1 .. t+7: cur=670000000
t+8 (probe done): cur=305000000
trans_stat time at 670 MHz: 11684 ms before -> 19780 ms after (+8096 ms, the 8 s load)
available: 305000000 400000000 441600000 490000000 525000000 587000000 670000000, governor simple_ondemand
```

No kernel GPU fault, hang or recovery after the run. Not covered: graphics pipelines and a
Vulkan swapchain on an Android surface (no app on the image uses Vulkan; HWUI stays on
`skiagl` by product choice).

## 6. Cần bạn tự kiểm tra

Những mục dưới đây mình chưa chứng minh được qua adb (hoặc cần thao tác thay đổi trạng thái
máy mà phiên chia sẻ tablet không được phép làm):

1. **Hình trên màn hình thật khớp với screencap.** Panel hiện chỉ hiện khoảng 160 dòng của
   DSI0 (lỗi DSI/C-PHY của kernel). Khi các phiên panel sửa xong, hãy so ảnh panel với
   `adb -s R52R504WFTK exec-out screencap -p > s.png`: cùng cửa sổ, đúng màu (không đảo
   đỏ/xanh), không bị xoay, không xé hình khi kéo cửa sổ.
2. **Vì sao drm_hwcomposer không đặt được layer nào lên plane DPU** (2290/2397 test commit
   thất bại). Cần mount debugfs (thay đổi hệ thống, nên mình không làm):
   `adb shell mount -t debugfs debugfs /sys/kernel/debug`, rồi
   `adb shell cat /sys/kernel/debug/dri/1/state` và `.../dri/1/debug/...` để xem plane,
   rotation, format/modifier mà DPU nhận; thử thêm một lần với màn hình dọc
   (`adb shell settings put system user_rotation 0`; lệnh này thay đổi cài đặt, hãy trả lại
   sau) xem các layer có chuyển sang `DEVICE` không:
   `adb shell dumpsys SurfaceFlinger | grep -A3 "HWC layers"`.
3. **Lỗi log của composer** `Encountered multiple cursor planes for CRTC 95. Ignoring plane 89`
   (in ra mỗi khung hình). Xem trên tablet bằng
   `adb shell logcat -d | grep -c "multiple cursor"`; cần quyết định có vá drm_hwcomposer hoặc
   chỉ để một cursor plane cho CRTC này.
4. **Vulkan với một ứng dụng thật** (swapchain lên màn hình). Chưa có app Vulkan trên image.
   Cách thử: cài một app Vulkan demo (ví dụ một APK mẫu Vulkan của NDK) rồi xem
   `adb shell dumpsys gpu | grep -E "createdVulkanSwapchain|vkLoadingFailureCount"`; hoặc thử
   HWUI trên Vulkan cho app mới mở: `adb shell setprop debug.hwui.renderer skiavk`, mở lại
   Settings, xem `adb shell dumpsys gfxinfo com.android.settings | grep Pipeline` phải là
   `Skia (Vulkan)`, nhìn hình trên panel, rồi trả về `skiagl`.
5. **Độ mượt khi dùng thật.** `HWC missed frame count: 15`, `GPU missed frame count: 60` và 4.6%
   frame giật trong SystemUI ngay sau khởi động. Hãy dùng vài phút (kéo cửa sổ, mở app) rồi
   chạy `adb shell dumpsys gfxinfo com.android.systemui | head -20` và
   `adb shell dumpsys SurfaceFlinger | grep -i "missed"` để xem có tăng nhanh không.
6. **Màn hình tắt/bật và vsync sau khi resume.** Mình không tắt màn hình (phiên chia sẻ). Hãy
   bấm nút nguồn tắt rồi bật, sau đó kiểm tra `adb shell dumpsys SurfaceFlinger | grep -A12 "drm_hwcomposer"`
   ("Failed to commit frames" vẫn là 0) và `adb shell logcat -d | grep -i -E "drmhwc|hwc3" | grep -v cursor`.
7. **Wide color / 10-bit**: HWUI không tìm được EGL config RGBA1010102. Chỉ là cảnh báo nếu bạn
   không cần màu rộng; nếu cần, cần kiểm tra lại cấu hình EGL của Mesa.
