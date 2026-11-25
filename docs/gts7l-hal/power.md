# gts7l: power, health, thermal and light HALs on the tablet

Checked on 2026-11-25 on the SM-T875 (`adb -s R52R504WFTK`, root, SELinux permissive),
build `aosp_gts7l/gts7l:17/CP2A.260605.016/eng.caoanh.20261124.1208:userdebug`, a few
minutes after a boot. USB stayed plugged in the whole time (adb), so nothing here saw
the tablet on battery.

Services running:

```
system  636  android.hardware.health-service.pc
system  638  android.hardware.light-service.pc
system  643  android.hardware.power-service.pc
system  646  android.hardware.thermal-service.pc
system  648  android.hardware.power.stats-service.example   (vendor.power.stats-default)
```

The HALs' own startup lines (`found N lights`, `no intel_pstate ...`) were no longer in
logcat: the 256 KiB main buffer had already rotated past them.

## Summary

| HAL | Result | Why |
|---|---|---|
| IHealth (`health-pc`) | **Partial** | Reports the MAX77705 charger and USB-C correctly, but there is no battery: the MAX77705 fuel gauge (`max17042` driver) never probes, so level/voltage/temperature are absent (Android shows 100 %, 0 mV, 0 °C). |
| IPower (`power-pc`) | **Partial** | Mode hints and INTERACTION boosts reach the HAL (binder transactions from `system_server`), but they change nothing: the HAL only drives `intel_pstate`, which an ARM SoC does not have. cpufreq stays on schedutil with the same limits. |
| IPowerStats (`power.stats-default`) | **Fails** | It is AOSP's example service with fake entities (Rail1/2/3, CPU Idle/Active, GPU/MODEM). No real SM8250 data. |
| IThermal (`thermal-pc`) | **Partial** | All 25 Qualcomm thermal zones with live temperatures that match `/sys/class/thermal`, 4 cooling devices, trip-point thresholds. But every sensor has type UNKNOWN (-1): no CPU/GPU/SKIN/BATTERY type, so Android's thermal status and throttling never react. |
| ILights (`light-pc`) | **Works** (as a HAL) | Exposes one BACKLIGHT light (the DSI backlight); `setLightState` writes the scaled value to sysfs. Settings brightness does not go through it: HWC3 (drm_hwcomposer) writes the backlight itself, which also works. No notification LED or keyboard light exists in sysfs. |

## IHealth: Partial

`dumpsys battery` (framework view of IHealth):

```
  AC powered: false
  USB powered: true
  Max charging current: 3000000
  Charge counter: 0
  status: 1            (UNKNOWN)
  health: 1            (UNKNOWN)
  present: false
  level: 100
  voltage: 0
  temperature: 0
  technology:
```

`/sys/class/power_supply` has only the charger and the Type-C port, no battery:

```
max77705-charger: type=USB online=1 present=1 status=Full health=Good
                  CONSTANT_CHARGE_CURRENT=900000 CONSTANT_CHARGE_VOLTAGE=4350000
                  INPUT_CURRENT_LIMIT=900000 VOLTAGE_MAX_DESIGN=4350000
max77705-usbc:    type=USB online=1 voltage_now=5000000 current_max=3000000 usb_type=[C] PD
```

So the HAL reports what sysfs has: USB online (from both supplies) and 3 A max current
(from `max77705-usbc/current_max`). With no `type=Battery` supply it falls back to the
"no battery" case (`present: false`); level 100 is the framework's default for that.

Why there is no battery (kernel, not the HAL):

```
$ dmesg | grep max17042
max17042 max77705-battery.1.auto: error -ENXIO: IRQ index 0 not found
platform max77705-battery.1.auto: deferred probe pending: max17042: temp channel
$ ls /sys/bus/iio/devices
(empty)
```

The fuel gauge (i2c 0-0036, `max77705-battery`) waits for an IIO temperature channel
that never appears (no battery thermistor ADC). docs/gts7l.md already lists this
("fuel gauge deferred"). Until the kernel registers the battery supply, Android cannot
show level, voltage or temperature.

