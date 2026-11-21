# Rust vendor HALs (phases F2, F4–F8)

Status (2026-11-11): `aosp_pc_x86_64-cp2a-userdebug` builds and boots on Cuttlefish
(`sys.boot_completed=1`). The Rust lights HAL runs as `u:r:hal_pc_light:s0`, is listed by
`service list` / `dumpsys -l` as `android.hardware.light.ILights/default`, the device
manifest carries `ILights/default (@3)` matching the framework matrix, there are no VINTF
errors in logcat and no `hal_pc_light` AVC denials. Host tests (`libpchal_test`, 8) and
device tests (`android.hardware.light-service.pc_test`, 4) pass.

Our Android userspace HALs are written in Rust, built from source in the AOSP 17 tree
(`aosp/`, see [aosp-build.md](aosp-build.md)) as part of our own product
`aosp_pc_x86_64`. This page covers the device tree, the shared `libpchal` crate, the
packaging pattern every HAL follows, and how to build and boot it.

## Layout

The device tree lives in this repository at `aosp-device/` and appears in the AOSP tree
as `aosp/device/android-pc/pc_x86_64`:

```
aosp-device/
  AndroidProducts.mk        lunch target aosp_pc_x86_64-cp2a-userdebug
  aosp_pc_x86_64.mk         product: reads hals/*/hal.mk, inherits the Cuttlefish phone
  BoardConfig.mk            Cuttlefish vsoc_x86_64_only board + our vendor sepolicy dirs
  Android.bp                rust_defaults "pc_hal_defaults" for HAL binaries
  sepolicy/vendor/te_macros pc_hal_service(domain, hal_attribute) macro
  libpchal/                 shared Rust library (rust_library, vendor_available)
  hals/<name>/              one directory per HAL
    hal.mk                  PRODUCT_PACKAGES += ..., switches off the Cuttlefish HAL it replaces
    Android.bp              rust_binary + vintf_fragment (+ rust_test)
    <service>.rc            init service
    <service>.xml           VINTF manifest fragment
    sepolicy/               file_contexts + <domain>.te
    src/                    main.rs and the AIDL implementation
```

### How the tree gets into AOSP

`tools/aosp-device-link.sh` copies `aosp-device/` to `aosp/device/android-pc/pc_x86_64`
with `rsync -a --delete`. `tools/aosp17-build.sh` runs it before every build. The copy keeps
mtimes, so ninja only rebuilds what changed. The mirror is generated: edit `aosp-device/`,
never the copy.

Modules whose dependencies are only visible to `//vendor` live in `aosp-device-vendor/`,
mirrored the same way to `aosp/vendor/android-pc` (today only `fmq`, see F6).
`EXCLUDE_HALS="wifi camera"` leaves those `hals/` directories out of the mirror, so a HAL
another session is still writing does not break this build's Soong analysis.

Links do not work:

- **Directory symlink:** Soong's finder, which also finds `AndroidProducts.mk` for Make,
  skips symlinked directories. It only follows them with `ALLOW_BP_UNDER_SYMLINKS=true`,
  which applies to the whole tree and walks AOSP's own symlink loops.
- **Per-file symlinks (`cp -as`):** the build works, but Soong installs `init_rc` files by
  copying the symlink itself. The vendor image then held
  `/vendor/etc/init/<hal>.rc -> /home/.../aosp-device/...`, init could not read it, the HAL
  never started and system_server waited for `ILights/default` forever.
- **Local manifest:** needs `aosp-device/` to be its own git project and a `repo sync`
  after every change.

### Product and board

- `aosp_pc_x86_64` inherits `device/google/cuttlefish/vsoc_x86_64_only/phone/aosp_cf.mk`,
  so it still boots on Cuttlefish with the same `cvd` host tools.
- `PRODUCT_DEVICE := pc_x86_64`, so the images go to `aosp/out/target/product/pc_x86_64/`
  (the stock product keeps `vsoc_x86_64_only/`). Our own device name is needed for
  `BoardConfig.mk`, which is the only place `BOARD_VENDOR_SEPOLICY_DIRS` can be added.
  `BoardConfig.mk` includes Cuttlefish's `vsoc_x86_64_only/BoardConfig.mk` and appends
  `sepolicy/vendor` plus every `hals/*/sepolicy`.
- The Cuttlefish makefile sets `PRODUCT_SOONG_ONLY` only when `TARGET_PRODUCT` is its own
  name, so our makefile sets it again (Soong-only build, as in F1).
- `hals/*/hal.mk` files are included **before** the Cuttlefish product, so they can set
  Cuttlefish's `LOCAL_ENABLE_*` switches (`?=` there) to drop the HAL they replace.

## libpchal

`aosp-device/libpchal`, crate `pchal`. Host-supported, so its unit tests run on the host
(`libpchal_test`).

| Item | Purpose |
|---|---|
| `pchal::init_logging(tag)` | `log` → logcat via `liblogger`; logs panics (HAL stderr goes to /dev/null) |
| `pchal::Service::new(descriptor, instance)` | builder: `.registration(Registration::Lazy)`, `.extra_threads(n)`, `.serve(binder)` registers with servicemanager and joins the thread pool |
| `pchal::sysfs::{read_string, read, read_optional, write, entries}` | sysfs attributes: newline stripped, parse via `FromStr`, `read_optional` maps a missing file to `None`, missing class dir = empty list |
| `pchal::Callbacks<dyn ICallback, F>` | client callback list: `add` rejects the same binder twice and `remove` an unknown one (`EX_ILLEGAL_ARGUMENT`, as the VTS of every HAL with callbacks checks), `notify(filter, call)` calls outside the lock and drops callbacks that return `DEAD_OBJECT`; `F` is a per-callback filter (a temperature type, ...) |
| `pchal::uevent::{UeventSocket, Uevent}` | kernel uevents (`NETLINK_KOBJECT_UEVENT` group 1, kernel sender only), parsed into `ACTION`/`DEVPATH`/`SUBSYSTEM`/`KEY=value`; for hotplug (power supplies now, sound cards/cameras/Bluetooth later) |
| `pchal::procstat::CpuTimes` | aggregate CPU busy/total ticks from `/proc/stat`, `usage_since` = busy % between two samples |
| `pchal::poll::readable(fd, timeout)` | `poll(2)` for `POLLIN` with a timeout (`EINTR` = timeout); reader threads use it to notice a stop flag |
| `pchal::devfs::open(path, Access)` | open a `/dev` node with `O_CLOEXEC | O_NOCTTY` |
| `pchal::Error` / `pchal::Result` | every error carries its path; `From<Error> for binder::Status` (ILLEGAL_STATE + message), so AIDL methods can use `?` |

`Service::serve` returns `Result<Infallible>`, so `main` reads:

```rust
let binder = BnLights::new_binder(lights, BinderFeatures::default());
let Err(err) = Service::new(Lights::get_descriptor(), "default").serve(binder.as_binder());
error!("{err}");
ExitCode::FAILURE
```

Lazy services (`Registration::Lazy`) need `interface aidl <name>`, `oneshot` and
`disabled` in the `.rc` file.

## The proof HAL: android.hardware.light

`hals/light` builds `android.hardware.light-service.pc` (`ILights/default`, AIDL V3).
It drives LED class devices (`/sys/class/leds`) and display backlights
(`/sys/class/backlight`):

- LEDs are typed by the function part of `devicename:color:function`: `kbd_backlight` →
  KEYBOARD, `micmute` → MICROPHONE, `wlan`/`wifi` → WIFI, `bluetooth`, `camera`,
  `charging`/`battery` → BATTERY, `indicator`/`status` → NOTIFICATIONS. Lock-key LEDs
  (`capslock`, ...) belong to the input subsystem and are not exposed.
- Every backlight is a BACKLIGHT light.
- `setLightState` writes the colour's luminance scaled to `max_brightness`. Flash modes are
  shown as a steady light, and no light supports effects (`minUpdatePeriodMillis` 0).

