# Desktop mode and the 2560x1080 landscape display (F9f, H4)

The PC is driven with a mouse and keyboard on a 2560x1080 ultrawide monitor, so
`aosp_pc_x86_64` boots into AOSP 17's own desktop windowing on its primary display,
locked to landscape. The window manager draws the window captions. The default launcher is
Smart Dock (H4, below); AOSP's Launcher3 (Quickstep) stays installed as the recents
provider and as a second launcher.

Everything is in `aosp-device/desktop/` (included by `aosp_pc_x86_64.mk`), and the boot
resolution is a `tools/mkusb-aosp17.sh` default.

## Desktop windowing

| Piece | Where | Effect |
|---|---|---|
| `android.software.freeform_window_management` | `/vendor/etc/permissions` | WindowManager allows `WINDOWING_MODE_FREEFORM` (without it a freeform display falls back to fullscreen) |
| `config_isDesktopModeSupported`, `config_canInternalDisplayHostDesktops` | framework overlay `desktop/overlay` | WM Shell's `DesktopModeStatus.canEnterDesktopMode()` is true on the internal display; the aconfig flag `enable_desktop_windowing_mode` is already on in `cp2a` |
| `config_enterDesktopByDefaultOnFreeformDisplay` | framework overlay | a display whose task area is freeform is treated as a desktop: apps open in desktop windows |
| `display_settings.xml`, `windowingMode="5"` | `/vendor/etc/display_settings.xml` | WindowManager creates the primary display (`port:0`) with a freeform task area |
| `persist.wm.debug.force_desktop_first_on_default_display_for_testing=true` | `/product/etc/build.prop` | WM Shell keeps the primary display desktop-first (see below) |

In AOSP 17 the WM Shell, not WindowManager, has the last word on the default display's
windowing mode: `DesktopDisplayModeController` makes it desktop-first (freeform) only when
there is a touchpad *and* a physical keyboard (a laptop), an external display, or a laptop
device state, and otherwise switches the task area back to fullscreen ("touch-first"). A
desktop PC has a mouse but no touchpad, so on Cuttlefish the display came up fullscreen
even with the display-settings entry, and apps opened fullscreen. Shell's only switch for
this is the `..._for_testing` property above; it is a `persist.wm.debug.*` property, so it
can also be turned off at run time (`adb shell setprop
persist.wm.debug.force_desktop_first_on_default_display_for_testing false`, then reboot) to
get the touch-first behaviour back. With touch-first, desktop mode still works, but the user
enters it per app (the app handle at the top of a fullscreen app → "Desktop", or dragging a
window).

Using it: apps open as resizable windows with a caption bar (minimize, maximize, close,
and the app-handle menu for fullscreen/split). Launcher3's taskbar along the bottom shows
pinned and running apps; the home screen sits behind the windows. `adb shell dumpsys
activity containers` shows the tasks as `mode=freeform`.

Why not `android.hardware.type.pc`: on a "pc" device WindowManager forces freeform too,
but the feature also changes how the Play Store filters apps and how some apps behave,
which would undo F9's Play Store work.

## Smart Dock, the default launcher (H4)

[Smart Dock](https://github.com/axel358/smartdock) (`cu.axel.smartdock`, GPL-3.0) is a
desktop launcher: a dock with a start menu, pinned and running apps, a notification panel
and a system tray, and apps opened in freeform windows. The user asked for it as the
default launcher on 2026-11-15.

- **APK.** `tools/fetch-apps.sh smartdock` downloads F-Droid's build (1.15.0, version code
  1150), pinned by SHA-256 and checked against the certificate F-Droid signs it with, into
  `aosp/device/android-pc/apps/SmartDock/`; `pc.mk` installs it on `/product` like the
  other fetched apps. F-Droid can update it in place (same signature). Its JNI library is
  stored uncompressed (`extractNativeLibs="false"`), so nothing has to be unpacked.
- **First-boot setup.** Smart Dock's own setup screen asks the user to grant five things by
  hand. `aosp-device/desktop/smartdock-setup.sh` (init service `smartdock_setup` from
  `init.desktop.rc`, on `sys.boot_completed=1`, in the `su` domain like the `pc*.sh`
  helpers) does it instead, once per userdata (marker `/data/misc/smartdock-setup.done`),
  so later user choices stay:
  - allows its notification listener, the `SYSTEM_ALERT_WINDOW` and `GET_USAGE_STATS` app
    ops, and grants `WRITE_SECURE_SETTINGS` (a development permission);
  - makes it the `android.app.role.HOME` holder. Launcher3 and Smart Dock both declare
    their HOME filter at priority 0, so the role controller has no fallback holder, and the
    first boot reaches the HOME intent before the script runs: the "Use Smart Dock as
    Home? Just once / Always" chooser is already up. The script closes it (BACK, only when
    `ResolverActivity` is the resumed activity) and starts HOME again, which now goes to
    the role holder. A `/product/etc/preferred-apps` entry does not help: HOME resolves
    through the role, not PackageManager's preferred activities;
  - turns on its accessibility service `DockService` (the dock itself), last.