Not tested: the charging state change on unplug. Unplugging USB drops adb, and the
tablet was shared with other sessions.

## IPower: Partial

`dumpsys power` shows the framework talks to the HAL:

```
  mWakefulness=Awake
  mIsPowered=true
  mHalAutoSuspendModeEnabled=false
  mHalInteractiveModeEnabled=true
```

IPower calls are oneway, so the proof is the binder transaction log
(`/dev/binderfs/binder_logs/transaction_log`), filtered on the HAL's pid 643 (its binder
node 311 is held by surfaceflinger 665, system_server 834 and servicemanager 342).

Fixed performance mode on and off (`setMode(FIXED_PERFORMANCE, …)`):

```
$ cmd power set-fixed-performance-mode-enabled true;  grep 'to 643:' transaction_log
112521: async from 834:2202 to 643:0 context binder node 311 handle 7 size 84:0
$ cmd power set-fixed-performance-mode-enabled false; grep 'to 643:' transaction_log
112542: async from 834:2202 to 643:0 context binder node 311 handle 7 size 84:0
```

One oneway call from system_server per toggle. Touch input gives a steady stream of
calls from system_server's `InputDispatcher` thread (tid 953), which is the
`userActivity` → `setBoost(INTERACTION)` path (INTERACTION is the one boost the HAL
reports as supported):

```
119645: async from 834:953 to 643:0 context binder node 311 handle 7 size 84:0
119761: async from 834:953 to 643:0 ...
120568: async from 834:875 to 643:0 ...   (875 = android.ui)
$ cat /proc/834/task/953/comm
InputDispatcher
```

`cmd power set-mode 1` (battery saver) sent nothing: the framework refuses battery saver
while plugged in (`settings get global low_power` stayed 0).

Effect on the CPUs: none. The limits are the same before, during and after fixed
performance mode:

```
p0 300000-1804800  p4 710400-2419200  p7 844800-3091200   (all three times)
policy0/4/7 scaling_governor = schedutil (set by init.gts7l.rc)
$ ls /sys/devices/system/cpu/intel_pstate
ls: /sys/devices/system/cpu/intel_pstate: Invalid argument
```