Why lights: it fits PC hardware (keyboard backlight, mic-mute LED, laptop panel
backlight), is small, and Cuttlefish does not depend on it. Cuttlefish's own lights HAL
(`com.google.cf.light`, a vsock bridge to the host) serves the same instance, so
`hals/light/hal.mk` sets `LOCAL_ENABLE_LIGHT := false` and ours replaces it. The other
candidate, vibrator, has no PC hardware behind it. On Cuttlefish there are no LEDs or
backlights, so the HAL registers and reports an empty list. The service log says
`found N lights`.

## F4: health, thermal, power

Status (2026-11-11): the three HALs replace Cuttlefish's on `aosp_pc_x86_64`. On Cuttlefish
(SELinux enforcing) they run as `hal_pc_health`, `hal_pc_thermal`, `hal_pc_power` with no AVC
denials; VTS: `VtsHalHealthTargetTest` 25/25 passed, `VtsHalThermalTargetTest` 9 passed +
2 skipped (skin thresholds and forecast: emulator / not supported),
`VtsHalPowerTargetTest` 10 passed + 13 skipped (no hint sessions). Unit tests: host
`libpchal_test` 14, `*_supply_test` 5, `*_sensors_test` 4, `*_pstate_test` 4; device
`*-service.pc_test` health 8, thermal 6, power 8.

