# gts7l: sensors and input, checked on the tablet (2026-11-25)

Build `Android/aosp_gts7l/gts7l:17/CP2A.260605.016/eng.caoanh.20261124.1208:userdebug/test-keys`,
`adb -s $SERIAL`, SELinux **Permissive**. The tablet had just rebooted (uptime 0-3 min
during the checks) and stood still in landscape; Book Cover not attached. Someone was tapping
the screen while the checks ran, which gave real touchscreen events.

Read-only checks only (`dumpsys`, `getevent`, `/proc`, `/sys`, and short
`gts7l-sensord --list` / `--stream --count 5` runs that enable a DSP sensor for a second and
disable it again).

The sensors HAL source is not on this branch: it is in commits 3708ae1 / 9254437 / d22ae79
(`aosp-device/gts7l/sensors/`, docs/gts7l.md §19), on `claude/blissful-chebyshev-d3a5ba`.
The image on the tablet has it.

## Summary

| Item | Result | Evidence |
|---|---|---|
| `ISensors` HAL (`vendor.sensors-gts7l`, `vendor.gts7l-sensord`) | **Works** | services running, `ISensors/default` registered, events flow |
| Accelerometer (LSM6DSO) | **Works** | live samples, \|g\| = 9.84 m/s², 25 Hz |
| Auto-rotation | **Works** (static) | `WindowOrientationListener` subscribed, `mProposedRotation=1` matches gravity on +x; turning the tablet needs a human |
| Gyroscope, magnetometer, ambient light, gravity, rotation vectors | **Partial** | the DSP lists and streams them, but the HAL exposes only the accelerometer; Android has no light sensor, so no auto-brightness |
| Proximity, pressure, hall (DSP) | **Absent** | the DSP lists none of them |
| Touchscreen (Novatek NT36523) | **Works** | real taps in `getevent` and in InputDispatcher's RecentQueue |
| S Pen (Wacom W90xx) | **Partial** | device probed and configured as STYLUS; no events without a pen (IRQ count 0) |
| Power key (`pm8941_pwrkey`) | **Partial** | device present, IRQ 0 (needs a press) |
| Volume down (`pm8941_resin`) / volume up (`gpio-keys`) | **Partial** | devices present, IRQ 0 (needs a press) |
| Book Cover keyboard + touchpad (STM32 pogo) | **Partial** | devices registered, cover not attached (`pogo-conn` IRQ 0, `SW_TABLET_MODE=1`) |
| Hall sensor | **Partial / by design** | the flip-cover hall (TLMM 157) is not wired up in the gts7l DT; the lid comes from the Book Cover over pogo (`SW_LID` on the Tablet Mode Switch device) |

## Sensors

### HAL processes and service

```
$ adb shell 'ps -A | grep sens; service list | grep -i sensor'
system  616   1  ... gts7l-sensord                          (--serve, file service for the DSP)
system  645   1  ... android.hardware.sensors-service.gts7l
system 1067 645  ... gts7l-sensord                          (--stream accel, child of the HAL)
40   android.hardware.sensors.ISensors/default: [android.hardware.sensors.ISensors]
219  sensorservice: [android.gui.SensorServer]
$ adb shell getprop | grep svc.vendor
[init.svc.vendor.gts7l-sensord]: [running]
[init.svc.vendor.sensors-gts7l]: [running]
```

### `dumpsys sensorservice`: one real sensor, live events

```
Total 1 h/w sensors, 1 running 0 disabled clients:
0x00000001) active-count = 2; sampling_period(ms) = {38.5, 38.5}, selected = 38.46 ms; ...
Sensor List:
0x00000001) LSM6DSO Accelerometer | STMicroelectronics | ver: 1 | type: android.sensor.accelerometer(1) | perm: n/a | flags: 0x00000000
	continuous | minRate=26.00Hz | maxRate=208.03Hz | no batching | non-wakeUp |
Recent Sensor events:
LSM6DSO Accelerometer: last 50 events
	 1 (ts=32.031573268, wall=05:09:56.814) 8.75, -0.04, 4.53,
	 2 (ts=32.071642383, wall=05:09:56.855) 8.74, -0.06, 4.53,
	...
	47 (ts=33.875108528, wall=05:09:58.658) 8.72, -0.04, 4.53,
Active sensors:
LSM6DSO Accelerometer (handle=0x00000001, connections=2)
	 com.android.server.wm.WindowOrientationListener$AccelSensorJudge ...
	 com.android.server.power.FaceDownDetector ...
```

