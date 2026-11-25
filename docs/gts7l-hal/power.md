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

**Update, same day (branch `claude/gts7l-fix-fuelgauge-a44bc2`):** the fuel gauge, IThermal
and IPower gaps below are fixed; see [Fixes](#fixes-2026-11-25) near the end. The sections
before it describe the build above, as first checked.

## Summary

| HAL | Result | Why |
|---|---|---|
| IHealth (`health-pc`) | **Partial** → **Works** after the fixes | Reports the MAX77705 charger and USB-C correctly, but there is no battery: the MAX77705 fuel gauge (`max17042` driver) never probes, so level/voltage/temperature are absent (Android shows 100 %, 0 mV, 0 °C). |
| IPower (`power-pc`) | **Partial** → **Works** after the fixes | Mode hints and INTERACTION boosts reach the HAL (binder transactions from `system_server`), but they change nothing: the HAL only drives `intel_pstate`, which an ARM SoC does not have. cpufreq stays on schedutil with the same limits. |
| IPowerStats (`power.stats-default`) | **Fails** | It is AOSP's example service with fake entities (Rail1/2/3, CPU Idle/Active, GPU/MODEM). No real SM8250 data. |
| IThermal (`thermal-pc`) | **Partial** → **Works** after the fixes | All 25 Qualcomm thermal zones with live temperatures that match `/sys/class/thermal`, 4 cooling devices, trip-point thresholds. But every sensor has type UNKNOWN (-1): no CPU/GPU/SKIN/BATTERY type, so Android's thermal status and throttling never react. |
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

## Fixes (2026-11-25)

Branch `claude/gts7l-fix-fuelgauge-a44bc2`. Tested on the tablet with the new kernel in `boot`
(`#1 SMP PREEMPT Wed Nov 25 08:11:57 +07 2026`) and the three rebuilt HAL binaries bind-mounted
over `/vendor/bin/hw` (not a full image); the `modules.load` and `init.gts7l.rc` changes were
done by hand (`modprobe`, `chown`/`chmod`) with the same effect. USB stayed plugged in.

### Why the fuel gauge never probed

Not the DT node, the i2c address or a config option: the gauge (`fuel-gauge@36`,
`maxim,max77705-battery`, `max17042` driver built in) names an IIO channel for its battery
thermistor (`io-channels = <&pm8150l_adc ADC5_AMUX_THM1_100K_PU>`, `io-channel-names = "temp"`),
and the PM8150L ADC driver is a module (`CONFIG_QCOM_SPMI_ADC5=m`) that `modules.load` did not
list. So the `pm8150l_adc` IIO device never appeared and the gauge deferred forever
(`deferred probe pending: max17042: temp channel`). The same module feeds the `batt`, `chg`,
`skin-msm` and `blkt` thermal zones (generic-adc-thermal, also a module), which were missing
too.

`modprobe qcom-spmi-adc5` on the running tablet was enough: `iio:device0/1` appear and
`max170xx_battery` registers at once.

Two smaller kernel problems showed up on the way:

- The gauge's ALRT interrupt was not wired (`IRQ index 0 not found`), so the driver never
  enabled its SOC alerts and Android only learned of level changes by polling. It goes through
  the MAX77705's INTSRC (FG = bit 2), as on starqltechn.
- `cycle_count` read 27607: the MAX17047-family `Cycles` register counts in 1 % of a cycle.

### Changes

| Where | What |
|---|---|
| `aosp-device/gts7l/kernel/modules.bp` | `modules.load` gets `qcom-vadc-common`, `qcom-spmi-adc5`, `thermal-generic-adc`, `qcom-spmi-adc-tm5`, `qcom-spmi-temp-alarm` |
| kernel patch 0220 | DTS: `interrupt-parent = <&max77705>; interrupts = <2>;` on `fuel-gauge@36` |
| kernel patch 0221 | max17042: `platform_get_irq_optional()` (no bogus error, defers on `-EPROBE_DEFER`, polls without one); `cycle_count` in whole cycles for MAX17042/7/50 |
| `hals/health` (`supply.rs`) | Android's charge counter now comes from `charge_now` (remaining charge) before `charge_counter`: max17042 exports its free-running QH coulomb counter there, which read 55325000 µAh for an ~7.3 Ah battery |
| `hals/thermal` (`sensors.rs`, `thermal.rs`) | Qualcomm zones get real types: `cpuN-*`/`clusterN-thermal` CPU, `gpu-*-thermal` GPU, `batt-thermal` and `max170xx_battery` BATTERY, `wlan-thermal` WIFI, `blkt-thermal` SKIN; a real SKIN sensor replaces the virtual one; default thresholds for skin and battery zones without trips; `cpufreq-*` cooling devices are CPU, `devfreq-*.gpu` GPU, `ath11k_thermal` WIFI |
| `hals/power` (`freq.rs`, `main.rs`, `power.rs`) | Without `intel_pstate`, the hints move each cpufreq policy's `scaling_min_freq`/`scaling_max_freq` (schedutil keeps choosing the frequency) |
| `gts7l/init.gts7l.rc` | `scaling_{min,max}_freq` of policy0/4/7 group `system`, 0664, for the power HAL (replaces the `TODO(T4)`) |

The health HAL already mapped every other field (`capacity`, `voltage_now`, `current_now`,
`current_avg`, `temp`, `status`, `health`, `charge_full`, `charge_full_design`, `cycle_count`,
`technology`, `voltage_min_design`); `time_to_full_now` returns `EINVAL` from the driver and is
reported as unsupported (-1).

### Proof: IHealth

Kernel: the IRQ is wired, cycle count is sane, the supply is there.

```
$ grep max170 /proc/interrupts
 249:  0 0 0 0 0 0 0 0  max77705  2 Edge  max170xx_battery
$ cat /sys/class/power_supply/max170xx_battery/cycle_count
276
```

`/sys/class/power_supply/max170xx_battery` and `dumpsys battery` at the same moment:

```
status=Charging  capacity=100  voltage_now=4340937  current_now=162500
current_avg=143750  temp=359  charge_now=7332500  charge_full=7372500
charge_full_design=7255000  cycle_count=276  health=Good

  USB powered: true
  Max charging current: 3000000
  Charge counter: 7332500
  status: 2                (CHARGING)
  health: 2                (GOOD)
  present: true
  level: 100
  voltage: 4340
  temperature: 359         (35.9 °C)
  technology: Li-ion
```

The status bar's controller has the same level:

```
$ dumpsys activity service com.android.systemui | grep -A3 'BatteryController state'
    BatteryController state:
      mLevel=100
      mPluggedIn=true
      mCharging=true
```

Values move (40 s apart, on the charger):

```
12:38:18  Charge counter: 7335000  sysfs voltage_now=4339375  current_now=114843
12:38:58  Charge counter: 7337500  sysfs voltage_now=4341718  current_now=81250
12:39:38  Charge counter: 7337500  sysfs voltage_now=4339218  current_now=135156
```

and the framework logged the level rising while it charged (events log, earlier in the same
session, with the module loaded by hand):

```
battery_level: [97,4307,350]
battery_status: [2,2,1,2,Li-ion]      (CHARGING, GOOD, present, USB)
battery_level: [98,4324,350]
battery_level: [99,4330,351]
battery_level: [100,4332,351]
```

Not checked: unplugged (adb runs over that cable). See the checks at the end.

### Proof: IThermal

```
$ dumpsys thermalservice
Thermal Status: 0
	Temperature{mValue=42.0, mType=0, mName=cpu0-thermal, mStatus=0}         CPU
	Temperature{mValue=46.3, mType=0, mName=cpu7-top-thermal, mStatus=0}
	Temperature{mValue=42.8, mType=0, mName=cluster1-thermal, mStatus=0}
	Temperature{mValue=37.4, mType=1, mName=gpu-top-thermal, mStatus=0}      GPU
	Temperature{mValue=35.937, mType=2, mName=batt-thermal, mStatus=0}       BATTERY
	Temperature{mValue=35.9, mType=2, mName=max170xx_battery, mStatus=0}
	Temperature{mValue=36.546, mType=3, mName=blkt-thermal, mStatus=0}       SKIN
	Temperature{mValue=38.7, mType=14, mName=wlan-thermal, mStatus=0}        WIFI
	CoolingDevice{mValue=0, mType=2, mName=cpufreq-cpu0-0}                   CPU
	CoolingDevice{mValue=0, mType=3, mName=devfreq-3d00000.gpu-3}            GPU
	CoolingDevice{mValue=0, mType=11, mName=ath11k_thermal-4}                WIFI
	TemperatureThreshold{mType=3, mName=blkt-thermal, mHotThrottlingThresholds=[NaN, 39.0, 43.0, 45.0, 47.0, 52.0, 55.0]}
	TemperatureThreshold{mType=2, mName=batt-thermal, mHotThrottlingThresholds=[NaN, NaN, 45.0, 50.0, 55.0, 60.0, NaN]}
	TemperatureThreshold{mType=0, mName=cpu0-thermal, mHotThrottlingThresholds=[NaN, NaN, NaN, 90.0, NaN, NaN, 110.0]}
	TemperatureThreshold{mType=1, mName=gpu-top-thermal, mHotThrottlingThresholds=[NaN, NaN, NaN, 85.0, NaN, 90.0, 110.0]}
```

32 sensors in all (the 25 tsens zones plus the PMIC and ADC ones that now load). The skin
sensor is the display backlight thermistor (`blkt_thm`), behind the front glass: 34-37 °C idle
on the charger. `skin-msm-thermal`, despite its name, sits next to the SoC (70 °C under load,
see the DTS comment) and stays UNKNOWN. Thermal status is 0 because the skin is below 39 °C;
the skin thresholds are the common phone/tablet surface values and are a guess for this
thermistor: tune them after a heavy-load test (checks below).

### Proof: IPower

The HAL now logs its cpufreq policies at start:

```
power-service.pc: main::pstate: no intel_pstate, using the cpufreq limits
power-service.pc: main::freq: cpufreq .../policy0: 300000-1804800 kHz, 17 frequencies
power-service.pc: main::freq: cpufreq .../policy4: 710400-2419200 kHz, 18 frequencies
power-service.pc: main::freq: cpufreq .../policy7: 844800-3091200 kHz, 21 frequencies
```

Fixed performance mode pins each cluster at the offered frequency nearest below 80 % of its
maximum, and puts the limits back afterwards:

```
$ cmd power set-fixed-performance-mode-enabled true / false
before  p0 300000-1804800   p4 710400-2419200   p7 844800-3091200
fixed   p0 1420800-1420800  p4 1862400-1862400  p7 2457600-2457600
after   p0 300000-1804800   p4 710400-2419200   p7 844800-3091200
```

Touch (`input swipe` held 2 s) raises the floors to about half of each maximum for the
INTERACTION boost, then they drop back:

```
during  p0 883200  p4 1171200  p7 1516800
after   p0 300000  p4 710400   p7 844800
```

Profiles (`freq.rs`): boost/launch floor 50 %, game floor 30 %, sustained cap 80 %, fixed
min = max = 80 %, battery saver and screen off cap 60 %, of each policy's boot limits. The
governor stays schedutil. The GPU (devfreq) is not steered yet.

### Remaining

- **IPowerStats:** still AOSP's example with fake data. A real one could report cpuidle
  residency per CPU; there is no energy meter or RPMh stats in this kernel.
- **SELinux:** a `max170xx_battery` node read right after it appears is denied as plain `sysfs`
  until ueventd relabels it (`sysfs_batteryinfo` afterwards). Harmless while permissive; with
  the module loaded at `on fs` (before the HALs start) it should not happen at boot; not checked.
- **Skin and battery thresholds** are not calibrated (see IThermal above).
- **GPU devfreq** under IPower (GAME, FIXED_PERFORMANCE).

## Cleanup

Everything changed was put back: `screen_brightness` is 9 again (backlight 142), fixed
performance mode is off, battery saver never turned on, the `strace` output files in
`/data/local/tmp` were removed. A first `setLightState` attempt passed the color in hex,
which `service call` reads as 0, so the backlight was off for about 3 s before the
restore.

Fixes test: the kernel with patches 0220-0221 stays in `boot` (the rest of the boot image,
ramdisk and command line, is the one that was there). The HAL binaries were bind-mounted from
`/data/local/tmp` and the `scaling_*_freq` permissions changed by hand; both go away at the
next reboot, which brings back the old HALs and leaves the ADC modules unloaded until an image
with this branch is flashed. Fixed performance mode was turned off again.

## Cần bạn tự kiểm tra

Với image có nhánh `claude/gts7l-fix-fuelgauge-a44bc2` (kernel 0220-0221, `modules.load`
mới, ba HAL mới):

1. **Rút/cắm USB (IHealth).** Rút cáp USB, đợi khoảng 1 phút, cắm lại, rồi chạy:
   ```
   adb -s R52R504WFTK shell dumpsys battery
   adb -s R52R504WFTK shell logcat -d -b events | grep battery_
   adb -s R52R504WFTK shell cat /sys/class/power_supply/max170xx_battery/current_now
   ```
   Kỳ vọng: trong `battery_status` có `[3,...]` (DISCHARGING, plugged 0) khi rút và `[2,...]`
   khi cắm lại; `current_now` âm khi rút, dương khi đang sạc; `level` và `voltage` là giá
   trị thật (không còn 100 % / 0 mV / 0 °C).
2. **Mức pin trên thanh trạng thái.** Mở thanh trạng thái: biểu tượng pin phải hiện cùng
   phần trăm với `cat /sys/class/power_supply/max170xx_battery/capacity`, có tia sét khi
   cắm sạc. Rút sạc vài phút: phần trăm phải giảm dần.
3. **Đèn nền thay đổi thật.** Mở Cài đặt → Màn hình → Độ sáng, kéo thanh trượt từ thấp
   lên cao và quan sát màn hình sáng dần. Sau đó kéo hết về mức thấp nhất: theo đo đạc,
   đèn nền về 0 (màn hình tối hẳn). Hãy xác nhận màn hình có tắt đèn nền hay chỉ rất tối,
   rồi trả thanh trượt về mức cũ.
4. **Tác dụng của thermal.** Chạy một game hoặc ứng dụng nặng 5-10 phút, rồi
   `adb -s R52R504WFTK shell dumpsys thermalservice | grep -E "Thermal Status|blkt|batt-thermal"`.
   Sensor SKIN là `blkt-thermal` (đèn nền, sau mặt kính): ghi lại nhiệt độ của nó và cảm giác
   tay khi chạm mặt trước. Từ 39 °C `Thermal Status` lên 1 (LIGHT), 43 °C lên 2. Nếu mặt kính
   vẫn mát mà status đã lên 2-3, ngưỡng cần nâng; báo lại số đo.
5. **Đèn bàn phím Book Cover.** Gắn bàn phím, bấm Caps Lock: nếu có đèn trên bàn phím
   sáng lên, báo lại (trong sysfs không thấy LED nào, nên Android không điều khiển được).