Real PC (Z790, one boot from the F4 stick, permissive): `dumpsys battery` AC powered,
present false, level 100; `dumpsys thermalservice` CPU0 and VIRTUAL-SKIN 74 °C with
thresholds `[NaN, 90, 95, 100, 101, 103, 105]`, 16 `processor-*` and 5 `fan-*` cooling
devices; the power HAL found `intel_pstate` active with 16 policies and switched EPP; no
AVC denials from the HALs. That boot showed two bugs, fixed since and verified on
Cuttlefish only: the global `intel_pstate` limits were not writable (now opened in the
power HAL's `.rc`, see below), and the ACPI thermal zones appeared after the thermal HAL
had probed (it now probes again when the class directories change). The re-check on the
PC is part of the combined F4–F8 USB test.

| HAL | Binary / instance | Replaces (switch in `hal.mk`) |
|---|---|---|
| health V5 | `android.hardware.health-service.pc`, `IHealth/default` | `com.google.cf.health` (`LOCAL_HEALTH_PRODUCT_PACKAGE`; recovery keeps Cuttlefish's) |
| thermal V3 | `android.hardware.thermal-service.pc`, `IThermal/default` | `com.android.hardware.thermal` (`LOCAL_THERMAL_HAL_PRODUCT_PACKAGE`) |
| power V7 | `android.hardware.power-service.pc`, `IPower/default` | `com.android.hardware.power` (no switch: `overrides` in `Android.bp`; its power.stats half, `android.hardware.power.stats-service.example`, is installed on its own) |

Versions are the newest the framework matrix for target level 202604 accepts (health 3-5,
power 5-7, thermal 3).

### Health (`hals/health`)

`supply.rs` reads `/sys/class/power_supply`: `Mains` = AC, `USB*`/`BrickID` = USB,
`Wireless`; the system battery is the first present `Battery` whose `scope` is not `Device`
(a wireless mouse's is). Energy-based ACPI batteries (`energy_*` µWh, `power_now` µW) are
converted to charge and current at `voltage_min_design`; currents are negative while
discharging; the charge counter falls back to `charge_full * capacity / 100`.

A desktop with no supply at all (the Z790 PC) is reported as AC online, battery not
present, status/health UNKNOWN, level 100, capacity level UNSUPPORTED. That is what Android
expects of battery-less devices: `BatteryService` never shuts down a device without a
battery, VTS requires status UNKNOWN for it, and SystemUI shows its "battery unknown" icon
when `EXTRA_PRESENT` is false. Level 100 instead of healthd's 0 so apps that display the
level do not show an empty battery.

Updates: a uevent thread (`pchal::uevent`) pushes new info on every `power_supply` event; a
battery is also re-read every 60 s. `getBatteryHealthData` must succeed from vendor level
202604, so it returns zeros without a battery. Storage info, disk stats, hinge info and
charging policies are `EX_UNSUPPORTED_OPERATION`. Kernel: `CONFIG_ACPI_AC=m`,
`CONFIG_ACPI_BATTERY=m` (`ac.ko`, `battery.ko`) so laptops get their supplies.

### Thermal (`hals/thermal`)

`sensors.rs` probes once at start:

- hwmon drivers `coretemp`/`k10temp`/`zenpower` → CPU (`Package id N`/`Tctl`, named `CPU0`...,
  TjMax from `tempN_crit`, default 100 °C), GPU drivers (`amdgpu`, `nouveau`, `i915`, `xe`,
  `radeon`) → GPU, `nvme` → UNKNOWN; other hwmon drivers (board WMI, DIMM, NIC) are skipped;
- thermal zones: `x86_pkg_temp` → CPU only without coretemp (same sensor), `iwlwifi_*` →
  WIFI, ACPI `acpitz` and the rest → UNKNOWN;
- `VIRTUAL-SKIN` (SKIN) = the hottest CPU package. Android takes its thermal status and
  headroom from the single SKIN sensor, and on a PC the CPU package is what throttles.

Thresholds (°C): CPU and skin LIGHT TjMax-10, MODERATE TjMax-5, SEVERE TjMax (the CPU
throttles itself there), CRITICAL +1, EMERGENCY +3, SHUTDOWN +5. The sensor never reads
above TjMax, so the framework's thermal shutdown (CPU/GPU/SKIN at SHUTDOWN) is left to the
CPU's THERMTRIP. hwmon `max`/`crit`/`emergency` → SEVERE/CRITICAL/EMERGENCY; ACPI trips
`passive`/`hot`/`critical` → SEVERE/EMERGENCY/SHUTDOWN, ignoring trips below 40 °C (the Z790
firmware has a 20.8 °C critical trip on a zone that always reads 16.8 °C).

Cooling devices: `Fan` → FAN, `Processor`/`intel_powerclamp`/`TCC Offset` → CPU, `iwlwifi*`
→ WIFI, others COMPONENT, named `<type>-<N>`. A poll thread (5 s, 1 s while anything is
above NONE) sends `notifyThrottling` on severity changes and `notifyCoolingDeviceChanged` on
state changes, filtered by the type each callback registered for. Each poll first compares
the entries of `/sys/class/thermal` and `/sys/class/hwmon` with the last probe and probes
again if they changed: ACPI `thermal.ko`, `coretemp` and `iwlwifi` are modules that can
load after the HAL starts (on the Z790 the two `acpitz` zones did).
`forecastSkinTemperature` is unsupported. AIDL thermal has no CPU usage call; `/proc/stat`
is used by the power HAL's CPU headroom instead.

### Power (`hals/power`)

`pstate.rs` drives `intel_pstate` in active (HWP) mode: `energy_performance_preference` of
every `cpufreq/policy*`, and `intel_pstate/{min_perf_pct,max_perf_pct,no_turbo}`. The values
at start are the balanced defaults. Hints → profile (highest priority first) → settings:

| Hint | Profile | Settings |
|---|---|---|
| `FIXED_PERFORMANCE` | Fixed | EPP performance, no turbo, min = max perf |
| `LOW_POWER` (battery saver) | PowerSave | EPP power, no turbo |
| `INTERACTIVE` off, `DEVICE_IDLE`, `DISPLAY_INACTIVE` | Idle | EPP power |
| `SUSTAINED_PERFORMANCE` | Sustained | no turbo |
| `INTERACTION` boost (200 ms default, 5 s max), `LAUNCH`, `GAME_LOADING` | Boost | EPP performance, min perf ≥ 50 % |
| `GAME` | Game | EPP performance |
| none | Balanced | boot defaults |

A worker thread applies a profile only when it changes and wakes when a boost ends; EPP
values the CPU lacks fall back to `balance_performance`/`balance_power`. Without
`intel_pstate` active (Cuttlefish, AMD, `acpi-cpufreq`) the hints change nothing, but
`FIXED_PERFORMANCE` is still reported (VTS requires it). The knobs are root-owned 0644:
`ueventd.pc.rc` (installed as `/odm/etc/ueventd.rc`, which `/system/etc/ueventd.rc` imports)
makes each CPU's `energy_performance_preference` group `system` writable on its uevent; the
global `intel_pstate/*` files belong to no device that sends uevents, so the HAL's `.rc`
opens them `on boot`. A failed write is logged once per file.

Hint sessions (ADPF) are not implemented: the 202604 matrix does not require them, and
with sessions VTS also requires the FMQ session channel. `getCpuHeadroom` returns the global
idle share over the requested window from `/proc/stat` (the binder thread waits for the
window, 50 ms to 10 s); GPU headroom and composition data are unsupported.

### SELinux

- `pc_hal_service(hal_pc_<name>, hal_<name>)` per HAL; health also needs
  `typeattribute ... bpfdomain` (every `hal_health_server` is one).
- `/sys/class/*` entries are `sysfs` symlinks: `allow <domain> sysfs:lnk_file read`.
- Thermal zones and cooling devices: `genfscon sysfs /devices/virtual/thermal` →
  `sysfs_thermal`.
- Paths that depend on the firmware or bus layout are labelled by **file_contexts regexes
  on `/sys`**, which ueventd applies (recursive restorecon at coldboot, then per uevent):
  `/sys/devices/.*/power_supply(/.*)?` → `sysfs_batteryinfo` (wakeup nodes stay
  `sysfs_wakeup`), `/sys/devices/.*/hwmon[0-9]+(/.*)?` → `sysfs_hwmon_pc` (our type; the
  platform has none). Cuttlefish labels its virtio paths the same way.
- intel_pstate knobs keep the platform's `sysfs_devices_system_cpu`; the power HAL gets
  `w_file_perms` on it and reads `proc_stat`.

## F5: Bluetooth (`hals/bluetooth`)

Status (2026-11-11): `android.hardware.bluetooth-service.pc` (IBluetoothHci V1) replaces the
interim C++ HAL of F3a (AOSP's default `BluetoothHci`, which lived in `aosp-device-vendor/`
and is gone). On Cuttlefish (enforcing), running on the virtual controller
(`stop bt_hci; start vendor.bluetooth-pc-h4`): `VtsHalBluetoothTargetTest` 16/16 passed
(loopback, bandwidth, VSR 4.2/5 requirements included), the Bluetooth stack turns ON on it,
no AVC denials. Unit tests: host `*_packet_test` 6, `*_mgmt_test` 3, `libpchal_test` 16;
device `*-service.pc_test` 10. The real-PC check (AX211) is part of the combined F4–F8 test.

Transports (`src/transport.rs`):

| Transport | Selected by | Framing |
|---|---|---|
| HCI user channel | no arguments (service `vendor.bluetooth-pc`, started by `init.pc.rc` on the PC) | one H4 frame per datagram |
| H4 UART | `--h4 <tty>` (service `vendor.bluetooth-pc-h4`, `${vendor.ser.bt-uart}` = `/dev/hvc5` on Cuttlefish, started by hand for VTS) | byte stream, split by `H4Decoder` |

- **Finding the controller** (`src/mgmt.rs`): on the management channel
  (`HCI_CHANNEL_CONTROL`) it sends `Read Controller Index List` every 500 ms and takes
  `hci0`, or the lowest index above it (a re-plugged dongle comes back as `hci1`), from the
  reply or an `Index Added` event. It gives up after 3 s (`OPEN_TIMEOUT`); AOSP's default HAL
  waits forever.
- **User channel**: `bind(HCI_CHANNEL_USER)` makes the kernel stack let go of the
  controller (it powers it down first); `EBUSY`/`EALREADY` while the kernel is still in setup
  (firmware download) is retried every 100 ms until the deadline. Closing the socket gives the
  controller back. Needs `CAP_NET_ADMIN` (`.rc` capabilities, `net_admin` in the domain).
- **Initialize**: open, send HCI_Reset and wait up to 2 s for its Command Complete
  (dropping anything older), link to the client's death, start the `hci-reader` thread,
  then `initializationComplete(SUCCESS)` — called after the session lock is released, since
  the stack sends its first command from inside the callback. A second `initialize` gets
  `ALREADY_INITIALIZED` (VTS checks it). Failures map to `UNABLE_TO_OPEN_INTERFACE` (no
  controller) or `HARDWARE_INITIALIZATION_ERROR` (no reset answer).
- **Reader thread**: polls with `pchal::poll::readable` every 100 ms so `close` (or client
  death) can stop and join it. If the link breaks (USB controller unplugged) it sends the
  stack an HCI Hardware Error event, which makes the stack restart Bluetooth.
- **Sends** hold the session lock for one `write` per packet, so frames never interleave on
  the UART; a failed write is logged, not returned (the reader reports the broken link).
- No VINTF fragment: the `IBluetoothHci/default` instance is declared by Cuttlefish's
  `com.google.cf.bt` APEX, whose `bt_hci` service `init.pc.rc` stops on the PC. The
  binary keeps the C++ HAL's name, so `pc.mk` and `init.pc.rc` did not change.
- AOSP's `libbluetooth_offload_hal` (`HciHal` trait + `HciProxy`, used by Cuttlefish's own
  Rust HAL) is not used: it is only visible to `vendor/` and Cuttlefish, and its LE audio
  software offload needs an audio HAL that offloads. Our HAL implements `IBluetoothHci`
  directly and lives in the normal `hals/` layout.

Without a controller (QEMU, a PC without Bluetooth) the HAL answers
`UNABLE_TO_OPEN_INTERFACE` after 3 s and keeps running. The stack
(`system/gd/hal/hci_backend_aidl.cc`) asserts on any status but SUCCESS (and on a HAL
that does not answer within 3 s, `bluetooth.gd.start_timeout`), so `com.android.bluetooth`
aborts; `BluetoothManagerService` restarts it 6 times and then leaves Bluetooth OFF. This is
the stack's behaviour with any HAL, AOSP's default included.

SELinux: domain `hal_pc_bluetooth` (`pc_hal_service(hal_pc_bluetooth, hal_bluetooth)`):
`bluetooth_socket { create bind read write }`, `net_admin`, and `bt_device` for the H4
service. No `ioctl`/`setopt` on the socket (the former is a platform neverallow) and no
`/dev/rfkill` (unlabelled `device`; the user channel works with the radio as the kernel
left it).

## F6: Audio (`hals/audio`)

Status (2026-11-12): `android.hardware.audio.service.pc` serves the primary module,
`IModule/default` (`android.hardware.audio.core` V4, the version AOSP's HAL APEX declares;
the 202604 matrix accepts 1-4). SELinux enforcing, domain `hal_pc_audio`, no AVC denials.
`VtsHalAudioCoreTargetTest --gtest_filter='*IModule_default*'` (framework stopped, as the
VTS template does): 148 passed, 73 skipped, 0 failed, both on Cuttlefish with an
`snd-aloop` loopback card and on QEMU with `intel-hda` + `hda-duplex`. The C++ HAL's instances (IConfig,
r_submix, bluetooth) still pass their part (161 tests). Playback and capture
through AudioFlinger work on both (`tools/audio-check.sh`, below). Unit tests: host
`*_cards_test` 5, `*_worker_test` 9; device `*-service.pc_test` 29 (with the mixer policy
and port/patch logic). The real-PC check (ALC897) is part of the combined F4–F8 test.

### What serves what

| Instance | Served by |
|---|---|
| `IModule/default` (primary) | this HAL, service `vendor.audio-hal` |
| `IConfig/default`, `IModule/r_submix`, `IModule/bluetooth`, `IFactory/default` (effects) | AOSP's default C++ HAL in the `com.android.hardware.audio` APEX (Cuttlefish's `LOCAL_AUDIO_PRODUCT_PACKAGE`), unchanged |

