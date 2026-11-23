# Desktop mode and the 2560x1080 landscape display (F9f, H4, I2)

The PC is driven with a mouse and keyboard on a 2560x1080 ultrawide monitor, so
`aosp_pc_x86_64` boots into AOSP 17's own desktop windowing on its primary display,
locked to landscape. The window manager draws the window captions. Since Phase D4 the home
app and recents provider is `HyprLauncher`, part of the Hyprland-style shell (tiling,
workspaces, a top bar, an app launcher and an overview: docs/hypr-desktop.md, "`HyprLauncher`
(D4)"); it replaced Smart Dock (H4) and Launcher3 as recents provider only (I2), whose sections
below are kept for the record of older builds. SystemUI's status bar has no height since D4:
the launcher's bar takes its place.

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

Using it: apps open as resizable windows; since Phase D they are tiled by hyprtile, with a
caption bar only when floating, and HyprLauncher's bar along the top shows workspaces, the
focused window and running apps (H4 to D3: Smart Dock's dock along the bottom; before I2:
Launcher3's taskbar); the wallpaper sits behind the windows. `adb shell dumpsys
activity containers` shows the tasks as `mode=freeform`.

Why not `android.hardware.type.pc`: on a "pc" device WindowManager forces freeform too,
but the feature also changes how the Play Store filters apps and how some apps behave,
which would undo F9's Play Store work.

## Smart Dock, the default launcher (H4) — superseded by HyprLauncher (D4)

Not installed since Phase D4: `aosp-device/hypr/hypr.mk` filters `SmartDock` out and
`desktop.mk` no longer installs `smartdock-setup.sh` and `init.desktop.rc` (the files stay).
On a userdata from an H4..D3 build the HOME role moves from Smart Dock to HyprLauncher at the
first boot of a D4 build (docs/hypr-desktop.md, "HOME on fresh and existing userdata").
`config_disableLockscreenByDefault` stays: a desktop without a swipe lock screen is still wanted.

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
  - makes it the `android.app.role.HOME` holder. Until I2, Launcher3 and Smart Dock both
    declared their HOME filter at priority 0, so the role controller had no fallback
    holder, and the first boot reached the HOME intent before the script ran: the "Use
    Smart Dock as Home? Just once / Always" chooser was already up. The script closes it
    (BACK, only when `ResolverActivity` is the resumed activity) and starts HOME again,
    which then goes to the role holder. Since I2 Smart Dock is the only HOME activity
    (besides Settings' `FallbackHome`), so the chooser no longer comes up and this step is
    a safety net. A `/product/etc/preferred-apps` entry does not help: HOME resolves
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
- **Settings hides the dock.** Settings (and other sensitive screens such as permission
  dialogs) sets `HIDE_NON_SYSTEM_OVERLAY_WINDOWS`, so while one of its windows is visible
  every app overlay, the dock included, is hidden. Until I2 Launcher3's taskbar showed
  instead; since I2 there is no bar while such a window is visible (use its caption bar's
  close button, or Esc/Alt+Tab). The dock comes back when that window closes. This is
  Android's overlay protection, not a bug to fix.
- **Running the setup again:** removing the marker (`adb root; adb shell rm
  /data/misc/smartdock-setup.done`) runs it on the next boot. Launcher3 can no longer be
  chosen as home (I2, below); another launcher installed from F-Droid or Play can, in
  Settings → Apps → Default apps → Home app.

## Launcher3 only as the recents provider (I2) — superseded by HyprLauncher (D4)

Since Phase D4 `HyprLauncher` overrides `Launcher3QuickStep` and is the recents provider
(`config_recentsComponentName` in `hypr_overlay_frameworks_base_core`), so Launcher3 is not in
the image. The two Launcher3 patches stay in `tools/patches/` unused (they only matter for a
build that ships Launcher3); `config_showNavigationBar=false` stays.

On the real PC with the H3 build (`out/pclog-h3/boot-9`), Smart Dock was the HOME role
holder and the resumed home activity, but Launcher3's taskbar (window `Taskbar`, a trusted
overlay owned by `com.android.launcher3`, `[0,963][2560,1080]`) was drawn over the same
strip as Smart Dock's dock (`[0,1024][2560,1080]`), so the old launcher's bar with its
back/home/recents buttons was what the user saw, and Launcher3 was still offered as a
second home app. Removing Launcher3 is not an option: in AOSP 17 the recents screen
(overview) lives in Quickstep (`config_recentsComponentName`), SystemUI has no recents of
its own any more, and Smart Dock's recents button, Alt+Tab and the recents key all end in
Quickstep's `OverviewCommandHelper`. So Launcher3 stays, reduced to recents:

| Piece | Where | Effect |
|---|---|---|
| `android:enabled="false"` on `QuickstepLauncher` | [tools/patches/aosp17-launcher3-recents-only.patch](../tools/patches/aosp17-launcher3-recents-only.patch), `quickstep/AndroidManifest-launcher.xml` | Launcher3's home activity is disabled: Smart Dock is the only HOME candidate (Settings → Default apps and the first-boot chooser no longer list Launcher3) |
| `resolveActivity(..., MATCH_DISABLED_COMPONENTS)` | same patch, `OverviewComponentObserver` | Quickstep looks up its own home activity at start (and dereferenced a null `ResolveInfo` once it was disabled); with the flag it still finds it, sees that the default home is another app and uses its fallback `RecentsActivity` |
| `config_showNavigationBar=false` | RRO `pc_desktop_overlay_frameworks_base_core` (`aosp-device/desktop/rro/`, `/vendor/overlay`, priority 10 over Cuttlefish's phone overlay that sets it to true) | WindowManager reports no navigation bar, SystemUI sets `SYSUI_STATE_NAVIGATION_BAR_DISABLED`, and Launcher3's `TaskbarManager` creates no taskbar; SystemUI draws no navigation bar either. Smart Dock's dock has its own back, home and recents buttons |

Both are part of the OS partitions (`system_ext`, `vendor`), so they apply when the stick is
updated with `tools/update-usb-aosp17.sh`; nothing in userdata has to change. An existing
userdata already has Smart Dock as the HOME role holder (H4's setup script), and Android
rescans the system apps because the build fingerprint changes, picking up the disabled
activity. `git apply` the patch in `aosp/packages/apps/Launcher3`, next to H4's taskbar
NPE patch.

Known limit: in desktop mode Quickstep's fallback `RecentsActivity` opens as a small
freeform window (`bounds=[1074,186][1486,918]`, caption "Quickstep") rather than full
screen. This is how recents already behaved with Smart Dock as home since H4 (Quickstep
was not the default home then either); Smart Dock's dock lists the running apps, which is
the usual way to switch.

Checked on Cuttlefish (2026-11-15, `--display0=width=2560,height=1080,dpi=160
--resume=false`, fresh userdata): `cmd package query-activities -a MAIN -c HOME` lists
only `cu.axel.smartdock/.activities.LauncherActivity` and `com.android.settings.FallbackHome`;
the HOME role holder is `cu.axel.smartdock` and its `LauncherActivity` is resumed after
boot with no chooser; `dumpsys window` has no `Taskbar` or `NavigationBar` window, only
Smart Dock's dock, which shows its back/home/recents buttons; Quickstep's
`OverviewComponentObserver` reports `isDefaultHome=false`, `overviewIntent=...RecentsActivity`;
`KEYCODE_APP_SWITCH` and Smart Dock's recents button both open
`com.android.quickstep.RecentsActivity` (`OverviewCommandHelper` `TOGGLE`); no
Launcher3, Quickstep or Smart Dock entries in the crash log.

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
  `fixedToUserRotation="2"` (an app's orientation request never turns the screen) and
  `ignoreOrientationRequest="true"`. The overlay also sets `config_supportAutoRotation=false`
  (no accelerometer on a PC; Cuttlefish's virtual one is ignored). Portrait-only apps are
  no longer letterboxed either: see "Every app in landscape" below.
- **Density** is 160 since H4 (`androidboot.lcd_density` in the stick's bootconfig,
  `tools/mkusb-aosp17.sh --density N`): 1 dp = 1 px, so the 2560x1080 screen is
  2560x1080 dp (`sw1080dp`), about the size of Windows' 100 % scale. The user asked for a
  smaller, desktop-sized UI for mouse and keyboard rather than touch. Before, 240 gave
  1707x720 dp (`sw720dp`), a touch-tablet size. `--density 240` (or `adb shell wm density
  240` at run time) brings the larger UI back. Checked on Cuttlefish with `dpi=160`:
  Settings, Chromium, Smart Dock's dock and start menu lay out normally.

## Every app in landscape

Every app opens in a desktop window on a desk (tiled by hyprtile) and fills it in landscape,
including apps that declare a portrait-only `screenOrientation` and `resizeableActivity=false`;
the same app in fullscreen ("Android mode": Shell's fullscreen, exit-desktop, `am start
--windowingMode 1`) fills the landscape screen. The display never rotates.

### How

AOSP 17's own "universal resizable" behaviour does it. On a large screen (`sw >= 600dp`)
whose display ignores orientation requests, `ActivityRecord.isUniversalResizeable()` makes
WindowManager ignore an activity's fixed orientation, min/max aspect ratio and
non-resizability: the activity's `overrideOrientation` becomes `UNSPECIFIED`, it counts as
resizable, and it gets no letterbox. Apps that target SDK 36 or higher get it already
(compat change `UNIVERSAL_RESIZABLE_BY_DEFAULT`, 357141415). For every other app it is
the `window_manager` device config `ignore_activity_orientation_request_screens=large`
(`WindowManagerConstants`), which is what we set:

| Piece | Where | Effect |
|---|---|---|
| `device_config put window_manager ignore_activity_orientation_request_screens large` | `landscape-defaults.sh` (`/product/bin`), started by `init.landscape.rc` at `sys.boot_completed=1` | universal resizable for apps that target SDK < 36. Put only while the key is unset, so it is there from the first boot on fresh and existing userdata and survives a device-config reset; WindowManager listens to the key, so it applies without a reboot |
| `settings put system accelerometer_rotation 0`, `user_rotation 0` | same script, once per userdata (marker `/data/misc/pc-landscape-rotation.done`) | rotation locked at 0 on an existing userdata, which had auto-rotation on (`USER_ROTATION_FREE`) from Cuttlefish's SettingsProvider defaults |
| `def_accelerometer_rotation=false`, `def_user_rotation=0` | RRO `pc_desktop_overlay_settings_provider` (`desktop/rro-settings/`, `/vendor/overlay`, priority 11 over Cuttlefish's priority 1, which sets auto-rotation on) | the same on a fresh userdata |
| `ignoreOrientationRequest`, `fixedToUserRotation`, `config_supportAutoRotation=false` | `display_settings.xml`, framework overlay (above) | the display stays at rotation 0 whatever an app asks; `ignoreOrientationRequest` is also what lets universal resizable apply |

No `frameworks/base` change. The script runs in the `su` domain from `/product`, as H4's
`smartdock-setup.sh` did (userdebug only, which this product is).

Where the window's shape comes from: hyprtile gives every tiled window its tile, whatever the
app declares, and the app now fills it. Shell's own placement (`DesktopModeUtils.
calculateInitialBounds`, used when a task opens in a desk or comes back to one) still reads the
manifest's `screenOrientation`, and the task still counts as non-resizable for it
(`Task.updateForceResizeOverrides` asks `isUniversalResizeable()` before the activity is attached
to the display, so it answers false), so it picks a portrait-shaped window (about 318x761 here)
for a portrait app. Only a window that keeps those bounds shows that:

- a window that comes back from fullscreen to its desk kept them until hyprtile forgot its tile
  while fullscreen (`HyprTileController.sync`, docs/hypr-desktop.md, `fullscreen 0`); now it
  takes its tile again, for every app;
- a window that floats from the moment it opens (a Lua window rule, an exec rule) keeps Shell's
  portrait shape; `togglefloating` gives hyprtile's own landscape size (half the width, two
  thirds of the height of the work area).

A tile can be taller than wide (dwindle splits the newest window's tile in turn, so the third
window on a 2560-wide desk gets a narrow column); the app then lays out for that shape, as any
app does there. That is the layout, not the app's request.

What stays as the app asks:

- **Games** (`android:appCategory="game"`, `ApplicationInfo.CATEGORY_GAME`) are never
  universal resizable; a portrait-only game is letterboxed (pillarboxed) in its window.
- An app that sets `android.window.PROPERTY_COMPAT_ALLOW_RESTRICTED_RESIZABILITY` (application
  or activity `<property>`) opts itself out.
- A per-app aspect ratio the user picks in Settings → Apps → the app → Aspect ratio (when that
  page is offered) wins over universal resizable.

### Opting an app out

If an app misbehaves in landscape (crashes, a stretched or sideways camera preview, a layout
it was never tested in), put it on WindowManager's opt-out list (comma-separated package
names, kept in userdata, applies to the app's next launch):

```
adb shell device_config put window_manager opt_out_ignore_activity_orientation_request_list com.example.app,com.example.other
adb shell am force-stop com.example.app
```

That is enough for an app that targets SDK < 36. One that targets 36+ is also universal
resizable through the compat change, so it needs the list *and* a per-app compat override
(allowed on this userdebug build): `adb shell am compat disable UNIVERSAL_RESIZABLE_BY_DEFAULT
com.example.app` (`am compat reset ...` undoes it). Checked with two portrait-only,
non-resizable test apps (target 33 and 36): letterboxed again only with those steps. An
opted-out app keeps its portrait window, letterboxed in a landscape tile; to give it a
floating window instead, add a window rule to `init.lua` (docs/hypr-lua.md):
`hl.window_rule({ match = { class = "^com\\.example\\.app$" }, float = true })`. Turning the whole thing
off is `device_config put window_manager ignore_activity_orientation_request_screens none`
(the script leaves a set value alone).

Apps found to misbehave so far: none.

### Checked on Waydroid (2026-11-23)

Two test APKs made for this (`aapt2` only, no code: `android.app.Activity` with
`screenOrientation="portrait"`, `resizeableActivity="false"`, target SDK 33 and 36), TCInvest
(`com.fss.tcbs.mobiletrading`: portrait, non-resizable, target 36) and Settings. State read with
`dumpsys activity containers` / `activities` (`overrideOrientation`, `areBoundsLetterboxed`) and
`hyprctl clients` inside the container, screenshots with `screencap`.

| Case | Before | After |
|---|---|---|
| target-33 portrait app in a tile (2560-wide desk, 623x500 tile) | letterboxed: a 186x446 portrait window in the middle of the tile, config `port` | fills the tile, `overrideOrientation=UNSPECIFIED`, `land` |
| target-36 portrait app, TCInvest | already universal resizable (compat change): fill their tile | same |
| alone on a desk | — | the whole work area, `[12,45][2526,1046]` |
| `am start --windowingMode 1`, Shell's `moveTaskOutOfDesk` (exit desktop) | pillarboxed portrait | fullscreen `[0,0][2538,1058]`, not letterboxed, rotation 0 |
| `fullscreen 0` and back, Shell's exit-desktop and `moveTaskToDesk` back | back with Shell's bounds: 318x761 (portrait app), 1827x761 (Settings), not tiled | back in its tile (hyprtile fix) |
| `fullscreen 1` (maximize) and back | work area, then the tile | same |
| Settings, Files, Contacts | tiled | unchanged |
| rotation (existing userdata with auto-rotation on) | `USER_ROTATION_FREE`, `accelerometer_rotation=1` | `USER_ROTATION_LOCKED`, 0, rotation 0 |
| desktop-first | `DefaultTaskDisplayArea mode=FREEFORM`, `force_desktop_first...=true` | unchanged |

Opt-out (list, and list + `am compat disable` for target 36) gave the letterboxed portrait window
back, as described above.

## Testing on Cuttlefish

Done on 2026-11-13 (`--display0=width=2560,height=1080,dpi=240 --resume=false`, fresh
userdata): `wm size` 2560x1080, density 240; `DefaultTaskDisplayArea mode=FREEFORM`;
`mRotation=0`, `mFixedToUserRotation=true`; Settings and Chromium open as freeform windows
(`bounds=[358,104][2201,881]`, `[717,219][2560,996]`) with caption bars, the taskbar at the
bottom shows pinned and running apps, Launcher3 and the desktop wallpaper behind; no AVC
denials. Before the property was added the same build came up with a fullscreen task area
and fullscreen apps.

```
aosp/cvd-launch.sh --display0=width=2560,height=1080,dpi=160
adb shell dumpsys window displays | grep -E 'mRotation|mWindowingMode|cur='
adb shell dumpsys activity containers | grep -m5 'mode='
```