A capture 90 s later shows fresh timestamps (ts=126.01 ... 127.98, 8.72 -0.05 4.54), so the
stream keeps running. Magnitude sqrt(8.74² + 0.04² + 4.53²) = 9.84 m/s² (gravity), samples
40 ms apart (the 26 Hz the framework asked for), noise ±0.02. The tablet stood still, so the
values did not change; that they follow the tablet when it turns needs a human (see below;
docs/gts7l.md §19 records it was seen once on 2026-11-25).

### Auto-rotation

```
$ adb shell settings get system accelerometer_rotation
1
$ adb shell dumpsys window | grep -E 'mRotation=|mProposedRotation|mUserRotationMode|WindowOrientationListener|mEnabled'
    mRotation=1 mDeferredRotationPauseCount=0
    WindowOrientationListener
      mEnabled=true
        mProposedRotation=1
    mUserRotationMode=USER_ROTATION_FREE mUserRotation=ROTATION_90
```

Auto-rotate is on, the orientation listener is subscribed to the accelerometer and proposes
ROTATION_90, which is what gravity on +x means (tablet in landscape, turned left). Rotation
following a turn needs a human.

### Other DSP sensors: present on the DSP, not in the HAL

```
$ adb shell /vendor/bin/gts7l-sensord --list --wait 10
SSC service at node 9 port 12
accel: lsm6dso   gyro: lsm6dso   mag: ak0991x   ambient_light: veml3235
gravity, gyro_cal, mag_cal, game_rv (Game Rotation Vector), rotv (Rotation Vector), amd, tilt
proximity: none   pressure: none   rgb: none   humidity: none
ambient_temperature: none   hall: none   device_orient: none   sig_motion: none   pedometer: none
12 sensors found

$ adb shell /vendor/bin/gts7l-sensord --stream gyro --rate 10 --count 5
gyro t=74.582616 -0.0073 0.0018 0.0024 accuracy=0
gyro t=74.622656 -0.0073 0.0018 0.0018 accuracy=0
$ adb shell /vendor/bin/gts7l-sensord --stream mag --rate 10 --count 5
mag t=74.741487 -37.9479 -82.7017 -66.9210 accuracy=3
mag t=74.841487 -36.9784 -81.9200 -67.5176 accuracy=3
$ adb shell /vendor/bin/gts7l-sensord --stream ambient_light --rate 10 --count 5
ambient_light t=75.815028 5.0000 16.0000 82.0000 0.0000 ... accuracy=3
ambient_light t=76.165029 5.0000 17.0000 83.0000 0.0000 ... accuracy=3
```

Gyro reads ~0 rad/s at rest, the magnetometer ~110 µT total with accuracy 3, the light sensor
reports a value (first field 4-5, the meaning of the other fields not checked). None of them
reach Android: `getSensorsList()` returns only the accelerometer
(`aosp-device/gts7l/sensors/hal/src/sensors.rs`), so

```
$ adb shell dumpsys display | grep -E 'mLightSensor|mUseAutoBrightness'
  mLightSensor=null
  mUseAutoBrightness=false
```

No `/sys/bus/iio/devices` exist on this kernel either (the gts7xl IIO bridges, patches 0047
and 0072, are not used on Android).

### SELinux

The HAL depends on Permissive mode. Denials seen (auditd deduplicates, so one of each):

```
{ use }        hal_gts7l_sensors -> system_server fd           (event FMQ)
{ read write } hal_gts7l_sensors -> system_server_tmpfs file
{ map }        hal_gts7l_sensors -> system_server_tmpfs file
{ use }        gts7l_sensord     -> system_server fd           (FMQ fd inherited by the child)
{ read write } gts7l_sensord     -> system_server_tmpfs file
{ getattr }    gts7l_sensord     -> hal_gts7l_sensors fifo_file
{ search }     gts7l_sensors_setup -> mnt_vendor_file dir
```

In Enforcing mode the HAL could not map the framework's event queue.

## Input

