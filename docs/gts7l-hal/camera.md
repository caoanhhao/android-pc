# gts7l cameras: CAMSS, three Samsung sensors, Rust camera HAL with a software ISP

State on 2026-11-25, branch `claude/gts7l-camera-0ba159`. Kernel and capture proven on the
tablet (SM-T875) with the modules loaded by hand on the running build; the HAL builds in
Soong (clippy clean), finds the three cameras on the tablet and passes its tests, but has
**not** run under cameraserver yet (no partial flashes: it goes on the tablet with the next
full image, see "Not tested yet").

## Hardware

From the stock device tree (`gts7l-stock/ap/dtbo/04_id0_rev0.dts`, fragments 150/151) and
the probe on the tablet. Wiring is the same as on the Tab S7+ (gts7xlwifi), so the DT nodes
of `sm8250-samsung-gts7l.dts` (patch 0094, from the S7+ series 0033-0036) were right; the
GPIOs and CSI PHYs below were checked against the stock DT.

| Camera | Sensor | CCI bus, addr | CSIPHY, lanes | MCLK | Reset | VANA | Mode(s) |
|---|---|---|---|---|---|---|---|
| Rear 13 MP (id 0) | S5K3M5 (`s5k3m5`, mainline + 0033) | CCI0 bus 0, 0x2d | 0, 4 lanes (reversed) | gpio94 MCLK0 | gpio73 | gpio39 | 4128x3096 GRBG 34 fps, 1008x756 |
| Front 8 MP (id 1) | S5K4HA (`s5k-tab`, 0035) | CCI0 bus 1, 0x2d | 3, 4 lanes | gpio96 MCLK2 | gpio30 | gpio38 | 3264x2448 GBRG, 800x600 |
| Rear ultrawide 5 MP (id 2) | S5K5E9 (`s5k-tab`) | CCI1 bus 0, 0x10 | 2, 2 lanes | gpio95 MCLK1 | gpio78 | gpio42 | 2576x1932 **GRBG** (0230/0231), 1296x972 |
| Rear focus | DW9808 (`dw9807-vcm`, 0034) | CCI1 bus 1, 0x0c | | | | VAF = gpio39 | 0..1023 |

VIO (1.8 V) for all three is gpio37, always on (0036). The stock DT also lists a second front
module (`cam-sensor@12`, same wiring) and EEPROMs at 0xb0/0xa2/0xa0 on CCI1 bus 1; the
EEPROMs hold calibration (lens shading, AF) we do not read yet.

Orientation and rotation come from the DT (`orientation`, `rotation`) through the sensors'
V4L2 controls: rear and ultrawide back/90, front front/270, as the stock
`sensor-position-roll` values say.

## Kernel

