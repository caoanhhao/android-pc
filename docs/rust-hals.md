# Rust vendor HALs (phase F2)

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
| `pchal::sysfs::{read_string, read, write, entries}` | sysfs attributes: newline stripped, parse via `FromStr`, missing class dir = empty list |
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

- The product out directory is now `aosp/out/target/product/pc_x86_64`. The
  `tools/aosp-*-test.py` scripts still read `vsoc_x86_64_only` (F3 should switch them).
- Real-PC sysfs paths (`/sys/devices/...`) are labelled `sysfs`, not `sysfs_leds` or other
  specific types; vendor domains may not write plain `sysfs`. Each HAL that targets real
  hardware adds `genfscon` labels for its device paths in its `sepolicy/` directory.
- Rust HALs link `libbinder_rs` (vendor variant), so lazy services and `check_service`
  work but the system-only `check_service_access` does not.