The C++ HAL creates one module per `<module>` of `/vendor/etc/audio_policy_configuration.xml`;
`hals/audio/audio_policy_configuration.xml` (installed through Cuttlefish's
`LOCAL_AUDIO_PRODUCT_COPY_FILES` switch, set in `hal.mk`) lists only `r_submix` and
`bluetooth`, so `IModule/default` is ours. The framework reads every module's ports from the
HAL (`IConfig` + `IModule`), not from that file. There is no VINTF fragment: the APEX
already declares `IModule/default` V4, and a second declaration would fail `checkvintf`.
The effects configuration Cuttlefish installs together with its policy file
(`audio_effects.mk`) is included by `hal.mk`. The init service is called `vendor.audio-hal`:
`audioserver.rc` starts that name with audioserver and restarts it with the other audio
HALs when `sys.audio.restart.hal` is set (audioserver does that after a HAL crash, VTS for
`CanBeRestarted`); an own `on property:` trigger does not run, because audioserver.rc's
action resets the property first.

### Ports

| Port | Kind | Backed by |
|---|---|---|
| `Speaker` (`OUT_SPEAKER`, default output) | attached device | first analog playback PCM |
| `Built-In Mic` (`IN_MICROPHONE`, address `bottom`) | attached device | first analog capture PCM |
| `primary output` (flag `PRIMARY`) → `Speaker` | mix port, 1 stream | |
| `Built-In Mic` → `primary input` | mix port, 1 stream | |
| `HDMI Out` (`OUT_DEVICE`, connection `hdmi`) | external device template (F9h) | the HDMI/DP PCM picked at connection |
| `hdmi output` → `HDMI Out` | mix port, 1 stream, 16 bit stereo 32/44.1/48 kHz | |

`src/cards.rs` reads `/proc/asound/cards` and `/proc/asound/pcm`: USB-Audio cards and
HDMI/DisplayPort PCMs are skipped for the attached ports, the lowest analog card/device with playback (capture)
backs the speaker (microphone). `pcm::probe` asks the device with `HW_REFINE` which formats,
channel counts (1, 2) and rates (8–48 kHz; higher rates are left out, or the framework
mixes at 192 kHz on an HDA codec) it accepts; the mix and device ports offer 16 bit PCM
(the device's own format if it has no S16_LE). A path without hardware (no sound card, a
codec without capture, Cuttlefish's AC97 that has no driver) gets a null device that keeps
time and returns silence, so the framework always has a speaker and a microphone.

Nominal latency (one client burst) is 40 ms, 85 ms on virtio-snd (as AOSP's Cuttlefish
HAL); the minimum buffer follows the framework's rule (multiple of 16 frames, power of two
above 512 frames), so 2048 frames at 48 kHz. The ALSA buffer holds two bursts in periods of
half a burst (the nearest the hardware accepts otherwise), playback starts once a burst is
queued, xruns are recovered by preparing the PCM again.

At start, `src/ctl.rs` sets the mixer of the cards in use through the control ABI
(`ELEM_LIST`/`ELEM_INFO`/`ELEM_WRITE`): HDA codecs come up muted with volumes at 0 and
Android has no alsactl. Output switches on and output volumes to the maximum (0 dB on HDA;
stream volumes are applied by the framework), `Capture Switch` on and `Capture Volume` at
two thirds; loopback paths (`Mic Playback`, `Line Playback`, `Beep`, ...) are not touched.
Master volume/mute and microphone mute are applied in software by the streams.

### Streams over FMQ

AOSP's `libfmq_rust` (crate `fmq`) creates the command, reply and data queues of a
`StreamDescriptor`; the Rust AIDL backend generates `@FixedSize` `Command` (a union) and
`Reply` with zerocopy layouts the C++ clients share. Two catches:

- `libfmq_rust` is only visible to `//vendor`. `aosp-device-vendor/fmq` (mirrored to
  `aosp/vendor/android-pc/fmq` by `tools/aosp-device-link.sh`) is a one-line re-export,
  `libpcfmq`, visible to `device/android-pc`; it also adds the C++ `libfmq`'s shared
  libraries (`libbase`, `libcutils`, `libutils`).
- The wrapper has no `availableToRead`/`availableToWrite`. A `read_many(n)`/`write_many(n)`
  transaction succeeds only if `n` elements fit and a failed one changes nothing, so the data
  queue is drained/filled in halving steps (`DataFmq` in `src/stream.rs`). A completion
  commits its whole transaction when dropped, whether read or not, so it can not be used to
  probe.

Each stream has a worker thread (nice -16) that blocks on the command queue
(`read_blocking`, event flag) and runs the state machine of `src/worker.rs` (host-tested,
no AIDL types): AOSP's `Stream.cpp` without async callbacks, MMap or offload (STANDBY, IDLE,
ACTIVE, PAUSED, DRAINING, DRAIN_PAUSED, ERROR; bursts in STANDBY prime the output; DRAINING
ends after `ModuleDebug.streamTransientStateDelayMs`). A stream not connected by a patch
simulates its I/O (3 ms per burst, silence). `close()` (or dropping the last binder of a
stream) writes `halReservedExit` with the stream's random cookie through a second handle on
the command queue and joins the worker; the PCM is closed in standby.

The generated parcelables derive only `Debug`, so ports, configs and patches are copied
and compared through a parcel (`aidl::dup`, `aidl::same`).

What it does not do (yet): USB audio (the framework's `UsbAlsaManager` reports USB cards,
which needs a USB template port next to the HDMI one), MMap/AAudio exclusive mode, offload, telephony, sound dose, hardware
volume/gain, effects on streams or devices (all reported as unsupported, which VTS accepts).

### Testing

- `snd-aloop.ko` is in Cuttlefish's vendor modules: `insmod /vendor/lib/modules/snd-aloop.ko`
  then `setprop sys.audio.restart.hal 1` gives the HAL a real ALSA card. What the HAL plays
  to `hw:0,0` can be recorded from `hw:0,1` with `tinycap -D 0 -d 1`, and `tinyplay -D 0 -d 1`
  feeds the HAL's capture (snd-aloop needs the same channel count on both sides: mono for
  `audio-check.sh record`).
- `tools/run-qemu-aosp17-usb.sh` now adds `-device intel-hda -device hda-duplex` (QEMU's
  generic codec, `snd_hda_codec_generic`). `AUDIO=wav` records the guest's playback to
  `out/aosp17-usb-audio.wav` (its capture fails with EIO: the wav backend has no input),
  `AUDIO=none` gives silent capture, `pa` (default) uses the host's sound server.
- `tools/audio-check.sh play SECONDS HZ` / `record SECONDS` runs a small `AudioTrack` /
  `AudioRecord` program (`tools/audio-check/AudioCheck.java`, built with AOSP's prebuilt JDK
  and d8) on the device with `app_process`; `ADB` picks the device. `hdmi 1` / `hdmi 0`
  connects/disconnects an HDMI sink through the audio service, as `WiredAccessoryManager`
  does for an HDMI jack, so the HDMI path can be tried without a monitor; `play` prints the
  device type the track was routed to (2 speaker, 9 HDMI).
- VTS for the core HAL expects the framework stopped (`stop`,
  `setprop vts.native_server.on 1`): otherwise audioserver holds the only stream of each mix
  port and its patches.

### On the real PC (combined test)

Card 0 (HDA Intel PCH, ALC897) backs both ports: line out / front headphones (the codec
auto-mutes the line out when headphones are plugged) and the mic input the `Input Source`
control selects. Check `logcat -s android.hardware.audio.service.pc` (`found card ...`,
`Playback: ...`, mixer lines `card 0: Master Playback Switch = 1`), `tinymix -D 0`,
`dumpsys media.audio_flinger`, and play/record with `tools/audio-check.sh`. HDMI on the
RTX 4070 (card 1) is the external `HDMI Out` port (next section); the webcam microphone
(card 2) is not used.

### HDMI/DisplayPort output (F9h)

The real-PC f9d test: sound from the onboard codec, none from the monitor. The HAL listed
the RTX 4070's HDMI PCMs (card 1, devices 3, 7, 8, 9) but only ever exposed `Speaker`, and
nothing told the framework a monitor was there, so no HDMI device existed for the audio
policy to route to.

How it works now:

1. The HD Audio HDMI codec driver reports each pin as a jack: the kcontrol
   `HDMI/DP,pcm=<N> Jack` and an input device `HDA NVidia HDMI/DP,pcm=<N>` that sends
   `SW_LINEOUT_INSERT` + `SW_VIDEOOUT_INSERT` when a monitor with a valid ELD is on the pin
   (`snd_jack_report(..., monitor_present && eld_valid ? SND_JACK_AVOUT : 0)`).