```
$ adb shell getevent -lp       (trimmed)
/dev/input/event0  regulator-haptic                       FF_RUMBLE FF_PERIODIC ...
/dev/input/event1  pm8941_pwrkey                          KEY_POWER
/dev/input/event2  pm8941_resin                           KEY_VOLUMEDOWN
/dev/input/event3  Wacom W90xx Pen                        BTN_TOOL_RUBBER BTN_TOUCH BTN_STYLUS,
                   ABS_X 0-26750 (res 97), ABS_Y 0-16700 (res 100), ABS_PRESSURE 0-4095,
                   ABS_DISTANCE 0-255, ABS_TILT_X/Y -63..63; INPUT_PROP_DIRECT
/dev/input/event4  nt36523-ts                             BTN_TOUCH, ABS_MT_* 10 slots,
                   X 0-1599, Y 0-2559; INPUT_PROP_DIRECT
/dev/input/event5  Samsung Book Cover Keyboard            full keyboard, media/brightness keys, KEY_FN
/dev/input/event6  Samsung Book Cover Touchpad            BTN_MOUSE, 3 slots, X 0-2559, Y 0-1599;
                   INPUT_PROP_POINTER INPUT_PROP_BUTTONPAD
/dev/input/event7  Samsung Book Cover Tablet Mode Switch  SW_LID SW_TABLET_MODE*   (* = on)
/dev/input/event8  gpio-keys                              KEY_VOLUMEUP
```

`dumpsys input` maps them: `nt36523-ts` → TOUCHSCREEN; `Wacom W90xx Pen` → KEYBOARD |
TOUCHSCREEN | STYLUS; `pm8941_pwrkey`, `pm8941_resin`, `gpio-keys` → KEYBOARD (Generic.kl);
the three Book Cover devices share one sysfs parent (`i2c-9/9-002a`) and become one
InputDevice: KEYBOARD | MOUSE | TOUCHPAD with a switch mapper, KeyboardType 2 (alphabetic).
No device has an `.idc` file.

Interrupt counts about 2 minutes after boot:

```
$ adb shell grep -E 'nt36|w90|pogo|pwrkey|resin|Volume' /proc/interrupts   (CPU0 column)
128:     0  pmic_arb 524301 Edge  pm8941_pwrkey
129:     0  pmic_arb 532493 Edge  pm8941_resin
155:     0  msmgpio  136 Level    w90xx
163:  1205  msmgpio   15 Edge     nt36523-ts
167:     0  msmgpio   31 Level    pogo-keyboard
168:     0  msmgpio   43 Edge     pogo-conn
186:     0  spmi-gpio  2 Edge     Volume Up
```

### Touchscreen (Novatek NT36523): Works

```
$ adb shell timeout 20 getevent -lt /dev/input/event4      (trimmed)
[136.596871] EV_ABS ABS_MT_TRACKING_ID 00000021
[136.596871] EV_KEY BTN_TOUCH          DOWN
[136.686881] EV_ABS ABS_MT_TRACKING_ID ffffffff
[136.686881] EV_KEY BTN_TOUCH          UP
[153.172103] EV_ABS ABS_MT_TRACKING_ID 00000022
...
$ adb shell dumpsys input | grep -A6 RecentQueue           (trimmed)
  MotionEvent(deviceId=8, source=TOUCHSCREEN, action=MOVE, pointers=[0: (397.0, 2265.0)])
  MotionEvent(deviceId=8, source=TOUCHSCREEN, action=UP,   pointers=[0: (404.0, 2269.0)])
  MotionEvent(deviceId=8, source=TOUCHSCREEN, action=DOWN, pointers=[0: (225.0, 2473.0)])
  MotionEvent(deviceId=8, source=TOUCHSCREEN, action=UP,   pointers=[0: (225.0, 2473.0)])
  FocusEvent(hasFocus=false) / FocusEvent(hasFocus=true)
```

Taps of ~90 ms with clean DOWN/UP pairs, no events between them (no ghost touches in the 20 s
window); they reach InputDispatcher and changed window focus. That the position matches the
finger, and multi-touch, need a human.

### S Pen (Wacom W90xx): Partial

Probed on `i2c-17/17-0056` with a `ble_charge` attribute; the input device and STYLUS source
are there, the IRQ count is 0 because no pen came near the screen.

### Power and volume keys: Partial

All three devices are present and mapped through `Generic.kl`; nothing presses them without a
human.

### Book Cover keyboard and touchpad: Partial

The pogo driver registered the keyboard, touchpad and switch devices, but the cover is not
attached: `pogo-conn` has 0 interrupts and the switch reports `SW_TABLET_MODE` on (the driver
sets it while the connect line is low). `dumpsys window`: `mLidState=LID_OPEN`.