`hals/power` only knows `intel_pstate` (see `pstate.rs`; it logs "no intel_pstate, power
hints are ignored"). The `TODO(T4)` in `init.gts7l.rc` already notes that the HAL should
drive cpufreq/devfreq on this SoC.

## IPowerStats: Fails

```
$ cat /vendor/etc/init/power.stats-default.rc
service vendor.power.stats-default /vendor/bin/hw/android.hardware.power.stats-service.example
$ dumpsys powerstats
  PowerEntityId: 0, PowerEntityName: CPU      StateName: Idle / Active
  PowerEntityId: 1, PowerEntityName: Display  StateName: Off / On
  ChannelId: 0, ChannelName: Rail1, ChannelSubsystem: Display
  ChannelId: 1, ChannelName: Rail2, ChannelSubsystem: CPU
  ChannelId: 2, ChannelName: Rail3, ChannelSubsystem: Modem
  EnergyConsumerId: 0, Type: 0, Name: GPU
  EnergyConsumerId: 1, Type: 5, Name: MODEM
  PowerStats State Residency Data: # files : 0
```

These are exactly the fakes in AOSP's
`hardware/interfaces/power/stats/aidl/default/main.cpp` (`FakeEnergyMeter` with
Rail1/Rail2/Rail3, `FakeStateResidencyDataProvider` "CPU" and "Display",
`FakeEnergyConsumer` "GPU" and "MODEM"); their values are made up.
`dumpsys android.hardware.power.stats.IPowerStats/default` prints nothing (the example
has no dump).

Real sources the kernel has today: cpuidle residency per CPU
(`/sys/devices/system/cpu/cpu0/cpuidle/state{0,1}`: `WFI`, `cpu-sleep-0-0`, usage
122863 / 31627). No RPMh/`qcom_stats` and no energy meter are visible
(`/sys/kernel/debug/qcom_stats` is absent).

## IThermal: Partial

`dumpsys thermalservice`:

```
HAL Ready: true
HAL connection:
	ThermalHAL AIDL 3  connected: yes
Current temperatures from HAL:
	Temperature{mValue=35.9, mType=-1, mName=aoss0-thermal, mStatus=0}
	Temperature{mValue=37.4, mType=-1, mName=cpu0-thermal, mStatus=0}
	Temperature{mValue=38.2, mType=-1, mName=cpu7-top-thermal, mStatus=0}
	Temperature{mValue=34.7, mType=-1, mName=gpu-top-thermal, mStatus=0}
	Temperature{mValue=36.4, mType=-1, mName=npu-thermal, mStatus=0}
	... 25 sensors in all
Current cooling devices from HAL:
	CoolingDevice{mValue=0, mType=6, mName=cpufreq-cpu0-0}
	CoolingDevice{mValue=0, mType=6, mName=cpufreq-cpu4-1}
	CoolingDevice{mValue=0, mType=6, mName=cpufreq-cpu7-2}
	CoolingDevice{mValue=0, mType=6, mName=devfreq-3d00000.gpu-3}
Temperature static thresholds from HAL:
	TemperatureThreshold{mName=cpu0-thermal, mHotThrottlingThresholds=[NaN, NaN, NaN, 90.0, NaN, NaN, 110.0]}
	TemperatureThreshold{mName=gpu-top-thermal, mHotThrottlingThresholds=[NaN, NaN, NaN, 85.0, NaN, 90.0, 110.0]}
	...
```

Same moment from sysfs (millidegrees):

```
thermal_zone0  aoss0-thermal     36300
thermal_zone1  cpu0-thermal      37800
thermal_zone10 cpu7-top-thermal  37800
thermal_zone15 gpu-top-thermal   35100
thermal_zone23 npu-thermal       36800
... 25 zones, same names
```

The names, count and values match within a few tenths of a degree (the two reads are a
second apart). The values are real and move with load (the cached set from earlier in
the boot read 50-54 °C).

What is missing: `mType=-1` (UNKNOWN) on every sensor. `hals/thermal/src/sensors.rs`
recognises PC zone names (`x86_pkg_temp`, `acpitz`, `iwlwifi_*`, ...) but not Qualcomm
ones (`cpu*-thermal`, `gpu-*-thermal`, `cluster*`). Android computes the thermal status
from typed sensors (SKIN above all), so `Thermal Status: 0` can never change and apps
get no thermal headroom. There is also no SKIN or BATTERY sensor (the battery
thermistor is the same missing IIO channel as in IHealth).

## ILights: Works (HAL) / brightness path is HWC

```
$ ls /sys/class/backlight /sys/class/leds
/sys/class/backlight: ae94000.dsi.0
/sys/class/leds:      mmc0::
$ cat ae94000.dsi.0/{brightness,actual_brightness,max_brightness,bl_power,type}
145 145 462 0 raw
$ dumpsys lights
Lights:
  Light id=0 ordinal=0 color=00000000
```

`getLights` (code 2) returns one light: id 0, ordinal 0, type 0 (BACKLIGHT). The
`mmc0::` LED has no Android light type and is skipped, as designed.

### Settings brightness → backlight

```
setting   Display Brightness   sysfs brightness = actual_brightness (max 462)
  1          0.0                   0      <- backlight off
  9          0.0315              142
 30          0.114               265
 60          -                   334
128          0.5                 402
200          -                   441
255          1.0                 462
```

The register follows the setting every time (`actual_brightness` equals `brightness`),
and the original value (9) was put back. The curve is not linear in Android's float
(0.5 → 402/462), so something on the way applies a perceptual mapping.

The writer is not the light HAL. `dumpsys lights` keeps `color=00000000`, the light
HAL's pid gets no binder calls while the slider moves, and `strace` shows the composer
writing sysfs:

```
$ strace -f -p 651 -e trace=openat   # android.hardware.composer.hwc3-service.drm
651 openat(AT_FDCWD, "/sys/class/backlight/ae94000.dsi.0/brightness", O_WRONLY|O_CREAT|O_TRUNC|...) = 9
651 openat(AT_FDCWD, "/sys/class/backlight/ae94000.dsi.0/brightness", ...) = 9
$ strace -f -p 638 -e trace=openat,write   # light HAL
(nothing)
```

Android uses HWC3 `setDisplayBrightness` when the composer supports it, and
drm_hwcomposer does, so ILights is the fallback only.

Issue found: setting 1 (the slider's minimum) gives float 0.0 and drm_hwcomposer writes
raw 0, which turns the backlight off. On phones 0.0 is the dimmest visible level, and
`mBacklightMinimum=0.035433073` suggests the same was meant here.

### ILights directly

```
$ service call android.hardware.light.ILights/default 1 i32 0 i32 1 i32 24 i32 -8355712 i32 0 i32 0 i32 0 i32 0
Result: Parcel(00000000)                 # setLightState(0, color 0xff808080)
$ cat .../brightness
232                                      # 0x80/0xff * 462 = 231.9
$ service call android.hardware.light.ILights/default 1 i32 5 ...
Result: Parcel(fffffff9 ...)             # id 5 -> EX_UNSUPPORTED_OPERATION (-7)
```

Afterwards the setting was moved 10 → 9, and HWC wrote 142 back.

### Notification LED, keyboard light

None. `/sys/class/leds` has only `mmc0::`. The MAX77705 has an RGB LED block
(`/sys/bus/platform/devices/max77705-rgb` exists) but no driver is bound to it, and the
Tab S7 has no notification LED on its front anyway. The Book Cover keyboard
(`stm32-pogo-keyboard`, input "Samsung Book Cover Keyboard") registers no LED class
device (no caps-lock or backlight LED).

## Cleanup

Everything changed was put back: `screen_brightness` is 9 again (backlight 142), fixed
performance mode is off, battery saver never turned on, the `strace` output files in
`/data/local/tmp` were removed. A first `setLightState` attempt passed the color in hex,
which `service call` reads as 0, so the backlight was off for about 3 s before the
restore.

## Cần bạn tự kiểm tra

1. **Sạc khi rút/cắm USB (IHealth).** Rút cáp USB khỏi tablet, đợi 10 giây, cắm lại, rồi
   chạy:
   ```
   adb -s R52R504WFTK shell dumpsys battery
   adb -s R52R504WFTK shell logcat -d -b events | grep battery
   ```
   Kỳ vọng: `USB powered` lần lượt false rồi true trong sự kiện `battery_status`/
   `battery_level`. Khi chưa có fuel gauge, `level` vẫn là 100 và `present: false`; đó
   là lỗi kernel đã biết, không phải lỗi HAL.
2. **Mức pin trong thanh trạng thái.** Mở thanh trạng thái: biểu tượng pin hiện gì (100%,
   dấu ?, hay không có)? Kết quả này cho biết SystemUI xử lý trường hợp không có pin ra
   sao.
3. **Đèn nền thay đổi thật.** Mở Cài đặt → Màn hình → Độ sáng, kéo thanh trượt từ thấp
   lên cao và quan sát màn hình sáng dần. Sau đó kéo hết về mức thấp nhất: theo đo đạc,
   đèn nền về 0 (màn hình tối hẳn). Hãy xác nhận màn hình có tắt đèn nền hay chỉ rất tối,
   rồi trả thanh trượt về mức cũ.
4. **Tác dụng của thermal.** Chạy một ứng dụng nặng vài phút, rồi
   `adb -s R52R504WFTK shell dumpsys thermalservice | head -20`: nhiệt độ tăng nhưng
   `Thermal Status` sẽ vẫn là 0 (vì sensor không có type). Hãy chú ý tablet có quá nóng
   không: hiện chỉ có giới hạn nhiệt của kernel bảo vệ.
5. **Đèn bàn phím Book Cover.** Gắn bàn phím, bấm Caps Lock: nếu có đèn trên bàn phím
   sáng lên, báo lại (trong sysfs không thấy LED nào, nên Android không điều khiển được).
