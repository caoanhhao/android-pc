# Mouse input, untrusted touches and touch-only games (F9m)

The PC has a mouse and no touchscreen. On the real PC (image `aosp17-usb-f9f.img`, logs in
`out/pclog-f9f/boot-1`, 2026-11-13) Delta Force (`com.garena.game.df`) did not react to
clicks, and the log had 586 `InputDispatcher: Dropping untrusted touch event` lines. This
page explains those lines, the real cause, and the fix: a per-app "mouse as touch" mode in
InputDispatcher.

## How Android delivers mouse clicks

A USB mouse is a `SOURCE_MOUSE` device (`CursorInputMapper`). Moving it sends
`ACTION_HOVER_MOVE` events; pressing a button sends `ACTION_DOWN`, `ACTION_BUTTON_PRESS`,
then `ACTION_MOVE`s while held, `ACTION_BUTTON_RELEASE` and `ACTION_UP`, all with source
`SOURCE_MOUSE` (0x2002) and tool type `TOOL_TYPE_MOUSE`. InputDispatcher routes a
button-down gesture exactly like a finger on a touchscreen: same window lookup, same
"untrusted touch" (occlusion) check. So in the log a mouse click is a "touch".

Java views (`View.onTouchEvent`) handle both sources the same way. Native games often do
not: an engine that reads `AInputEvent` and checks for `AINPUT_SOURCE_TOUCHSCREEN`
(0x1002), or a tool type of `FINGER`, sees a mouse click as something else and drops it.
Unreal (UE4 `GameActivity`, a `NativeActivity`) and Unity games that read `Input.touches`
are in this group.

## What the F9f log shows

The 586 drops come in five bursts, each a single held-button drag (dozens of `ACTION_MOVE`s,
each logged):

| Time | Drops | Touched window | Blocking window |
|---|---|---|---|
| 12:32:05, 12:32:59 | 94 | `DesktopWallpaperActivity` (SystemUI, full screen) | `Task=12` layer, uid 1000 |
| 12:35:01–04 | 436 | Play Store `AssetBrowserActivity` (and the wallpaper) | `Splash Screen com.garena.game.df`, uid 10095 |
| 12:41:55–12:42:31 | 56 | Play Store, Launcher3 | `Task=15`, `Task=17` layers, a dim layer |

None of them is a stuck overlay. Each burst is a click that landed on a window while it
was not taking input:

- **Splash screens.** Delta Force was launched from the Play Store at 12:35:00 into a
  freeform window on top of the Play Store. For 4 s (until "Displayed ...GameActivity
  +4s182ms") the window showed only its starting window. Starting windows are
  `NOT_TOUCHABLE`, so the click went through to the Play Store underneath, and
  InputDispatcher dropped it because a window of another app covered that spot
  (`BLOCK_UNTRUSTED`). This is Android's tapjacking protection working as intended.
- **Why systemui and the game have the same uid (10095).** Since Android 12 the splash
  screen is drawn by WM Shell, which runs in the SystemUI process (`u0a95` = uid 10095,
  `ActivityManager: Start proc 3621:com.android.systemui/u0a95`). The window carries the
  app's package name but SystemUI's uid. Delta Force itself runs as uid 10129. Nothing is
  wrong here.
- **Task layers (`Task=N#id`, uid 1000).** In desktop mode WM Shell fills each freeform
  task's surface with a background colour. SurfaceFlinger reports every layer that draws a
  colour or buffer to InputDispatcher as a `NO_INPUT_CHANNEL` window
  (`RequestedLayerState::needsInputInfo`), owned by system_server, so that it counts for
  occlusion. Such a layer only matters when a click goes *through* the task, i.e. when the
  app's own window is not touchable: 12:32:59 is microG's `LoginActivity` finishing
  (alpha 0.22, `clear-task-all`), 12:41:55 a task fading out (alpha 0). A click on a
  live app window never reaches these layers.
- The F9f desktop mode adds no overlay of its own, and pclog/hdmi-check draw nothing.

After the game was drawn (12:35:04) there are no more drops. `input_interaction` shows
the clicks reaching `com.garena.game.df/...GameActivity` (12:35:32, 12:36:10–12:36:17,
while it had focus), and the game did not react. That is the "source is not a
touchscreen" problem above, not occlusion.

## The fix: mouse as touch, per app

`tools/patches/aosp17-input-mouse-as-touch.patch` (apply in `aosp/frameworks/native`
with `git apply`; part of `libinputflinger`, i.e. `system.img`) changes
`InputDispatcher::enqueueDispatchEntryLocked`. For a window whose app is on the list, a
single-pointer mouse event is rewritten before it is queued for that window:

- `ACTION_DOWN`, `ACTION_MOVE`, `ACTION_UP` (and dispatcher-made `CANCEL`) become
  `SOURCE_TOUCHSCREEN` with `TOOL_TYPE_FINGER`, button state 0 and no cursor position, so
  the app sees a one-finger touch;
- hover (`HOVER_ENTER/MOVE/EXIT`) and `BUTTON_PRESS/RELEASE` are not sent to it (a touch
  has no equivalent);
- the scroll wheel (`ACTION_SCROLL`) stays a mouse event.

Only what that one window receives changes. Window choice, the cursor, the caption bar
and resize handles, other windows and the occlusion checks see the normal mouse stream.
The rewritten events go through the connection's `InputState`, so cancellations the
dispatcher makes later (focus change, window gone) are consistent touches too.

The list is a built-in default plus the property `persist.sys.pc.mouse_as_touch`
(comma-separated; it is a `system_prop`, settable by root):

| Token | Meaning |
|---|---|
| `com.example.game` | add this app |
| `com.example.*` | add every app whose package name starts with `com.example.` |
| `*` | add every app (System UI, Launcher3 and system uids are never converted) |
| `-com.example.game`, `-com.example.*` | remove apps (removals win over additions) |
| `-*` | turn the mode off, defaults included |

Built-in default: Delta Force (`com.garena.game.df`), Liên Quân (`com.garena.game.kgvn`),
CoD Mobile VN (`com.garena.game.codm`), Free Fire and Free Fire MAX
(`com.dts.freefireth`, `com.dts.freefiremax`), PUBG Mobile (`com.tencent.ig`,
`com.vng.pubgmobile`) and Where Winds Meet (`com.netease.yyvn`): touch-only games. Other
apps keep the real mouse, which browsers and desktop-style apps need (hover, right-click
menus, text selection, the pointer icon).

The change takes effect at the next click, no restart needed:

```
adb root
adb shell setprop persist.sys.pc.mouse_as_touch 'com.miHoYo.GenshinImpact,com.supercell.*'
adb shell setprop persist.sys.pc.mouse_as_touch '-com.netease.yyvn'   # keep the mouse for one default
adb shell setprop persist.sys.pc.mouse_as_touch '-*'                  # off
```

On the PC itself there is no root shell for apps (Termux has no `su`; userdebug `su` is
for the adb shell only). Use adb from another machine: Settings → Developer options →
Wireless debugging, `adb pair`/`adb connect` to the PC, then `adb root` (allowed on this
userdebug build) and the `setprop` above.

Why in InputDispatcher and not in the app: the app side (`ViewRootImpl`) has no public
way to change a `MotionEvent`'s tool type while keeping its raw/window transforms, and
the dispatcher already rebuilds `MotionEntry`s per window for split and hover events.
Why a list and not "all games": InputDispatcher only knows a window's package and uid,
not the app's category; asking PackageManager from the dispatch thread would add a
binder call per gesture.

## TikTok: white screen after Google sign-in

- The F9f log has only Play Store metadata lines for `com.ss.android.ugc.trill`; TikTok
  was not started in that boot, so the white screen is **not in the log**.
- The F9d boot (`out/pclog-f9d/boot-1`) shows TikTok without a sign-in: the splash and
  `NewUserJourneyActivity`, then `SplashActivity` (which hosts the feed) resumed for 30 s
  with video decoding (`ByteVC1_dec`, `surfaceview_callback`) and no crash, ANR or
  untrusted-touch drop for TikTok.
- The F9b boot (`docs/arm-translation.md`, F9d section) is the one with a Google sign-in:
  microG's `AuthSignInActivity` returned, then TikTok asked the Play Store for a dynamic
  feature module, got the "get this app from Google Play" dialog (TikTok was installed
  with Aurora Store), and only its web view (`SparkActivity`) came up. That is the most
  likely white screen: a Spark page that needs the missing module, not an input problem.
  The fix stays: install TikTok from the Play Store so its split modules are served.
- If it happens again with a Play-installed TikTok, press **Print Screen** while the white
  screen is up. `pclog.sh` now notices the new screenshot and writes
  `boot-N/snap-K/` with the screenshot plus `dumpsys window windows`, `dumpsys input`
  (focused window, touch state, window list), `dumpsys activity top` (the activity and
  its view tree, which shows whether a `WebView`/`SurfaceView` is in front) and
  `dumpsys webviewupdate` (the WebView provider in use). Up to 10 snaps a boot. The
  periodic `state-*.txt` dumps now also list InputDispatcher's windows.

## Testing on Cuttlefish

Done on 2026-11-13 (`--display0=width=2560,height=1080,dpi=240`, desktop mode, the patched
`system.img`), with a real-looking USB mouse made by `uinput` (`BTN_LEFT` down, a 30 px
drag, up, one wheel step) and with `input mouse tap`. Per-window results from
`InputDispatcherOutboundEvent` logging:

| List | Window | VLC (arm64, via libndk_translation) receives |
|---|---|---|
| off (`-*`) | freeform | `MOUSE` hover enter/move/exit, `DOWN`, `BUTTON_PRESS`, `MOVE`, `BUTTON_RELEASE`, `UP`, `SCROLL` |
| `org.videolan.vlc` | freeform `[358,104][2201,881]` | `TOUCHSCREEN DOWN`, `MOVE`, `UP` (button state 0), `MOUSE SCROLL`; no hover |
| `org.videolan.vlc` | maximized and fullscreen | `TOUCHSCREEN DOWN`, `UP` |
| `*` | fullscreen | `TOUCHSCREEN`; the Taskbar and its overlay (Launcher3) stay `MOUSE` |
| `*,-org.videolan.vlc` | fullscreen | `MOUSE` |
| `org.videolan.*` | fullscreen | `TOUCHSCREEN` |
| empty (defaults only) | fullscreen | `MOUSE` |

Clicks with the mode on worked in the app: NEXT in VLC's onboarding (freeform, window
offset applied correctly) and the window's caption maximize button (a SystemUI caption
window, still a mouse). The `PointerEventDispatcher` spy kept getting the mouse stream.
No untrusted-touch drops, no AVC denials, no crashes. The tool type of the delivered
events is not printed by the dispatcher log; it is set to `FINGER` with the source.

To repeat:

```
aosp/cvd-launch.sh --display0=width=2560,height=1080,dpi=240
adb root; adb shell setprop log.tag.InputDispatcherOutboundEvent DEBUG; adb shell 'stop; start'
adb install out/downloads/arm64-test/vlc/org.videolan.vlc_13070206.apk   # arm64-v8a only
adb shell input mouse tap 1200 500          # VLC in front
adb logcat -d -s InputDispatcher | grep Publishing
```

The test app is F-Droid's VLC 3.7.2 build for `arm64-v8a` (version code 13070206, SHA-256
in `out/downloads/arm64-test/vlc/sums`): it has only arm64 libraries, so it runs through
libndk_translation. (The Termux arm64 build in `out/downloads/arm64-test` cannot be
installed over the preinstalled F-Droid Termux: different signing key.) Without the app
on the list VLC gets `source=MOUSE` with hover events; after
`setprop persist.sys.pc.mouse_as_touch org.videolan.vlc` the same tap arrives as
`source=TOUCHSCREEN`, `toolType=FINGER`, with no hover.
