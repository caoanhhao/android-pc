# Rust vendor HALs (phases F2, F4, F5)

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