Everything needed was already in the series; what was missing is that the modules never
loaded (the board's `modules.load` only listed DSP and Bluetooth modules). Changes:

- `kernel/modules.bp`: load `camcc-sm8250`, `i2c-qcom-cci`, `qcom-camss`, `s5k3m5`,
  `s5k-tab`, `dw9807-vcm` at boot (modprobe pulls in the V4L2 cores).
- **0230** `media: s5k-tab: take the Bayer order from the board`: optional
  `samsung,bayer-order` DT property. **0231** sets `"grbg"` on the gts7l ultrawide: the
  S7+ module reads out GBRG, this one GRBG (red objects came out blue; checked on a red
  chair against the rear camera and with both orders decoded from the same raw frame).
- **0232** `media: qcom: camss: stop the started subdevs when a start fails`: a failed
  `s_stream(1)` left the subdevs before it marked as streaming, so the next start skipped
  them (WARN in `call_s_stream`) and timed out; every second stream failed.
- `ueventd.gts7l.rc`: `/dev/video*`, `/dev/media*`, `/dev/v4l-subdev*` 0660 system:camera.
  HAL sepolicy labels `/dev/media*` and `/dev/v4l-subdev*` `video_device`.

The 0230-0232 kernel was built in a separate tree (`gts7l-kernel-camera`, out
`out/gts7l-camera`) but not flashed; the tests below ran on the tablet's kernel #35, which
has everything except 0230-0232 (so the ultrawide still needs the HAL to see GBRG there).

### Proof: media graph

`pccamera-raw graph` (our own `media-ctl -p`, below) on the tablet after loading the modules;
full output in [camera-graph.txt](camera-graph.txt). CAMSS has 6 CSIPHYs, 4 CSIDs (+lite),
4 VFEs with RDI paths; the three sensors and the lens are bound:

```
- entity 329: s5k3m5 21-002d (function 0x20001) /dev/v4l-subdev24
	pad0: Source
		-> "msm_csiphy0":0 [ENABLED,IMMUTABLE]
- entity 331: s5k-tab 22-002d (function 0x20001) /dev/v4l-subdev25
		-> "msm_csiphy3":0 [ENABLED,IMMUTABLE]
- entity 333: s5k-tab 23-0010 (function 0x20001) /dev/v4l-subdev26
		-> "msm_csiphy2":0 [ENABLED,IMMUTABLE]
- entity 335: dw9807 24-000c (function 0x20003) /dev/v4l-subdev27

sensor s5k3m5 21-002d (/dev/v4l-subdev24)
  active 0x300a 4128x3096
  exposure = 256 ControlRange { min: 8, max: 3318, step: 1, default: 256 }
  analogue_gain = 1 ControlRange { min: 1, max: 16, step: 1, default: 1 }
  orientation = 1, rotation = 90, pixel_rate = 552960000
  route: s5k3m5 21-002d -> msm_csiphy0 -> msm_csid0 -> msm_vfe0_rdi0 -> msm_vfe0_video0
```

### Proof: raw frames

```
$ pccamera-raw capture s5k3m5 rear.raw --frames 6 --exposure 2000 --gain 4
route: s5k3m5 21-002d -> msm_csiphy0 -> msm_csid0 -> msm_vfe0_rdi0 -> msm_vfe0_video0
s5k3m5 21-002d 4128x3096 Grbg stride 5168 bytes 16000128
$ pccamera-raw capture 23-0010 wide.raw --size 2576x1932 --frames 6 --exposure 2000 --gain 512
s5k-tab 23-0010 2576x1932 Gbrg stride 3232 bytes 6244224
```

Unpacked on the host (MIPI RAW10, black level 64, 2x2 binning, grey world): the rear frame
shows the room (chair, scooter, string lights), the ultrawide the same scene wider. Switching
between all three sensors and sizes, 9 streams in a row, all delivered frames after the
fixes. Focus: raw frames at lens positions 0/400/1023 show the string lights as blobs, sharp
points, blobs: the DW9808 moves (only while its node is open: it powers down on close).

The photos show the user's room and face, so they stay out of git.

## Camera HAL: our Rust HAL, extended

Choice: **extend `aosp-device/hals/camera` (Rust)** rather than libcamera's Android HAL.

- Project rules: own all source, Rust HALs on libpchal. libcamera's HAL is C++ with its own
  meson build, libyuv/libexif dependencies and a large IPA/pipeline-handler layer to port
  into Soong; its software ISP's GPU path needs EGL setup in the HAL process.
- Our HAL already had the whole AIDL provider/device/session machinery (request queue,
  gralloc buffers, JPEG blobs, metadata validated against `libcamera_metadata`) for UVC
  webcams. CAMSS needs a different frame source, not a different HAL.
- The raw pipeline is small: media-controller setup, RAW10 unpack and a 2x2 debayer, grey
  world AWB, exposure through the sensor's V4L2 controls, contrast AF.

What was added (all in `libpccamera`, host-tested, except the binder glue):

| File | What |
|---|---|
| `src/media.rs` | Media controller and sub-devices: topology, link setup, pad formats, controls, routes from a sensor to an RDI capture node, the lens of a sensor (ancillary link) |
| `src/v4l2.rs` | Multi-planar capture (CAMSS nodes are MPLANE only), stride, no `S_PARM` on CAMSS |
| `src/isp.rs` | Software ISP: RAW10 unpack, black level, white balance and digital gain, highlight roll-off, sRGB gamma, BT.601 YCbCr 4:2:0 with a little saturation, 4 threads; statistics (mid-tone means, green histogram, focus measure); auto exposure (time first, then analogue, then digital gain) and grey-world AWB |
| `src/raw.rs` | Finds raw sensors on `/dev/media*`, their modes and frame rates, sets up a capture, runs ISP + AE + AWB + contrast AF (coarse then fine sweep) per frame, keeps 3A state across mode changes |
| `src/sensor.rs` | Raw cameras: output = half the capture size, facing and orientation from the sensor |
| `src/camera.rs`, `provider.rs`, `device.rs`, `session.rs` | Raw cameras are ids `0` (main back), `1` (front), `2` (ultrawide); they exclude each other (one CAMSS graph: resource cost conflicts and one session at a time); the session sets up the pipeline before streaming and reports AF scan/lock |
| `src/bin/pccamera_raw.rs` | Bring-up tool `pccamera-raw` (`m pccamera-raw`, not in the product): `graph`, `capture` (raw), `still` (the HAL's pipeline, I420 out) |

Hardware level stays `EXTERNAL` (3A runs inside the HAL, settings are echoed back), with
`LENS_FACING` BACK/FRONT and the sensor orientation. Outputs per camera: YUV/JPEG at
2064x1548 and 504x378 (rear), 1632x1224 and 400x300 (front), 1288x966 and 648x486
(ultrawide), plus any smaller size cropped and scaled from those.

### Proof: the HAL pipeline on the tablet

`pccamera-raw still` runs `RawSensor::start` + `RawPipeline::process` exactly as the
session does, on the real sensors (room lit by string lights only, so AE ends at its limits):

```
$ pccamera-raw still s5k3m5 rear.yuv --frames 90
frame 0: 1900 lines gain 1 digital 1.00 median 0.000 ... sharp 0.8977 (focusing)
frame 60: 3318 lines gain 16 digital 4.00 median 0.010 ... sharp 0.1706 (focusing)
frame 89: 3318 lines gain 16 digital 4.00 median 0.010 ... sharp 0.1677
s5k3m5 21-002d I420 2064x1548 converged false (90 frames in 4.33s, ISP 41 ms per frame)
$ pccamera-raw still 22-002d front.yuv --frames 60
s5k-tab 22-002d I420 1632x1224 converged false (60 frames in 2.17s, ISP 31 ms per frame)
$ pccamera-raw still 23-0010 wide.yuv --frames 60
s5k-tab 23-0010 I420 1288x966 converged false (60 frames in 2.06s, ISP 25 ms per frame)
```

Converted on the host: all three show the scene with natural colours (front: a face with
natural skin tones; rear: in focus after the sweep, the red chair red), except the ultrawide
red/blue swap that 0230/0231 fix. "converged false" is AE at its limit in the dark room.

ISP cost is 25-41 ms per frame at the largest outputs on four A77/A55 threads, so preview
runs at about 20-25 fps there.

### Proof: the HAL binary on the tablet

Built with `STEPS="android.hardware.camera.provider-service.pc+pccamera-raw+libpccamera_test+android.hardware.camera.provider-service.pc_test" tools/aosp17-build.sh`.
`libpccamera_test` (host): 36 passed. The device test (metadata through
`validate_camera_metadata_structure`) and the HAL itself, run from `/data/local/tmp` with
its `libjpeg.so` (the running image has neither in `/vendor`):

```
$ ./android.hardware.camera.provider-service.pc_test
test result: ok. 4 passed; 0 failed
$ ./android.hardware.camera.provider-service.pc     (logcat)
I main::camera: s5k3m5 21-002d (/dev/video0): Back, rotation 90, outputs up to 2064x1548
I main::camera: s5k-tab 22-002d (/dev/video0): Front, rotation 270, outputs up to 1632x1224
I main::camera: s5k-tab 23-0010 (/dev/video0): Back, rotation 90, outputs up to 1288x966
I main::provider: device@1.1/pc/0: r#PRESENT
I main::provider: device@1.1/pc/1: r#PRESENT
I main::provider: device@1.1/pc/2: r#PRESENT
E main: cannot register android.hardware.camera.provider.ICameraProvider/pc/0: UNKNOWN_ERROR
I servicemanager: ... Could not find android.hardware.camera.provider.ICameraProvider/pc/0 in the VINTF manifest.
```

The registration fails only because the running image's vendor manifest does not declare
the instance yet; the full image has the HAL's VINTF fragment.

## On the full image

2026-11-26: on the full image the user took photos with the Camera app (user report).
Not yet checked one by one:

- `dumpsys media.camera` lists 3 cameras (`device@1.1/pc/0..2`), facing back/front/back.
- A still through the framework (`am start -a android.media.action.IMAGE_CAPTURE` or the
  Camera2 test) gives a JPEG of the scene.
- The modules load at boot from `modules.load`, and `/dev/media0` etc. are system:camera.
- The ultrawide with 0230/0231 reports GRBG and shows red as red.

## Next steps

- Lens shading correction from the EEPROMs (corners are darker) and a colour matrix per
  sensor (colours are grey-world only).
- Full-resolution demosaic for stills (now half size: 2064x1548 for 13 MP).
- Longer exposures in the dark: lengthen VBLANK when the FPS range allows (now capped at the
  mode's frame time, ~1/30 s).
- AF: continuous mode (re-sweep on scene change), touch regions; AF on the binned preview.
- GPU ISP (the CPU ISP limits preview to ~20-25 fps at the largest size).
- `LIMITED` hardware level: manual sensor controls, exposure/sensitivity in results.
- The second front module (`cam-sensor@12`) has no driver; identify it if a tablet has it.

## Cần bạn tự kiểm tra

After the main session flashes the full image:

1. Open the Camera app: the preview of the rear camera should be live and upright, and turn
   sharp within about two seconds (focus sweep). Switch to the front camera: upright, mirrored
   like a mirror.
2. Take a photo with each camera and check it in the gallery (colours, no red/blue swap on the
   ultrawide if the app offers it).
3. If the preview stays black: `adb logcat -s android.hardware.camera.provider-service.pc`
   and `adb shell ls -l /dev/media0 /dev/video0`.