- **Script on /product.** Init runs it in `su`, a system (core) domain, and Treble's
  neverallows forbid core domains from executing `/vendor` files, so the script and its
  `.rc` are product modules. `su` is permissive on userdebug builds, which this product
  is; a user build would need a domain of its own.
- **No swipe lock screen.** Smart Dock hides its dock when `DockService` starts while the
  keyguard is up (`dock.isVisible = !keyguardManager.isKeyguardLocked`) and only shows it
  again when the dock is unpinned and pinned, so with Android's default swipe lock screen
  the dock was missing after every boot. The overlay sets
  `config_disableLockscreenByDefault=true`; a PIN or password the user sets later still
  locks the PC (and brings the problem back: unpin and pin the dock with its pin button).
  Turning the lock screen off at run time instead (`locksettings set-disabled true` while
  the keyguard was showing) left the keyguard stuck until the next boot.
- **Launcher3 taskbar crash.** With Smart Dock as home, Launcher3's taskbar (Launcher3 stays
  the recents provider) is recreated on configuration changes, and a bubble bar update
  posted before `onDestroy()` ran `TaskbarNavButtonController.onRecentsButtonLayoutChanged`
  with `mControllers == null` ("Quickstep keeps stopping", twice on the first boot).
  [tools/patches/aosp17-launcher3-taskbar-npe.patch](../tools/patches/aosp17-launcher3-taskbar-npe.patch)
  (`git apply` in `aosp/packages/apps/Launcher3`) returns early there.
- **Going back to Launcher3:** Settings → Apps → Default apps → Home app. Removing the
  marker (`adb root; adb shell rm /data/misc/smartdock-setup.done`) runs the setup again
  on the next boot.

## 2560x1080 landscape

- **Mode.** The display is the UEFI GOP framebuffer (`pcfb`; the RTX 4070 has no usable
  open driver), so the resolution is whatever GOP mode Limine sets before the kernel.
  `tools/mkusb-aosp17.sh` now writes `resolution: 2560x1080x32` into every Limine entry by
  default (the EDID's preferred mode gave 1920x1080 on the first boot). `--resolution WxH`
  picks another mode, `--resolution auto` leaves it to the EDID. Ranchu HWC reads the
  framebuffer size, so SurfaceFlinger and the display manager see 2560x1080 with no other
  setting.
- **Orientation.** A 2560x1080 panel is landscape at rotation 0, its natural orientation.
  `display_settings.xml` locks it there: `userRotationMode="1"` (locked), `userRotation="0"`,
  `fixedToUserRotation="2"` (portrait-only apps are letterboxed instead of turning the
  screen) and `ignoreOrientationRequest="true"`. The overlay also sets
  `config_supportAutoRotation=false` (no accelerometer on a PC; Cuttlefish's virtual one is
  ignored).
- **Density** stays at 240 (`androidboot.lcd_density`, `--density N`): 1707x720 dp,
  `sw720dp`, a large-screen layout. A lower value (e.g. 160) gives more room to windows
  at the cost of smaller text.

## Testing on Cuttlefish

Done on 2026-11-13 (`--display0=width=2560,height=1080,dpi=240 --resume=false`, fresh
userdata): `wm size` 2560x1080, density 240; `DefaultTaskDisplayArea mode=FREEFORM`;
`mRotation=0`, `mFixedToUserRotation=true`; Settings and Chromium open as freeform windows
(`bounds=[358,104][2201,881]`, `[717,219][2560,996]`) with caption bars, the taskbar at the
bottom shows pinned and running apps, Launcher3 and the desktop wallpaper behind; no AVC
denials. Before the property was added the same build came up with a fullscreen task area
and fullscreen apps.

```
aosp/cvd-launch.sh --display0=width=2560,height=1080,dpi=240
adb shell dumpsys window displays | grep -E 'mRotation|mWindowingMode|cur='
adb shell dumpsys activity containers | grep -m5 'mode='
```