2. `config_useDevInputEventForAudioJack` (framework overlay in `hals/audio/overlay`) makes
   `WiredAccessoryManager` read audio jacks from input switches (the PC has no extcon or
   `/sys/class/switch` jack): both switches on → `DEVICE_OUT_HDMI` connected. Line out and
   headphone jacks of the analog codec become `DEVICE_OUT_LINE`/headphones, which no module
   offers, so they are refused and the analog path keeps playing through `Speaker` (the
   codec routes its one PCM to the plugged jack itself).
3. The audio policy asks the HAL to connect its `HDMI Out` template
   (`connectExternalDevice`). `Endpoint::connect_hdmi` (`src/config.rs`) picks the PCM:
   `persist.vendor.audio.hdmi` if set, else the HDMI PCM whose jack kcontrol is on, else the
   first HDMI PCM; it logs every `/proc/asound/card<N>/eld#*` (`monitor_present`,
   `eld_valid`, monitor name), switches the card's `IEC958 Playback Switch`es on, and probes
   the PCM (16 bit stereo at 32/44.1/48 kHz, CEA-861 basic audio). The connected port gets
   those profiles and a copy of the template's route from `hdmi output`.
4. The engine prefers HDMI over the speaker for media, so music, video and system sounds
   that follow media go to the monitor while it is connected; unplugging it disconnects the
   port (the HAL refuses while a port config of it is still set, as VTS requires) and audio
   returns to the onboard codec.

Checked on Cuttlefish (2026-11-13, enforcing, `snd-aloop` card 0, property `0,1` so the
HDMI path has its own PCM): the audio policy lists `HDMI Out` as `AUDIO_DEVICE_OUT_HDMI`;
`audio-check.sh hdmi 1` connects port 11, AudioFlinger opens a second output thread on
`AUDIO_DEVICE_OUT_HDMI`, `play 3 440` reports device type 9 and the tone arrives as 440 Hz
on the loopback of `hw:0,1`; `hdmi 0` disconnects the port and the next `play` goes to the
speaker (type 2). No AVC denials. `VtsHalAudioCoreTargetTest --gtest_filter='*IModule_default*'`:
158 passed, 127 skipped, 0 failed, with the external-device tests (connect/disconnect,
routes, port configs, two devices) now running against the HDMI template; device unit tests
35/35, host `*_cards_test` 9/9. After a connection the audio policy asks the module for the
mix ports of two ids it does not have (`getAudioMixPort ... Failed`, the HAL logs `port 9 not
found`); the output opens anyway. What Cuttlefish cannot show is the jack path (no HDMI
codec), so `WiredAccessoryManager` turning a real HDMI jack into the connection is checked
on the PC.

Choosing the output by hand, as root (`adb root`; the property is `vendor_pc_audio_prop`,
readable by the HAL only):

| `persist.vendor.audio.hdmi` | Effect |
|---|---|
| unset or `auto` | the PCM whose jack reports a monitor, else the first HDMI PCM |
| `off` | the HDMI connection is refused: audio stays on the onboard codec |
| `<device>` / `<card>,<device>` | this playback PCM (any card; `<device>` alone means the first card with HDMI PCMs) |

The property is read at each connection; after changing it, replug the monitor or restart
the HAL (`setprop sys.audio.restart.hal 1`).

**NVIDIA without a GPU driver.** The NVIDIA HDMI codec assigns its PCMs to pins dynamically
(`snd_hda_codec_hdmi` with `dp_mst`): a PCM is attached to a pin only when that pin has a
valid ELD, and a PCM without a pin opens fine but plays into nothing
(`hdmi_pcm_open_no_pin`). The ELD and the presence bit of an NVIDIA codec are written by the
GPU driver (nouveau or NVIDIA's) when it sets a mode; on this PC the display runs on the
firmware's GOP framebuffer (`pcfb`) with no GPU driver, so the jacks most likely stay off and
no HDMI PCM can reach the monitor, whatever the HAL does. The pclog service now records the
evidence on the next real-PC boot: `state-*.txt` gets `== hdmi eld` (`monitor_present`/
`eld_valid` per pin), `tinymix` of every card (jack and IEC958 controls) and the audio
service's device list, and `hdmi-check.txt` logs a 3 s, 375 Hz beep played with `tinyplay`
to each HDMI PCM in turn, 150 s after boot. If the monitor stays silent and `eld_valid` is 0,
HDMI audio needs a GPU driver that programs the audio path (nouveau with GSP firmware on
the RTX 4070), which is a display-stack change, not a HAL one. With a monitor on the
onboard Intel graphics (i915) or an AMD card, the jacks work and the path above applies.

**Real-PC result (H1, 2026-11-15).** The stick's default entry (pcfb, build
`eng.builder.20261114.2338`, pclog `boot-4`) confirmed it: card 1 "HDA NVidia" (`01:00.1`)
has its four HDMI PCMs (3, 7, 8, 9), every `/proc/asound/card1/eld#*` reads
`monitor_present 0`, `eld_valid 0`, all `HDMI/DP,pcm=N Jack` controls are off, the beeps in
`hdmi-check.txt` "play" (`tinyplay` exits 0) but nothing comes out, and the audio check
plays to the speaker device (type 2, the ALC897) because no HDMI device was ever connected.
That is the hardware, not the HAL: the NVIDIA HD Audio function only carries sound once the
GPU driver turns audio on for the output it drives. In nouveau on GSP-RM firmware that is
`r535_sor_hda_eld` (the ELD and presence bit, which also reach `snd-hda-codec-nvhdmi` through
the DRM audio component) and `r535_sor_hdmi_audio` / `r535_sor_dp_audio` (audio packets on
the link), all run from nouveau's modeset. The UEFI GOP framebuffer that pcfb uses sets a
mode without audio, and no driver runs on the GPU there, so on pcfb the monitor's speakers
cannot be reached; forcing the HAL onto an HDMI PCM would only play into a PCM with no pin.