### Hall sensor: Partial (by design)

The gts7l DT (patch 0094) has no lid switch on the flip-cover hall GPIO (TLMM 157): it never
toggles for the Book Cover Keyboard, which reports its own cover over pogo as `SW_LID` (patch
0062). `gpio-keys` holds only `KEY_VOLUMEUP`. So a plain flip cover (without keyboard) does not
turn the screen off; only the Book Cover can, which needs the cover attached to test.

## Cần bạn tự kiểm tra

Chạy các lệnh trên máy tính (tablet cắm USB). Tablet đang ở chế độ Permissive.

1. **Gia tốc kế + tự xoay màn hình**
   - Chạy `adb -s $SERIAL shell dumpsys sensorservice | grep -A5 "Recent Sensor events"`
     ba lần: tablet đứng dọc (portrait), nằm ngang (landscape), nằm phẳng trên bàn.
   - Phải thấy: dọc → y ≈ +9.8; ngang → x ≈ ±9.8; nằm phẳng → z ≈ +9.8 (màn hình hướng lên).
   - Bật "Tự động xoay", xoay tablet 90°: giao diện phải xoay theo trong ~1 giây. Kiểm tra
     `adb -s $SERIAL shell dumpsys window | grep mProposedRotation` đổi giá trị (0/1/2/3).
2. **Cảm ứng (Novatek)**
   - Chạy `adb -s $SERIAL shell getevent -lt /dev/input/event4`, chạm 1 ngón ở góc trên
     trái rồi góc dưới phải (theo chiều dọc): phải thấy `BTN_TOUCH DOWN/UP`, `ABS_MT_POSITION_X/Y`
     gần 0/0 và gần 1599/2559.
   - Chạm bằng 5 ngón cùng lúc: phải thấy 5 `ABS_MT_TRACKING_ID` khác nhau (slot 0-4).
   - Bật Tuỳ chọn nhà phát triển → "Vị trí con trỏ": vạch phải nằm đúng dưới ngón tay ở cả 4
     hướng xoay.
3. **S Pen (Wacom)**
   - Chạy `adb -s $SERIAL shell getevent -lt /dev/input/event3`, đưa bút lại gần màn hình
     (chưa chạm): phải thấy `BTN_TOOL_PEN` và `ABS_DISTANCE`. Chạm và ấn mạnh dần: `BTN_TOUCH DOWN`
     và `ABS_PRESSURE` tăng tới ~4095. Nhấn nút bên bút: `BTN_STYLUS`. Lật đầu tẩy: `BTN_TOOL_RUBBER`.
   - Vẽ trong một ứng dụng ghi chú: nét phải nằm đúng dưới đầu bút.
4. **Phím nguồn và âm lượng**
   - Chạy `adb -s $SERIAL shell getevent -l`, nhấn lần lượt: nguồn → `KEY_POWER` trên
     event1; giảm âm → `KEY_VOLUMEDOWN` trên event2; tăng âm → `KEY_VOLUMEUP` trên event8.
   - Nhấn tăng/giảm âm lượng: thanh âm lượng phải hiện ra. Nhấn nguồn: màn hình tắt/bật.
5. **Bàn phím Book Cover + touchpad**
   - Gắn bàn phím vào cổng pogo. Chạy
     `adb -s $SERIAL shell getevent -l /dev/input/event7`: phải thấy `SW_TABLET_MODE 0`
     (sau ~2 giây).
   - Kiểm tra `adb -s $SERIAL shell grep pogo /proc/interrupts`: số ngắt `pogo-conn` và
     `pogo-keyboard` phải tăng.
   - Gõ chữ vào một ô nhập liệu: chữ phải hiện đúng. Thử phím Fn + phím độ sáng/âm lượng.
   - Kéo một ngón trên touchpad: con trỏ chuột phải di chuyển đúng hướng; chạm 2 ngón để cuộn,
     nhấn xuống để click.
6. **Cảm biến hall / nắp bàn phím**
   - Với Book Cover đã gắn, gập nắp lên màn hình trong khi chạy
     `adb -s $SERIAL shell getevent -l /dev/input/event7`: phải thấy `SW_LID 1` và màn hình
     tắt; mở nắp → `SW_LID 0`, màn hình bật.
   - Bao da thường (không có bàn phím) sẽ **không** tắt màn hình: hall GPIO 157 không được khai
     báo trong DT gts7l (cố ý).