So monitor sound needs a nouveau entry ("Android 17, NVIDIA nouveau" or "Android 17,
NVIDIA GPU (NVK + ANGLE)"), where drm_hwcomposer's modeset gives the codec its ELD and the
chain above (jack → `WiredAccessoryManager` → `HDMI Out` → `Endpoint::connect_hdmi`) runs.
The Limine menu (`tools/mkusb-aosp17.sh --menu`) now says this under each entry
(`comment:`, shown at the bottom of the menu for the selected entry) and remembers the last
booted entry (`remember_last_entry: yes`, an EFI variable), so picking a nouveau entry once
keeps it, and its sound, for later boots; until an entry has been picked, the default is
still the pcfb entry. On the nouveau entries the sound follows the screen: when Android
turns the screen off, drm_hwcomposer disables the CRTC, nouveau turns the output's audio
off, the jack goes off, and media moves to the onboard codec until the screen is back.

On `main` today the nouveau entries are on branch `gpu-alternatives`; the only entry,
"Android 17" (NVIDIA's driver, formerly "Android 17, NVIDIA (proprietary)"), gives the codec
its ELD the same way, through nvidia-drm's modeset (`docs/nvidia-proprietary.md`, K4), and
the checks below apply to it. The stick now has only that entry, no menu.

What to check on the real PC (nouveau entry, monitor on the RTX 4070): `state-*.txt`
`== hdmi eld` shows `monitor_present 1`, `eld_valid 1` on one pin with the monitor's name,
`tinymix 1` shows that PCM's jack `On`, `== audio devices` lists HDMI connected, the beep in
`hdmi-check.txt` is heard on that PCM, and `audio-check.txt` reports device type 9.

QEMU check of the menu (image `out/aosp17-usb-h1-qemu.img`, build
`eng.builder.20261115.0135`, `tools/run-qemu-aosp17-usb.sh`): Limine 12.9.6 shows the pcfb
comment under the default entry and the "sound plays on the monitor" comment once
"Android 17, NVIDIA nouveau" is selected; after booting that entry and a `system_reset`
the menu comes back with it highlighted and counts down to it (OVMF keeps the variable
until QEMU exits; the launcher starts every run with fresh variables). The default entry
and the nouveau entry both reached `sys.boot_completed=1` (nouveau loaded, no NVIDIA GPU
to bind). QEMU has no NVIDIA HDMI codec, so the sound itself is for the real PC.

## F7: Camera (`hals/camera`)

Status (2026-11-12): done on Cuttlefish. With `vivid` loaded (a YUYV webcam node, picked
up by hotplug as camera `101`), SELinux enforcing, no AVC denials:
`VtsAidlHalCameraProvider_TargetTest --gtest_filter='*pc_0*'` 45/45 passed, cameraserver
lists the camera (`dumpsys media.camera`), device unit test
(`android.hardware.camera.provider-service.pc_test`, every metadata buffer checked with
`libcamera_metadata`'s `validate_camera_metadata_structure`) 4/4, host tests
(`libpccamera_test`: metadata codec, image conversion/scaling, JPEG round trips, V4L2
struct layouts and ioctl numbers, sensor/metadata logic) 26/26. The first run found two
bugs, both fixed: the domain could not open `/dev/dri/renderD128`, so minigbm's mapper
failed to initialise and every `importBuffer` failed (`processCaptureRequestPreview`,
`flushPreviewRequest`); and `BLOB` was the first entry of the stream configurations, which
VTS `getSessionCharacteristics` takes as its sample stream with `Dataspace::UNKNOWN` (now
YUV comes first). The real-PC photo with the Camera app is part of the combined USB test.

### What it serves

`ICameraProvider/pc/0` (provider V4, labelled `hal_camera_service`), service
`vendor.camera-pc` (user `cameraserver`, groups `camera system`), domain `hal_pc_camera`.
Every `/dev/videoN` that is a V4L2 capture node with a YUYV or MJPEG mode at 5 fps or more
becomes camera `100 + N`, device name `device@1.1/pc/<id>` (UVC metadata nodes are skipped).
Ids start at 100, like AOSP's external camera HAL, so they never clash with another
provider's. A `video4linux` uevent triggers a rescan (after 300 ms and 1.5 s, once ueventd
has set owner and label) and `cameraDeviceStatusChange` PRESENT / NOT_PRESENT; `setCallback`
reports the cameras present right away, inside the call, as VTS and cameraserver expect.

Cuttlefish's own provider stays (a multi-install APEX chosen by the bootconfig
`androidboot.vendor.apex.com.google.emulated.camera.provider.hal`: the emulated one, or
`.v4l2` for AOSP's C++ external HAL, instance `external/0`). That is the fallback: it keeps
serving its instance next to ours. On the real PC the boot config should set the property to
`none` so the webcam is the only camera (a one-line change in `tools/mkusb-aosp17.sh`, left
uncommitted in the working tree for the combined USB test; put back `.v4l2` there to use the
C++ HAL instead of ours).

### Camera model

An `EXTERNAL` hardware level camera like AOSP's C++ `ExternalCameraDevice`: facing
`EXTERNAL`, `BACKWARD_COMPATIBLE` only, 3A always reported converged (AE/AWB `CONVERGED`, AF
`FOCUSED_LOCKED` after a trigger until cancelled), request settings echoed in results.
`src/sensor.rs` keeps one V4L2 mode per landscape size (highest frame rate, YUYV on a tie)
and builds the static characteristics, the four request templates (MANUAL and ZSL return
`ILLEGAL_ARGUMENT`, which VTS accepts), results and session characteristics. Outputs are
`YCBCR_420_888`/`IMPLEMENTATION_DEFINED` (2 processed streams) and `BLOB` JPEG (1 stall
stream) at every mode size; fps ranges are `(fps/2, fps)` per mode.

Per request the worker thread (`src/session.rs`) picks the smallest mode that covers every
output, restarts streaming only when the mode or frame interval changes, takes a frame
(4 tries, 1 s each), converts MJPEG or YUYV to I420, then centre-crops and scales it to each
output buffer: YCbCr through the buffer's plane layout, JPEG with a `CameraBlob` trailer at
the end of `Stream.bufferSize`. Shutter notify, buffers and the result go back through
`processCaptureResult` with inline metadata; a failed frame sends `ERROR_REQUEST` and returns
the buffers with their acquire fences as release fences. `flush` waits for requests in
flight. No FMQs (`getCaptureRequestMetadataQueue`/`getCaptureResultMetadataQueue` return
empty descriptors, so clients send and receive metadata inline), no HAL buffer manager, no
offline sessions, no torch, no injection sessions.

### Pieces

| File | What |
|---|---|
| `src/metadata.rs`, `src/tags.rs` | `camera_metadata_t` encoder/decoder in Rust (sorted, 8-byte aligned data); typed `Tag<T>` constants generated from `camera_metadata_tags.h`/`camera_metadata_tag_info.c` |
| `src/v4l2.rs` | V4L2 ioctls (`QUERYCAP`, `ENUM_FMT/FRAMESIZES/FRAMEINTERVALS`, `S_FMT`, `S_PARM`, mmap streaming); `Stream` stops and frees on drop, `Frame` re-queues its buffer on drop |
| `src/image.rs` | YUYV → I420, JPEG planes of any subsampling → I420, crop + scale, write into a YCbCr layout |
| `src/jpeg.rs` + `jpeg/pcjpeg.c` | JPEG decode/encode. AOSP's `libjpeg` (libjpeg-turbo) is built without `turbojpeg.c`, so a small C library, `libpcjpeg`, drives libjpeg's raw data interface and keeps its `setjmp`/`longjmp` error handling out of Rust |
| `src/gralloc.rs` | stream buffers through the stable C `AIMapper` (IMapper 5, `dlopen` of `/vendor/lib64/hw/mapper.*.so`): import, lock for CPU writes, plane layouts from the `PLANE_LAYOUTS` metadata. `AHardwareBuffer_createFromHandle` was not usable: it checks a stride the HAL does not know |
| `src/camera.rs`, `src/device.rs`, `src/provider.rs`, `src/main.rs` | probing, `ICameraDevice`, `ICameraProvider` with hotplug, service registration |

`libpccamera` (everything except the AIDL and gralloc parts) is host-supported; its unit
tests run as `libpccamera_test`.

SELinux: `pc_hal_service(hal_pc_camera, hal_camera)` (the `hal_camera` attribute already
grants `video_device`), a `netlink_kobject_uevent_socket` for hotplug, and
`hal_client_domain(hal_pc_camera, hal_graphics_allocator)` for the buffers, and the DRM
render node (`gpu_device`, as Cuttlefish's `gpu_access` macro grants) because the minigbm
mapper opens it when it initialises.

### Testing

Cuttlefish's `vendor_dlkm` loads `vivid.ko` (with `v4l2-tpg.ko`) and `v4l2loopback.ko`, so
the guest has `/dev/video0` (v4l2loopback, no formats until a producer writes, so the HAL
skips it). `vivid` is not loaded by default: `insmod /vendor_dlkm/lib/modules/vivid.ko
n_devs=1 node_types=0x1` adds a capture node whose input 0 is a webcam (YUYV, 9 sizes up
to 3840x2160; do not pass `input_types`, which makes input 0 a TV input with no usable
modes). Check `ls -l /dev/video*` and
`logcat -s android.hardware.camera.provider-service.pc` (`/dev/videoN: <name>: K sizes up to
WxH`; restart with `stop vendor.camera-pc; start vendor.camera-pc` to see the start lines,
which come before logd). If no node offers YUYV/MJPEG, the provider reports no cameras and
VTS has nothing to run against.

```
adb shell /data/local/tmp/VtsAidlHalCameraProvider_TargetTest \
  --gtest_filter='*android.hardware.camera.provider.ICameraProvider/pc_0*'
adb shell /data/local/tmp/android.hardware.camera.provider-service.pc_test
```

(push both from `out/target/product/pc_x86_64/testcases/<name>/x86_64/`; the VTS
parameter names its instance `android_hardware_camera_provider_ICameraProvider_pc_0` or
similar, list with `--gtest_list_tests`). Then `adb shell dumpsys media.camera` should list
camera `100`, and the AOSP Camera app should take a photo. Expected caveats: tests needing
FULL/LIMITED features, logical cameras, torch, HAL buffer management, offline sessions or
10-bit output are skipped; vivid's frame rate and sizes are odd, so a test that needs a
specific size may skip. On the real PC: `/dev/video0` (UVC webcam), photo and preview in
the Camera app, hotplug by unplugging/replugging.

## F8: Wi-Fi (`hals/wifi`)

Status (2026-11-12): done on Cuttlefish (`--enable_wifi=true`, two hwsim radios), SELinux
enforcing, no AVC denials: VTS `VtsHalWifiTargetTest` 3/3, `VtsHalWifiChipTargetTest` 36 +
18 skipped, `VtsHalWifiStaIfaceTargetTest` 7 + 18 skipped, `VtsHalWifiApIfaceTargetTest` 6 +
2 skipped, 0 failed; device unit test 25/25. Wi-Fi turns on (`wlan0 on phy1: combinations
[{STA}≤1, {AP,P2P}≤1]`); a framework SoftAP (`cmd wifi start-softap`) comes up on `wlan1`
and `wlan0` scans it, joins it with WPA2 and gets a DHCP lease. (Tethering then restarts
the hotspot because the hotspot's own client became its upstream, which drops the link;
that is framework behaviour, not the HAL.) The HAL, its tests and `vendor.img` build, clippy and
rustfmt are clean, `check-vintf-all` passes. Host tests 25/25, one of them read-only
against the build host's own AX211 (iwlwifi, the target PC): two combinations
(`[{STA}, {P2P}]`, `[{STA}, {AP,P2P}]`), driver `iwlwifi`, firmware `89.b20dcd1f.0`, 62
usable channels over 2.4/5/6 GHz. The real-PC check (AX211 connection, hotspot) is part of
the combined USB test.

`android.hardware.wifi-service.pc` serves `IWifi/default` (AIDL V4, the newest the 202604
matrix accepts, 2-4) for the chip behind `wlan0`: Intel iwlmvm on the PC,
mac80211_hwsim on Cuttlefish. It replaces Cuttlefish's Wi-Fi APEX
(`com.android.hardware.wifi`: AOSP's default C++ HAL over the emulator legacy HAL) through
`overrides` in `Android.bp`.

**Fallback:** `PC_WIFI_HAL=legacy` (environment, or set in a product makefile before the
HAL makefiles are read) leaves our HAL out of `PRODUCT_PACKAGES`, so the override does not
apply and Cuttlefish's APEX is installed as before. The default is `rust`.

### AIDL bindings

`android.hardware.wifi` has no Rust backend in AOSP (`backend.rust.enabled: false`, as has
`android.hardware.wifi.common`). `hals/wifi/aidl/` holds copies of the frozen V4 API, of
`wifi.common` V2 (which V4 imports) and of the `PersistableBundle` declaration. A
`genrule` runs `aidl --lang=rust` over them with the frozen version and hash (the
`.hash` files of `aidl_api/`), then `aidl_rust_glue`, as `aidl_interface` does; the
`rust_library` `android.hardware.wifi-V4-pc-rust` has the official crate name
(`android_hardware_wifi`) and module paths. Clients see the same descriptors, version and
hash as from AOSP's HAL. When AOSP freezes a V5, copy it and change the version and hash.

### What it does

| Area | Implementation |
|---|---|
| `IWifi.start` | finds `wlan0` (`wifi.interface`), its wiphy (`NL80211_CMD_GET_INTERFACE`) and what the wiphy supports (split `NL80211_CMD_GET_WIPHY` dump); fails with `ERROR_UNKNOWN` and `onFailure` if there is no Wi-Fi interface |
| Chip modes | one mode (id 0) whose combinations come from the wiphy's nl80211 interface combinations: station → STA, AP → AP, P2P client/GO → P2P; at most one STA (no multi-STA), limits capped at 2. hwsim gives `[{STA}≤1, {AP,P2P}≤1]`, iwlmvm `[{STA}, {P2P}]` and `[{STA}, {AP,P2P}]`; a driver without combinations gives one interface of any supported type |
| STA | `wlan0` (`wifi.interface`); the driver's interface is used as is |
| AP | `ro.vendor.wifi.sap.interface`, else the first free `wlanN` after `wlan0` (`wlan1`); created with `NL80211_CMD_NEW_INTERFACE` (type AP) on the chip's wiphy if it does not exist (iwlmvm), used as is if it does (Cuttlefish's second hwsim radio); deleted on removal or `stop` only if the HAL created it |
| P2P | the name in `wifi.direct.interface` (`p2p-dev-wlan0`); wpa_supplicant creates the P2P device interface |
| MAC address | `SIOCSIFHWADDR` with the interface taken down and brought back up; factory MAC from `ETHTOOL_GPERMADDR` (current address if the driver has none) |
| Country code | `NL80211_CMD_REQ_SET_REG` (chip and AP interface) |
| Usable channels | the wiphy's enabled channels in the requested bands (DFS channels count as `BAND_5GHZ_DFS`), 20 MHz; SoftAP/P2P GO only where the device may initiate radiation (no `NO_IR`). `WifiUsableChannel.channel` is the centre frequency in MHz, not the channel number (F9e: with channel numbers the framework found no band at all, `updateSupportedBandForStaInternal wlan0 : 0x0`) |
| Debug info | driver name + version and firmware version from `ETHTOOL_GDRVINFO` |
| APF | reported as APFv4 with 4096 bytes; the program is only stored and read back (as the emulator HAL does), no driver on a PC runs it, so every packet still reaches the host. VTS requires APF from vendor API level U on non-PC devices |
| `wifi.active.interface` | set to the first STA (else AP) interface, as AOSP's HAL does |

Unsupported (`ERROR_NOT_SUPPORTED`, which VTS and the framework accept): link layer stats,
background scans, RSSI monitoring, roaming control, ND offload, keep-alive packets, packet
fates, debug ring buffers and dumps, TX power scenarios, latency/VoIP/MLO modes, coex, AFC,
radio combinations, chip capabilities, TWT, cached scan data, RTT. NAN and bridged APs
are `ERROR_NOT_AVAILABLE` (no combination offers them). Scanning and connecting never went
through this HAL: wificond and wpa_supplicant talk nl80211 themselves.

Interface objects (`IWifiStaIface`, ...) answer `ERROR_WIFI_IFACE_INVALID` after their
interface is removed or the HAL stops; the chip answers `ERROR_WIFI_CHIP_INVALID` after
`stop`.

Source: `nl80211/` (netlink encoding, wiphy parsing, the generic netlink socket),
`concurrency.rs` (combinations and the room check), `netdev.rs` (ioctls), `chip.rs`,
`iface.rs`, `wifi.rs`. Tests: `android.hardware.wifi-service.pc_host_test` (the whole
crate on the host) and `android.hardware.wifi-service.pc_test` (on the device).

SELinux: domain `hal_pc_wifi` (`pc_hal_service(hal_pc_wifi, hal_wifi)`); the platform's
`hal_wifi` attribute already allows nl80211, `net_admin`, the three interface ioctls and
the `wifi.*` properties, so the domain has no rules of its own. The `.rc` gives the HAL
`CAP_NET_ADMIN` (user `wifi`).

## Combined Cuttlefish test round (F4–F8, 2026-11-12)

One boot of `aosp_pc_x86_64` on Cuttlefish (`aosp/cvd-launch.sh --enable_wifi=true
--enable_host_bluetooth=true`), SELinux enforcing, then a second boot with the camera fixes.
All HAL services run in their `hal_pc_*` domains; no AVC denial from any of them. The only
denials in logcat came from elsewhere: `system_server` (InputReader) reading a sysfs
`country` file and `init` in the pVM guest's console (`zram0`).

| HAL | VTS (pass / skip / fail) | Device unit test | Functional check |
|---|---|---|---|
| health | `VtsHalHealthTargetTest` 25 / 0 / 0 | 8/8 | — |
| thermal | `VtsHalThermalTargetTest` 9 / 2 / 0 | 6/6 | — |
| power | `VtsHalPowerTargetTest` 10 / 13 / 0 | 8/8 | — |
| Bluetooth | `VtsHalBluetoothTargetTest` 16 / 0 / 0 (H4 on `/dev/hvc5`) | 10/10 | stack ON on our HAL |
| audio | `VtsHalAudioCoreTargetTest` (`IModule/default`, framework stopped) 148 / 73 / 0 | 29/29 | `snd-aloop`: `audio-check.sh play 3 440` arrives as 440 Hz on `hw:0,1`; `record` delivers frames |
| camera | `VtsAidlHalCameraProvider_TargetTest` (`pc/0`) 45 / 0 / 0 after the fixes (first run 42 / 0 / 3) | 4/4 | `vivid` hotplug → camera `101` in `dumpsys media.camera` |
| Wi-Fi | `VtsHalWifi{,Chip,StaIface,ApIface}TargetTest` 52 / 38 / 0 | 25/25 | Wi-Fi on, SoftAP on `wlan1`, `wlan0` WPA2 association + DHCP |

Fallbacks kept: none needed. The switches stay: Cuttlefish's camera provider APEX still runs
next to ours (`ICameraProvider/internal/0`), and `PC_WIFI_HAL=legacy` puts Cuttlefish's C++
Wi-Fi HAL back.

Notes: Cuttlefish's own Bluetooth HAL (`bt_hci`) is the one that runs at boot there; ours
runs on Cuttlefish only when started by hand (`stop bt_hci; start
vendor.bluetooth-pc-h4`). `stop` (framework) once made adbd unresponsive until the instance was
restarted; set `vts.native_server.on 1` and run `stop` with a timeout.

## Adding a HAL

1. Create `aosp-device/hals/<name>/` with:
   - `hal.mk`: `PRODUCT_PACKAGES += android.hardware.<name>-service.pc`, plus
     `LOCAL_ENABLE_<X> := false` (or the matching Cuttlefish switch) if Cuttlefish ships
     an implementation of the same instance. Check `device/google/cuttlefish/shared/device.mk`
     and `shared/phone/device_vendor.mk`; if Cuttlefish has no switch, the duplicate VINTF
     entry fails the build (`checkvintf`) and needs a different approach.
   - `Android.bp`: `rust_binary` with `defaults: ["pc_hal_defaults"]`, the AIDL
     `-V<n>-rust` library in `rustlibs`, `init_rc`, and `vintf_fragment_modules` naming a
     `vintf_fragment { vendor: true }` module. Add a `rust_test` with `unit_test: true`.
   - `<service>.rc`: `service vendor.<name>-pc /vendor/bin/hw/<binary>`, `class hal`,
     the least-privileged `user`/`group` that can open its devices.
   - `<service>.xml`: the VINTF `<hal format="aidl">` entry with the frozen version.
   - `sepolicy/file_contexts`: label the binary `hal_pc_<name>_exec`.
   - `sepolicy/hal_pc_<name>.te`: `pc_hal_service(hal_pc_<name>, hal_<name>)` and the
     `allow` rules for its sysfs/devfs types. Device nodes and sysfs paths that only real
     hardware has need their own labels (`genfs_contexts` / `file_contexts` in the same
     directory).
   - `src/main.rs`: `pchal::init_logging`, probe hardware, `pchal::Service::new(...).serve(...)`.
2. Build the binary first (fast feedback, includes Soong analysis):
   `STEPS="android.hardware.<name>-service.pc" tools/aosp17-build.sh`, then the images.
3. Boot and check `service list | grep <name>`, `logcat -s <tag>`, `dmesg | grep avc`.

Nothing else changes: the product makefile picks up `hal.mk`, the board config picks up
`sepolicy/`, and Soong finds `Android.bp` after `tools/aosp-device-link.sh`.

## Build

Rules from `CLAUDE.md` apply: one build at a time, everything through
`tools/limited-build.sh`, images one step at a time.

```
tools/aosp17-build.sh                                          # aosp_pc_x86_64, all steps
STEPS="android.hardware.light-service.pc libpchal_test" tools/aosp17-build.sh
STEPS="vendor.img" tools/aosp17-build.sh
PRODUCT=aosp_cf_x86_64_only_phone tools/aosp17-build.sh        # stock Cuttlefish product
```

`PRODUCT` defaults to `aosp_pc_x86_64`; the script derives the product out directory from
`lunch`. Run the host tests with
`aosp/out/host/linux-x86/testcases/libpchal_test/x86_64/libpchal_test`.

## Boot

`aosp/cvd-launch.sh` (local, gitignored) boots `out/target/product/pc_x86_64` by default (`DEVICE=vsoc_x86_64_only`
for the stock product). Checks:

```
ADB="aosp/out/host/linux-x86/bin/adb -s 0.0.0.0:6520"
$ADB shell getprop sys.boot_completed
$ADB shell service list | grep light
$ADB shell dumpsys -l | grep light
$ADB logcat -d -s android.hardware.light-service.pc
$ADB shell vintf dm | grep -i light       # device manifest entry
$ADB logcat -d | grep -iE "vintf.*(error|fail)"
```

HALs in `class hal` start before logd, so their first log lines (`found N lights`,
`registered ...`) are usually lost on boot. `stop vendor.light-pc; start vendor.light-pc`
(as root) shows them. Do not run the binary by hand while the service is up: as root it
re-registers the instance, and when it exits the name is gone until the service restarts.

## Notes for F3–F8

- `pchal::devfs::open` returns `pchal::Error`; code that works in `io::Result` wraps it
  with `io::Error::other` (keeps the path in the message).
- Clippy runs with `-D warnings` on every Rust module (`dead_code`, `unused_imports`,
  `double_ended_iterator_last`, ...); keep sysfs parsing in a module without AIDL types and
  give it a `rust_test_host` with that file as crate root (the HAL crates' AIDL libraries
  are not all host-supported).
- AIDL parcelable constants live in the parcelable's module, not on the struct:
  `HealthInfo::{HealthInfo, BATTERY_CHARGE_TIME_TO_FULL_NOW_SECONDS_UNSUPPORTED}`; parcelables
  do not implement `PartialEq`.
- A HAL that needs root-owned sysfs files writable adds lines to
  `hals/power/ueventd.pc.rc` (the only `/odm/etc/ueventd.rc`) for device attributes, or
  `chown`/`chmod` in its `.rc` for files outside any device (ueventd never sees them).
- Probe again when class directories change: vendor modules load in parallel with
  `class hal`, so a HAL may start before its devices exist.
- `out/target/product/pc_x86_64/installed-files-*.txt` are stale in this Soong-only build;
  check an image with `fsck.erofs --extract=DIR vendor.img`.

- The product out directory is now `aosp/out/target/product/pc_x86_64`. The
  `tools/aosp-*-test.py` scripts still read `vsoc_x86_64_only` (F3 should switch them).
- Real-PC sysfs paths (`/sys/devices/...`) are labelled `sysfs`, not `sysfs_leds` or other
  specific types; vendor domains may not write plain `sysfs`. Each HAL that targets real
  hardware adds `genfscon` labels for its device paths in its `sepolicy/` directory.
- Rust HALs link `libbinder_rs` (vendor variant), so lazy services and `check_service`
  work but the system-only `check_service_access` does not.
- A HAL that needs a crate only `//vendor` may use puts a re-export in
  `aosp-device-vendor/` (as `libpcfmq`); its own code stays in `hals/`.
- After a compile error `tools/aosp17-build.sh` retries 5 times. For quick iterations, take
  the failed `rustc`/`clippy-driver` command from `aosp/out/siso_failed_commands.sh`, re-run
  `tools/aosp-device-link.sh` and that command alone.
- A new `rust_test_host` module added to an existing directory once failed with "missing
  and no known rule to make it" for its `.config` (incremental Soong analysis); unit tests
  that the device `rust_test` already runs do not need a host twin.
- `tools/aosp17-build.sh` waits for other builds; its Gradle check matches only a real
  `java ... GradleDaemon` process, not shells whose command line mentions the name.
