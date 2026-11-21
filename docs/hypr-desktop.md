# A Hyprland-style desktop shell (Phase D)

Status (2026-11-21): **Phase D done** (D0-D7). The shell is part of the `aosp_pc_x86_64`
image, for the USB stick and Waydroid alike: `PcSystemUI` replaces SystemUI and hosts the
`hyprtile` layout engine in WM Shell (dwindle and master tiling, workspaces 1-9 on Shell's
desks, Hyprland's dispatchers and keybinds, borders, corners, bezier animations, mouse binds
and touch gestures), and `HyprLauncher` replaces Smart Dock and Launcher3 as home and recents
provider (bar, app launcher, overview, the Lua configuration). It was checked end to end on
Waydroid with the full image (D7, "Waydroid and wrap-up (D7)"); "As built" sums up the
pieces and "Known limits" collects what is left. Sub-phase notes: "`PcSystemUI` skeleton
(D0)", "Shell extension points (D0)", "`hyprtile` and dynamic tiling (D1)", "Workspaces and
keybinds (D2)", "Decoration and animation (D3)", "`HyprLauncher` (D4)", "Lua configuration
(D5)" (reference docs/hypr-lua.md), "Touch gestures (D6)". This note also records what was
decided in the brainstorm that started in `~/projects/hyprland-android` (an empty repo, now
folded into this one) and what AOSP 17 already offers. Plan and sub-phases: PLAN.md, "Phase D".

## Goal

A desktop shell for `aosp_pc_x86_64` that looks and works like Hyprland: dynamic tiling
(dwindle and master layouts), workspaces, gaps, rounded corners, gradient active borders,
bezier animations, keybinds, a waybar-like bar and a rofi-like app launcher, configured with a
hot-reloaded Lua file. Mouse and keyboard first, with simple touch gestures in the style of
HarmonyOS's PC mode. It replaces Smart Dock and Launcher3 (docs/desktop-mode.md).

## Decisions (user, 2026-11-21)

- **Desktop mode only** (2560x1080, mouse and keyboard). Touch is a secondary input with a few
  gestures.
- **No Shizuku, no root tricks.** The shell is part of the ROM: the device owner can change
  WM Shell, so the shell works through Shell's own APIs instead of hidden-API calls from an app.
- **Fixed to this tree's AOSP 17** (`android-17.0.0_r1`, the build that also runs in Waydroid,
  Phase W). Porting to another AOSP release means porting the Shell module again.
- **Follow Google's pattern for a SystemUI variant**, the one Android Automotive uses
  (CarSystemUI), rather than patching WM Shell in `frameworks/base`.
- **The project lives in android-pc**, not in a repo of its own, since it only works on a ROM
  that ships it.

## What the shell is

Not a launcher: it is the window-management part of SystemUI plus a launcher. Compared with a
Linux desktop:

| Layer | Linux + Hyprland | AOSP 17 | Here |
|---|---|---|---|
| Compositing | Hyprland (Aquamarine) | SurfaceFlinger | unchanged |
| Window policy core (z-order, focus, lifecycle) | Hyprland | WindowManagerService | unchanged |
| Window management UX (layout, workspaces, animations, borders, keybinds) | Hyprland | WM Shell, inside the SystemUI process | **new: `hyprtile` in a Shell module** |
| Bar | waybar | status bar, Launcher3 taskbar | **new, in the launcher** |
| App launcher, overview | rofi | Launcher3 / Quickstep | **new, in the launcher** |
| Notifications, quick settings, keyguard, volume | mako, hyprlock, ... | SystemUI | unchanged |
| Configuration | `hyprland.conf` / Lua | none | **new: Lua, in the launcher** |

## What AOSP 17's WM Shell already has

Paths under `aosp/frameworks/base/libs/WindowManager/Shell/src/com/android/wm/shell/`:

| Hyprland idea | Shell piece |
|---|---|
| Workspaces | `desktopmode/multidesks/` (`DesksController`, `DeskSwitchTransitionHandler`); `IDesktopMode` has `createDesk`, `activateDesk`, `removeDesk` |
| Keybinds | `desktopmode/DesktopModeKeyGestureHandler.kt`: snap left/right, maximize, minimize, previous/next desk, quit, move to next display, through InputManager's `KeyGestureEvent` |
| Tiling | `windowdecor/tiling/`: two windows with a divider only, no layout tree |
| Hiding windows | `DesktopMinimizationTransitionHandler`, `minimizeDesktopApp` |
| Borders, corners, captions | `windowdecor/DesktopModeWindowDecoration`, drawn with SurfaceControl |
| Animations | Shell transitions animate real app windows |
| Variants | `dagger/WMShellModule`, `TvWMShellModule`, `DynamicOverride` (for "specialized modules"), `UsedDownstream` |

## Architecture

### `PcSystemUI`: a SystemUI variant (system_ext)

Built like `aosp/packages/apps/Car/SystemUI`: its `Android.bp` has `overrides: ["SystemUI"]`,
`system_ext_specific`, `certificate: "platform"`, `privileged`, and it swaps parts of SystemUI
through Dagger (`CarSystemUIInitializer` extends `SystemUIInitializer`; `CarWMComponent` and
`CarWMShellModule` replace the default Shell module). `PcSystemUI` does the same with a
`PcWMShellModule` that provides:

- **`hyprtile`**, the layout engine: one layout tree (dwindle or master) per desk. Task opened,
  closed, moved to another desk, dragged or resized → new bounds, applied with a
  `WindowContainerTransaction` inside a Shell transition, so app windows animate.
- its own window placement for desktop tasks, in place of `DesktopTasksController`'s default;
- a `WindowDecorViewModel` that draws gaps, rounded corners, the gradient active border and a
  minimal caption (or none);
- keybinds from the configuration, through `KeyGestureEvent`;
- edge-swipe and pinch gestures for touch (`HyprTouch`, D6).

`hyprtile` takes only declarative configuration (gaps, colours, beziers, layout, rules). Lua
never runs in the SystemUI process, so a broken script cannot take SystemUI down. The layout
engine is plain Kotlin with unit tests, independent of Shell. Changes to `frameworks/base`
should be limited to opening extension points (visibility, a hook) where Shell has none.

### `HyprLauncher`: home, recents provider, bar, Lua (system_ext)

- The HOME activity and the **recents provider** (`config_recentsComponentName` in an RRO),
  replacing both Smart Dock and Launcher3/Quickstep. SystemUI binds the recents provider's
  service and hands it Shell's interfaces through `ExternalInterfaceBinder`, the way
  Quickstep gets `IDesktopMode`; `IHyprShell` is added to that same set, so no custom
  permission is needed.
- The bar (workspaces, running apps, clock, tray; modules scriptable in Lua), the rofi-like
  app launcher, the overview, the configuration error screen.
- The Lua runtime: reads `init.lua`, turns it into a Parcelable configuration for
  `hyprtile`, reloads on save (`FileObserver`), keeps the last good configuration on errors.
  Event hooks (`hl.on("window.open", ...)`) run here, asynchronously. Engine: the tree's
  `external/lua` (5.4) with a small JNI binding (D5); `os`, `io`, `debug` and `package` are
  not exposed.

### `IHyprShell`

`setConfig(config)`, `dispatch(name, args)` (Hyprland-style dispatchers: `movefocus`,
`movewindow`, `workspace`, `togglefloating`, `killactive`, ...) and a listener for window,
workspace and focus events.

### Partitions

| Piece | Where |
|---|---|
| `PcSystemUI`, `HyprLauncher` | `system_ext`, privileged, platform key |
| `privapp-permissions` allowlist | `system_ext/etc/permissions` |
| Desktop defaults, status bar off, `config_recentsComponentName` | RRO (`product` or `vendor`) |
| Feature switches | aconfig flags, e.g. `enable_dwindle` |
| `system` | stays generic |

Device-tree home: a new `aosp-device/hypr/` (`hypr.mk`, included by `aosp_pc_x86_64.mk` in
place of the Smart Dock pieces of `desktop.mk`), with `PcSystemUI/`, `HyprLauncher/`,
`hyprtile/` and the RROs.

## `PcSystemUI` skeleton (D0)

`aosp-device/hypr/PcSystemUI/`, added to the product by `hypr/hypr.mk` (included from
`aosp_pc_x86_64.mk`; Smart Dock and Launcher3 stay as they are):

| File | Role | AOSP counterpart |
|---|---|---|
| `Android.bp` | `PcSystemUI-application` (Dagger graph, KSP, same flags) + `android_app PcSystemUI` (`overrides: ["SystemUI"]`, `system_ext_specific`, `privileged`, platform key) | `SystemUI-application` + `SystemUI` |
| `AndroidManifest.xml` | `tools:replace="android:appComponentFactory"` → `PcSystemUIAppComponentFactory` | `.PhoneSystemUIAppComponentFactory` |
| `PcSystemUIAppComponentFactory.kt` | creates `PcSystemUIInitializer` | `PhoneSystemUIAppComponentFactory` |
| `PcSystemUIInitializer.kt` | returns `DaggerPcGlobalRootComponent.builder()` | `SystemUIInitializerImpl`, `CarSystemUIInitializer` |
| `PcGlobalRootComponent.java` | root component, `getWMComponentBuilder()` → `PcWMComponent.Builder` | `ReferenceGlobalRootComponent`, `CarGlobalRootComponent` |
| `PcSysUIComponent.java` | SysUI subcomponent, same module list as the reference | `ReferenceSysUIComponent` |
| `wmshell/PcWMComponent.java` | Shell subcomponent on `PcWMShellModule` | `WMComponent`, `CarWMComponent`, `TvWMComponent` |
| `wmshell/PcWMShellModule.java` | D0: `@Module(includes = WMShellModule.class)`, nothing else | `WMShellModule`, `CarWMShellModule`, `TvWMShellModule` |

How SystemUI picks its variant: there is no resource or property. The APK's manifest names an
`appComponentFactory` (a `SystemUIAppComponentFactoryBase`), whose
`createSystemUIInitializer()` returns the `SystemUIInitializer`; the initializer's
`getGlobalRootComponentBuilder()` returns the root Dagger component, and the root component's
`getWMComponentBuilder()` return type decides which Shell subcomponent (and so which Shell
module) is built. `SystemUIInitializer.init()` then hands the subcomponent's `WMComponent`
getters (`getShell()`, `getDesktopMode()`, ...) to the SysUI subcomponent. A variant can grab
its own Shell objects through extra getters on its `WMComponent` and an override of
`prepareSysUIComponentBuilder()` (CarSystemUI calls `initWmComponents()` there to create its
controllers eagerly). The variant APK keeps the package name `com.android.systemui` and the
`android.uid.systemui` shared UID, and `overrides` keeps AOSP's SystemUI out of the image.

Checked on Cuttlefish (2026-11-21, `--display0=width=2560,height=1080,dpi=160`, fresh
userdata): `pm path com.android.systemui` is `/system_ext/priv-app/PcSystemUI/PcSystemUI.apk`,
the image has no `SystemUI` any more, a heap dump of the SystemUI process contains
`DaggerPcGlobalRootComponent`, Settings and Clock open freeform with Shell's caption, Smart
Dock's bar shows (hidden while Settings is in front, as before: docs/desktop-mode.md), and the
crash buffer is empty (the only ANRs are the known `com.android.phone` ones of Cuttlefish
without a modem).

Not checked in Waydroid in D0 (the user allowed Waydroid tests for Phase D after D0 was
verified; D1 did it through the overlay, D7 with the full image). Trying `PcSystemUI` there needs the overlay to add
`/system_ext/priv-app/PcSystemUI` and hide `/system_ext/priv-app/SystemUI` (an overlayfs
whiteout), or a repacked image (docs/waydroid-aosp17.md); the next sub-phase that boots
Waydroid does it.

Build note: SystemUI's keep rules must be passed in `proguard_flags_files`
(`:sysui_proguard_common_and_kotlin`). In `included_proguard_flags_files` they are only
dependencies for a flags file that `-include`s them, so R8 dropped
`com.android.systemui.VendorServices` (created by reflection from
`config_systemUIVendorServiceComponent`) and SystemUI crash-looped with
`ClassNotFoundException`.

## Shell extension points (D0)

How Shell's Dagger graph is built (`frameworks/base/libs/WindowManager/Shell/src/com/android/wm/shell/dagger/`):

- `WMShellBaseModule` holds what every form factor shares and declares the variable pieces as
  `@BindsOptionalOf @DynamicOverride` (`FullscreenTaskListener`, `FreeformComponents`,
  `SplitScreenController`, `MixedTransitionHandler`, `StartingWindowTypeAlgorithm`,
  `DesktopMode`, `DesktopTasksController`, `DesktopUserRepositories`, `DesksOrganizer`,
  `PackageUpdateController`, `InteractiveTasksTransitionObserver`, ...), plus
  `Optional<X>` providers that return them only when the feature is on.
- A product module supplies those `@DynamicOverride` bindings: `WMShellModule` (handheld,
  includes `WMShellBaseModule`, `PipModule`, `ShellBackAnimationModule`, `LetterboxModule`,
  `PinnedLayerModule`, `DesktopModule`, `HandheldContainersModule`, `BubbleModule`),
  `TvWMShellModule` (TvSystemUI), `CarWMShellModule` (CarSystemUI, includes
  `WMShellBaseModule` and its own automotive modules, not `WMShellModule`).
- `UsedDownstream` only marks code used by such downstream variants.
- Dagger cannot override a binding: a second `@Provides` for the same key is a duplicate-binding
  error. So the `@DynamicOverride` slots are override points only for a module that does **not**
  include `WMShellModule`. A variant that includes `WMShellModule` (D0) can only add bindings.

What this means for `PcWMShellModule`:

| Piece | Where it is bound | Replaceable from `PcSystemUI`? |
|---|---|---|
| Any new object (hyprtile controller, a `TransitionHandler`, a `TransitionObserver`, an `ITransitionPlanner`, key gesture handler, desk listeners) | new `@Provides` in `PcWMShellModule`, created eagerly from `PcWMComponent` | **Yes**, additive, D0 layout works as is |
| `DesktopTasksController` (desktop task placement: `handleRequest` → `getInitialBounds` → `calculateInitialBounds` in `DesktopModeUtils.kt`) | `@DynamicOverride` in `WMShellModule` | Binding: yes, with a forked module (below). Behaviour: **no**, it is a final Kotlin class (~7000 lines, 60 constructor arguments) and placement is in final methods, so a replacement is a full copy of the class |
| `WindowDecorViewModel` (captions, borders, corners) | plain `@Provides` in `WMShellModule` (returns `DesktopModeWindowDecorViewModel` when desktop mode is on, else `CaptionWindowDecorViewModel`) | **Yes** with a forked module: `WindowDecorViewModel` is a public interface; `DesktopModeWindowDecorViewModel` is a public, non-final Java class that can be wrapped or subclassed |
| `DesktopModeWindowDecorViewModel` itself, `AppToWebRepository`, `DesktopModeKeyGestureHandler` | plain `@Provides` in `WMShellModule` | Yes with a forked module; the key gesture handler is final, but a second handler can register its own `KeyGestureEvent`s additively |
| `DesksController` (desk bookkeeping) | `DesktopModule` (included by `WMShellModule`) | Yes with a forked module that leaves `DesktopModule` out and copies it; final Kotlin class |
| `DeskSwitchTransitionHandler` (desk switch animation) | plain `@Provides` in `WMShellModule` | Binding yes with a forked module; final Kotlin class, so a new handler, not a subclass. Easier: a higher-priority `TransitionHandler` added after it (`Transitions` asks handlers newest first) that claims desk-switch transitions |
| `DesksOrganizer` (desk roots in WM) | `@DynamicOverride` in `WMShellModule` | Yes with a forked module; `DesksOrganizer` is an interface |
| `FreeformComponents`, `MixedTransitionHandler`, `SplitScreenController` | `@DynamicOverride` in `WMShellModule` | Yes with a forked module |

"Forked module" = `PcWMShellModule` stops including `WMShellModule`, includes
`WMShellBaseModule` and `WMShellModule`'s other includes (or copies of them), and carries a copy
of `WMShellModule`'s ~2400 lines of bindings with the few it replaces changed. That is what
CarSystemUI does, it needs no change in `frameworks/base` (everything `WMShellModule` calls is
public; Kotlin `internal` classes are public in bytecode), and it stays correct because the tree
is fixed to `android-17.0.0_r1`. The cost is that the copy must be refreshed by hand on an AOSP
update.

Runtime hooks Shell already offers without replacing anything (enough for D1):

- `Transitions.addHandler()`: handlers are asked in reverse order of registration, so a handler
  added at `ShellInit` after Shell's own wins `handleRequest`/`startAnimation` for the
  transitions it claims (e.g. task open and close in a desk), and can return a
  `WindowContainerTransaction` with tiled bounds.
- `Transitions.registerObserver()` (task open/close/move seen by everyone) and
  `Transitions.addPlanner()` (`ITransitionPlanner`, newest first) for animation plans.
- `DesktopTasksController.addVisibleTasksListener`, `addDeskChangeListener`, and the public
  `moveTaskToDesk`, `minimizeTask`, `activateDesk`, `createDesk`, `removeDesk`; `IDesktopMode`
  for the launcher side.
- Not available: a second `ShellTaskOrganizer` listener for the freeform type (one listener per
  type, `addListenerForType` throws on a second one).

Where `frameworks/base` would need an extension point (a small patch in
`libs/WindowManager/Shell`, kept in `tools/patches/`):

- **Initial placement of a desktop task.** `getInitialBounds()` is final and applies remembered
  bounds, cascading and gravity. Without a hook, hyprtile re-tiles right after the open
  transition (a visible second move) or claims the open transition itself and loses Shell's
  desk bookkeeping for it. The clean fix is an optional `DesktopTaskPlacementPolicy` interface
  (`@BindsOptionalOf` in `WMShellBaseModule`, consulted first by `getInitialBounds()` and the
  re-layout paths on close/minimize).
- **Turning off Shell's own snapping and two-window tiling** (`windowdecor/tiling/`,
  `SnapController`, drag-to-snap visual indicator) where they fight hyprtile: today this is
  flag-driven (`DesktopExperienceFlags`), and per-feature switches are not all exposed; a forked
  module can rebind `DesktopTilingDecorViewModel`, but `DesktopModeWindowDecorViewModel` takes
  it as a concrete class, so dropping it means a decoration view model of our own (D3).
- **Desk switch animation** if the higher-priority handler turns out to race
  `DesktopTasksController`'s own desk-activation transition (to be checked in D2).

Plan from here: D1 adds a hyprtile controller additively (handler + observer, D0 layout) and
first tries placement without a patch; if the second move is visible, it adds the placement
hook. D3 is the first sub-phase that needs the forked module (its own `WindowDecorViewModel`).
D1 needed no hook (below, "`hyprtile` and dynamic tiling (D1)").

## `hyprtile` and dynamic tiling (D1)

### The engine

`aosp-device/hypr/hyprtile/`, plain Kotlin with no Android types (`java_library hyprtile`,
host and device), package `com.android.hyprtile`:

| File | What |
|---|---|
| `Geometry.kt` | `Box` (a Rect with exclusive right/bottom), `Insets`, `applyGaps` |
| `TileConfig.kt` | the declarative configuration: `layout`, `gapsIn`, `gapsOut`, `reserved` (bar space), dwindle's `splitWidthMultiplier`, `defaultSplitRatio`, `preserveSplit`, `forceSplit`, master's `masterFactor`, `newIsMaster`, `newOnTop`, `floatPackages` |
| `DwindleLayout.kt` | Hyprland's dwindle: a binary tree; a new window splits the target window's box (the focused one, else the newest); top/bottom when `height * split_width_multiplier > width`, else side by side, recomputed on every arrange unless `preserve_split`; closing gives the box to the sibling; `togglesplit`, `swap`, `splitratio` |
| `MasterLayout.kt` | Hyprland's master, orientation left: master takes `mfact` of the width, the stack shares the rest in rows; closing the master promotes the top of the stack |
| `HyprTile.kt` | one layout per workspace (an AOSP desk), floating windows, `open`/`close`/`toggleFloating`/`swap`/`arrange`; switching the layout kind keeps the windows |
| `FloatRules.kt` | `shouldFloat`: not resizeable, translucent, dialog-themed (`windowIsFloating`), listed in `floatPackages`, or a minimum size larger than the work area |

Gaps follow Hyprland: the work area is the display's stable bounds minus `reserved`; a window
side on the work area's edge gets `gaps_out`, an inner side `gaps_in`, so neighbours are
`2 * gaps_in` apart. Split positions are rounded once and shared by both children, so tiles
never overlap and never leave a stray pixel.

Tests: 32 JUnit tests in `hyprtile/tests/` (exact boxes on 2560x1080 and 1920x1080, ratios,
preserve/toggle split, swap, workspaces, floating, layout switch, float rules, and 2000
random open/close steps that check the tiles stay inside the area and never overlap).
`aosp-device/hypr/hyprtile/run-tests.sh` compiles and runs them with the tree's kotlinc and
JDK in a few seconds without Soong; under Soong they are `java_test_host hyprtile-tests`
(`m hyprtile-tests`, or `atest hyprtile-tests`). Both pass.

### Wiring into Shell (no `frameworks/base` change)

`PcSystemUI/src/com/android/systemui/wmshell/HyprTileController.kt`, provided by
`PcWMShellModule` and created by `PcWMComponent.init()` after `getShell()` (so that its
ShellInit callback runs after Shell's own and its handler is the newest). It uses only the
runtime hooks D0 listed:

- **Request path.** As the newest `Transitions.TransitionHandler` it sees every
  `TRANSIT_OPEN`/`TO_FRONT`/`CLOSE`/`TO_BACK` request of a standard task on a display with an
  active desk first. It lets the other handlers answer (`Transitions.dispatchRequest(...,
  skip = this)`, so `DesktopTasksController` still does all of its desk bookkeeping), updates
  hyprtile, and adds `setBounds` for every tiled task whose tile moved to *their*
  `WindowContainerTransaction`; it returns their `RequestResult` unchanged otherwise, so the
  handler that would have animated still animates. The new window and its neighbours move in
  one Shell transition, with no second move. It never animates itself.
  Mixed handlers dispatch the request again from inside their own `handleRequest`, which comes
  back to this handler; the nested call steps aside (found on Waydroid: without the guard the
  request was handled twice). A task moved into a desk gets windowing mode `UNDEFINED` in the
  WCT (it inherits `FREEFORM` from the desk root); that counts as freeform.
- **Observer path.** As a `TransitionObserver`, 100 ms after any transition finishes (the
  repository learns about tasks from task-info callbacks that can come just after), it
  compares the active desk's expanded tasks (`DesktopRepository.getExpandedTasksIdsInDeskOrdered`,
  closing tasks and non-freeform tasks left out) with the layout and, if they differ, starts
  one `TRANSIT_CHANGE` with the new bounds. This catches what comes without a request:
  Shell's own minimize and close buttons, `moveTaskToDesk`, desks restored at boot, and the
  first window of a desk (the desk becomes active only with that transition, so the request
  path sees no active desk yet).
- Bounds are written only when a task's tile changes, so a window the user drags or resizes
  stays where it was put until the next layout change (and the observer cannot loop on an
  app that refuses a size).
- Floating: decided when the task first appears, and checked again on every sync while it
  floats, because a task can open behind a translucent activity (Contacts' permission dialog)
  and then become a normal window. Floating tasks keep Shell's own placement.
- Configuration until D5's Lua: system properties, read on each request and sync:
  `persist.hypr.tiling` (default true; false turns tiling off), `persist.hypr.layout`
  (`dwindle`/`master`), `persist.hypr.gaps_in` (5), `persist.hypr.gaps_out` (12),
  `persist.hypr.reserved_top`/`reserved_bottom` (0), `persist.hypr.float_packages`
  (comma-separated). The work area is `DisplayLayout.getStableBoundsForDesktopMode()` (status
  bar and taskbar already excluded).

Decided: no placement hook in `frameworks/base` for D1. The request path puts the new window
straight into its tile inside the open transition, so the extension point D0 anticipated
(`DesktopTaskPlacementPolicy`) is not needed; `tools/patches/` gets nothing.

### Checked on Waydroid (2026-11-21)

Waydroid replaced Cuttlefish as the test target (user, 2026-11-21). `PcSystemUI.apk` went into
the AOSP 17 image through the overlay, without repacking:
`/var/lib/waydroid/overlay/system_ext/priv-app/PcSystemUI/PcSystemUI.apk` plus an overlayfs
whiteout (`mknod c 0 0`) at `/var/lib/waydroid/overlay/system_ext/priv-app/SystemUI`, which
hides the image's SystemUI (removing both entries brings it back). `pm path com.android.systemui`
is then the overlay's APK. Tested on an empty data directory
(`XDG_DATA_HOME=~/.local/share/waydroid-test`), since the user's data sits at a locked
keyguard (`RUNNING_LOCKED`, a lock credential), with the Waydroid window alone on host
workspace 4 (the user's data also had `persist.waydroid.width` at 394, saved while the
window was tiled narrow on the host; it is back at 2560): display 2538x1058, density 180 (Waydroid's; the 2560x1080/160 of the plan is
Cuttlefish's). Stable bounds are `[0,27][2538,995]`, so with `gaps_out` 12 the work area is
`[12,39][2526,983]`.

Opening Settings, Contacts, Calendar (which also opened microG's sign-in activity, a task of
its own), Files and Clock one after another, then closing two and minimizing one
(`adb`-equivalent: `waydroid shell`, `dumpsys activity containers`, `screencap -p`):

| Step | Tiled task bounds |
|---|---|
| Settings | `[12,39][2526,983]` |
| + Contacts | Settings `[12,39][1264,983]`, Contacts `[1274,39][2526,983]` |
| + Calendar, microG sign-in | Contacts `[1274,39][1899,983]`, Calendar `[1909,39][2526,506]`, microG `[1909,516][2526,983]` (the right half is still wider than tall, so it split side by side, then top/bottom) |
| + Files | microG `[1909,516][2216,983]`, Files `[2226,516][2526,983]` |
| + Clock | Files `[2226,516][2526,763]`, Clock `[2226,758][2526,1005]` (Clock's minimum height, see below) |
| Files closed (`am force-stop`) | Clock takes Files' box, `[2226,516][2526,983]`, in the close transition |
| Calendar closed | the right column re-tiles: microG `[1909,39][2526,506]`, Clock `[1909,516][2526,983]` |
| Settings minimized (caption button) | Contacts `[12,39][1264,983]`, microG `[1274,39][1899,983]`, Clock `[1909,39][2526,983]` |
| Settings brought back (`am start`) | tiled again next to the focused Clock: `[1909,516][2526,983]` |
| `persist.hypr.layout=master`, Messaging opened | master `[12,39][1391,983]` (mfact 0.55), four stack rows on the right |
| `dwindle`, gaps 0, Messaging closed | `[0,27][1269,995]`, `[1269,27][1904,995]`, ... (exact halves, no gaps) |

From the second window on, the new window lands in its tile inside its own open transition
and the neighbours move in the same transition; closes from the app side (`force-stop`) go
the same way. The first window of a desk, the caption's minimize and close buttons go through
the observer (one `TRANSIT_CHANGE` right after). Contacts' notification-permission dialog,
which opens on top of the tiled Contacts task, does not make it float. No SystemUI crash.
Screenshots: `out/hypr-d1/` (`open1..5.png`, `close1..2.png`, `min1.png`, `restore1.png`,
`master.png`).

Bugs found on the way and fixed: the request was handled twice (mixed handlers dispatch it
again), a task entering a desk has mode `UNDEFINED` in the WCT, a new task is not known to
`ShellTaskOrganizer` at request time (its bounds were skipped and Shell's default
`[355,128][2182,889]` stayed; the request's own `TaskInfo` is used now), and a permission
dialog on top of a tiled task floated the whole task.

Known limits (left for later sub-phases):

- An app's minimum size wins over its tile: DeskClock asks for more height than a quarter
  tile, so WindowManager makes it taller and it overlaps its neighbour by a few pixels at the
  bottom. A layout that knows minimum sizes (or floats such a window) is a later refinement.
- A window the user maximizes, drags or resizes keeps its bounds until the next layout change
  re-tiles it; `togglefloating`, `movewindow` and resizing tiles are D2.
- The first window of a desk is placed by the observer, a moment after Shell placed it.
- Only the active desk of each display is tiled; one tree per desk is D2 (done there).

## Workspaces and keybinds (D2)

Code: `PcSystemUI/src/com/android/systemui/wmshell/` (`HyprTileController.kt` dispatchers,
`HyprKeybinds.kt`, `HyprSnapHandler.kt`, provided by `PcWMShellModule` and created by
`PcWMComponent.init()`), and in the engine `Direction.kt` (`movefocus` geometry) and
`Binds.kt` (bind parsing, the default binds, workspace arguments). Still no `frameworks/base`
change.

### Workspaces are desks

- Workspace N of a display is its N-th desk in Shell's order (`DesktopRepository.getDeskPosition`),
  so Shell's desk-switch slide goes the right way. A missing desk is created on demand
  (`DesktopTasksController.createDesk(enforceDeskLimit = false)`); going to workspace 5 with
  two desks creates desks 3, 4 and 5 (desks have no numbers of their own; the empty ones stay
  until used). Workspaces 1-9 (Hyprland allows more; the binds stop at 9). Empty desks are not
  removed when left (Hyprland removes empty workspaces; Shell keeps desks, which D4's bar will
  show as empty).
- Each desk keeps its own hyprtile layout (D1's `HyprTile` already held one per desk id; D1
  only synced the active one). A desk's layout can change while it is hidden (`movetoworkspace`
  from it); the sync writes its tiles when it shows again.
- `workspace N`: `addDeskActivationChanges()` and `DeskSwitchTransitionHandler.startTransition()`,
  the same calls Shell's `activateNextDesk()` makes, so the switch is Shell's lateral slide, plus
  the target desk's tiles in the same `WindowContainerTransaction`. Relative arguments `e+1`,
  `e-1`, `+1`, `-1` work. With no desk active (home), Shell's `activateDesk()`.
- `movetoworkspace N` follows the window, as in Hyprland: one desk-switch transition that
  activates the target desk, reparents the task (`addMoveToDeskTaskChanges()`, public), and
  carries the new tiles of both desks; the old desk closes the gap and the window lands in its
  tile on the new one, with the slide. A floating window stays floating.

### Dispatchers

`HyprTileController.dispatch(name, arg)` (Hyprland's names; D4's `IHyprShell.dispatch` will
call the same method):

| Dispatcher | What it does |
|---|---|
| `workspace N` / `e±1` | switch desks with the slide (above) |
| `movetoworkspace N` | move the focused window and follow it |
| `movefocus l/r/u/d` | brings the neighbour to front (focus follows): the closest window beyond that edge that overlaps on the other axis, ties to the larger overlap; with none, the nearest window wholly beyond the edge (floating ones); nothing at the layout's edge |
| `movewindow l/r/u/d` | tiled: swaps with that neighbour in the layout tree; floating: moves to that edge of the work area |
| `togglefloating` | tiled → floating, centred, half the work area's width and two thirds of its height; floating → back into the layout next to the newest tiled window. A window the user floated stays floating (the sync and float rules leave it); non-resizeable windows stay floating |
| `togglesplit` | dwindle: flips the split that holds the focused window |
| `fullscreen 1` | maximize inside the desk: the work area without gaps, the tile is kept; again, and the window goes back to its tile |
| `fullscreen` / `fullscreen 0` | Shell's real fullscreen (`toggleFocusedTaskFullscreenState`), out of the desk and back |
| `killactive` | Shell's `quitFocusedDesktopTask()` (what its close key gesture does) |
| `exec <package>` | starts the package's launcher activity as the current user; desktop mode puts it on the active desk, where it is tiled |

The focused window is the globally focused task of the active desk
(`FocusTransitionObserver`), as for Shell's own key gestures.

### Keybinds without a framework patch

`InputManager`'s key gestures have fixed types (`KeyGestureEvent.KEY_GESTURE_TYPE_*`), none of
them "workspace 3" or "move focus left". But the server never checks the type: a "custom input
gesture" (the shortcuts a user can define in Settings, `addCustomInputGesture`) maps a key
trigger to any int, `interceptKeyBeforeDispatching` matches it before the focused app sees the
key, and the gesture goes to the process that registered a handler for that int. So
`HyprKeybinds` gives every bind a private type (`0x48590000` + its index, 128 slots), registers
one `KeyGestureEventHandler` for all of them, and maps a gesture back to its dispatcher. The
SystemUI process already holds `MANAGE_KEY_GESTURES`; the
`enable_customizable_input_gestures` flag is on in this release. InputManager stores custom
gestures per user on disk; `HyprKeybinds` removes its own (by type) and adds the current list
at every start, so a changed modifier or bind replaces the old one. A bind whose keys a user
shortcut already uses, or that AOSP reserves (all of AOSP's system shortcuts use META, plus
ALT+TAB), is skipped with a log line.

**Default modifier: ALT.** On Waydroid the host's Hyprland takes SUPER before Android sees it,
and every AOSP system shortcut (`InputGestureManager`) is on META, so ALT is the one modifier
free on both. It is one setting, `persist.hypr.mod` (`ALT`, `SUPER`, `CTRL ALT`, ...); D5's Lua
sets it. Default binds, after Hyprland's example configuration (`$mod` = ALT):

| Keys | Dispatcher |
|---|---|
| `$mod 1`..`9` | `workspace 1`..`9` |
| `$mod SHIFT 1`..`9` | `movetoworkspace 1`..`9` |
| `$mod` arrows | `movefocus l/r/u/d` |
| `$mod SHIFT` arrows | `movewindow l/r/u/d` |
| `$mod C` | `killactive` |
| `$mod V` | `togglefloating` |
| `$mod J` | `togglesplit` |
| `$mod F` / `$mod SHIFT F` | `fullscreen 1` / `fullscreen 0` |
| `$mod E` | `exec com.android.documentsui` (Files) |
| `$mod Q` | `exec` of `persist.hypr.terminal`, when set (AOSP has no terminal) |

Extra binds until D5: `persist.hypr.bind.1` .. `.32`, one Hyprland bind line each
(`ALT SHIFT, Q, exec, com.termux`, `$mod` allowed); one replaces a default on the same keys.
A single property for all binds does not work: a property value holds at most 92 bytes. Key
names are Android's (`KEYCODE_` dropped) with xkb's common ones mapped (`LEFT` → `DPAD_LEFT`,
`RETURN` → `ENTER`, `BACKSPACE` → `DEL`, ...). The list is read at SystemUI start; D5 will push
changes through `IHyprShell` and call `installBinds()` again.

### Shell's own shortcuts and snapping

- **Key gestures.** `DesktopModeKeyGestureHandler` registers nine gesture types, and a type
  can have only one handler per process, so `HyprKeybinds` unregisters Shell's handler
  (`InputManager.unregisterKeyGestureEventHandler`, Shell's handler object comes from Dagger)
  and registers those types itself: snap left/right (META `[` / `]`) → `movewindow l/r`, toggle
  maximize (META `=`) → `fullscreen 1`, previous/next desk (META CTRL `[` / `]`) →
  `workspace e-1/e+1`; minimize, close, fullscreen and move-to-next-display go back to Shell's
  handler (`handleKeyGestureEvent` is public), as does everything while `persist.hypr.tiling`
  is false.
- **Snapping and two-window tiling.** Every snap (the caption menu's snap buttons, dragging a
  window to the left or right screen edge, the snap key gestures) ends in
  `SnapController.snapToHalfScreen()`, which forwards to the one `SnapEventHandler` given to
  its public `start()`; `DesktopModeWindowDecorViewModel` passes itself. `HyprSnapHandler`
  calls `start()` again at ShellInit and so comes first: a snap of a tiled window swaps it with
  its neighbour on that side, a floating one joins the layout, and the window's tile is written
  again (the drag moved it). Shell's `DesktopTilingDecorViewModel` never gets a pair, so no
  divider and no half-screen pair fight the layout. Everything else is forwarded to Shell's
  handler, and everything is while tiling is off. Left as is: Shell's drag indicator (the
  half-screen preview while dragging to an edge) still shows; D3 owns the decorations.

### Checked on Waydroid (2026-11-21)

PcSystemUI through the overlay as in D1, empty test data, 2538x1058, `gaps_in` 5,
`gaps_out` 12, keys sent with `input keycombination KEYCODE_ALT_LEFT ...`:

| Step | Result |
|---|---|
| Settings, Clock, Files on workspace 1 | dwindle tiles; `HyprKeybinds: 32 binds installed, main modifier [ALT]` |
| ALT arrows | focus moves left/right/up/down between tiles; at the layout's edge nothing happens |
| ALT SHIFT LEFT on Files | Files and Settings swap tiles |
| ALT V twice | Files floats centred at `[634,188][1903,833]`, the others re-tile; again and it is back in the layout |
| ALT F twice | Files `[0,27][2538,995]`, then back to `[1909,516][2526,983]` |
| ALT J | the right column's split flips to side by side |
| ALT SHIFT 2 | desk 2 is created; Files lands on it at `[12,39][2526,983]`, desk 1 closes the gap, one transition with Shell's lateral slide (`DeskSwitchTransitionHandler ... direction=RIGHT`) |
| ALT 1 / ALT 2 | slide between the desks, each with its own layout |
| ALT C | the focused window closes, the rest re-tile |
| ALT E | a new Files window opens tiled on the active desk |
| META CTRL `[`, META `[` | routed: `workspace e-1`, `movewindow l` |
| Settings dragged by its caption to the left edge | swaps with its left neighbour; no Shell tiling divider |
| ALT SHIFT 5 | desks 3, 4, 5 created; the window moves to 5; moved back with ALT SHIFT 2 it tiles next to Files on desk 2 |

Screenshots: `out/hypr-d2/` (`ws1-three.png`, `float.png`, `maximize.png`,
`ws2-moved.png`, `ws1-after-move.png`, `drag-snap-left.png`, `ws5.png`, `ws2-two.png`).
Host tests: 38 (`run-tests.sh`), with bind parsing, workspace arguments and `movefocus`
geometry.

Found on the way:

- **The host's gamepad reaches Waydroid.** An empty desk kept switching back to home a few
  seconds after it was shown: the wallpaper activity of the empty desk takes focus, and its
  back callback moves it to back, which deactivates the desk. The BACK came from the user's
  Xbox controller (Waydroid passes `/dev/input/event7` into the container; a gamepad's B is
  BACK), used for a game on another host workspace. For the test the node was removed inside
  the container (its own `/dev` tmpfs; the host's node is untouched; a new session brings it
  back). On the real PC, BACK on an empty desk going home is Shell's behaviour; Hyprland
  would stay on the empty workspace. D4's launcher, as home, can change that.
- `movefocus` first fell back to any window in the half-plane, which jumped diagonally;
  the fallback now only takes windows wholly beyond the edge.

Known limits:

- Binds are installed for user 0 (SystemUI's user); another user would need them added for
  that user on a user switch.
- Hyprland's `special` workspaces, `movetoworkspacesilent`, `swapnext`, `resizeactive`,
  `pseudo` and mouse binds (`bindm` move/resize) are not there yet.
- A window the user drags (not to an edge) or resizes still keeps its bounds until the next
  layout change (D1).

## Decoration and animation (D3)

Code: `PcSystemUI/src/com/android/systemui/wmshell/` (`HyprDecorations.kt`, `HyprAnimator.kt`,
`HyprMouse.kt`, `HyprStyleProps.kt`, wired into `HyprTileController`), in the engine
`Style.kt` (colours, gradients, `DecorationConfig`) and `Animation.kt` (`Bezier`,
`AnimationConfig`), and one small extension point in Shell
([tools/patches/aosp17-shell-decor-style.patch](../tools/patches/aosp17-shell-decor-style.patch)).

### Which decoration Shell runs, and the extension point

This release ships `enable_window_decoration_refactor` on, so desktop windows are decorated by
`DefaultWindowDecoration` (`WindowDecoration2`), not by `DesktopModeWindowDecoration`. Its
corner radius, SurfaceFlinger border (`BorderSettings`, a solid stroke) and shadows come from
fixed resources and are set on the task surface in every transition-driven relayout, and the
caption type is decided inside `getRelayoutParams()`. Replacing the view model would have
meant forking `WMShellModule` (D0) and copying a 2000-line decoration; setting our values from
outside fights Shell's relayouts. So D3 adds one extension point instead, `WindowDecorStyle`
(`libs/WindowManager/Shell/.../windowdecor/WindowDecorStyle.kt`, a static `instance`):

| Hook | Called from | Used for |
|---|---|---|
| `hidesCaption(taskInfo)` | `DefaultWindowDecoration.getRelayoutParams()` (freeform tasks) and its non-transition `relayout()` | caption type `NO_CAPTION` for tiled windows |
| `onRelayout(taskInfo, taskSurface, hasGlobalFocus, inSyncWithTransition, startT, finishT)` | `WindowDecoration2.relayout()`, right after Shell set the task surface's outline | corner radius, opacity, border layers, turning Shell's border off |
| `onClosed(taskId)` | `WindowDecoration2.close()` | releasing the border layers |

The patch is applied to the tree with
`git -C aosp/frameworks/base apply ../../tools/patches/aosp17-shell-decor-style.patch`.
Shell is a static library of the SystemUI APK, so the patch only rebuilds `PcSystemUI`
(`STEPS=PcSystemUI tools/aosp17-build.sh`); nothing in the system image changes. With no
`instance` set (AOSP's SystemUI), Shell behaves as before.

Found on the way: a freeform task's non-transition relayout (focus changes, task info changes)
builds its transaction with `applyStartTransactionOnDraw`, i.e. the caption's view applies it
when it next draws. A task with `NO_CAPTION` has no view, so the transaction was dropped: the
old window kept the active border after losing focus and a swapped window kept its old border
size. The patch applies the transaction at once when the style hides the caption.

### Decoration

`HyprDecorations` implements `WindowDecorStyle`:

- **Caption.** `caption = floating` (default): tiled windows have no caption (Hyprland), a
  floating window keeps Shell's header (app menu, minimize, maximize, close, drag), so a
  floating window can still be closed and moved with the mouse alone. `none` drops it
  everywhere, `all` keeps it everywhere. The caption follows `togglefloating`, since floating
  changes start a transition that relayouts the decoration.
- **Corners and opacity** are set on the task surface in Shell's own start and finish
  transactions after Shell's values, so they win and change in the same frame as Shell's
  geometry. A task leaving freeform gets radius 0 and alpha 1 back.
- **Border.** Four buffer layers per window, children of the task surface above the content
  and the caption (`HyprBorder#<task>.<i>`, no input): top and bottom strips as tall as the
  corner radius (they hold the rounded corners) and two side strips. Each strip is drawn in
  software with the gradient of the whole window and uploaded as a `HardwareBuffer`
  (`Transaction.setBuffer`), a few kilobytes instead of a window-sized buffer. The stroke runs
  inside the window, its outer edge on the task's rounded clip, so the corners match exactly.
  Buffers are cached per focus state until the size or configuration changes, so a focus
  change only swaps buffers. Hyprland draws the border outside the window; inside, it covers
  `border_size` pixels of the app's edge, which keeps it clipped and moving with the task
  surface in every Shell animation without a second surface to keep in step.
- **Gradient.** Hyprland's syntax (`rgba(33ccffee) rgba(00ff99ee) 45deg`, also `rgb()`,
  `0xAARRGGBB`, `#RRGGBB[AA]`), colours spread evenly, the angle evaluated exactly as
  Hyprland's border shader does (folded into 0-90 degrees, progress `y*sin + x*(1-sin)`);
  `Gradient.line()` turns that linear function into the two points of an Android
  `LinearGradient`, so the colours land where Hyprland puts them (unit-tested).
- **Shell's own outline.** Where Shell sets its border (floating windows with a caption, box
  shadows v2 on), a zero-width `BorderSettings` replaces it; its drop shadow stays unless
  `shadow = false`. Tiled windows have no caption, so Shell sets no outline on them at all.
- **Opacity.** Shell's animations end with alpha 1 in their finish transaction, which is
  applied after the decoration's; the controller sets every visible window's opacity again
  right after each transition finishes.

### Animations

`HyprAnimator` plays all animations on transition leashes with one `ValueAnimator` per
transition; every surface is a track with its own duration and curve, and the transition
finishes when the longest track ends. Curves are evaluated by `Bezier.value()` (cubic bezier,
Newton then bisection, overshoot allowed), so Lua (D5) and the unit tests see the same curves.
`HyprTileController` is the newest transition handler, so it is asked first:

| Transition | Who animates | Animation |
|---|---|---|
| Re-tiling it starts itself (`togglefloating`, `movewindow`, `togglesplit`, `fullscreen 1`, sync after Shell's minimize/close, mouse swap) | hyprtile (`startTransition(..., this)`) | `windowsMove`: position and size interpolated, the content (already at its new size) scaled to the current size, as Hyprland stretches it |
| Workspace switch (`workspace N`, `movetoworkspace N`) | hyprtile: the same `TRANSIT_DESKTOP_MODE_DESK_TO_DESK_SWITCH` with the same desk-activation WCT as Shell's `DeskSwitchTransitionHandler`, plus `SnapController`'s desk-switch callbacks | `workspacesIn`/`workspacesOut`: `slide` (100 % of the display), `slidevert`, `fade`, `slidefade[vert] N%` |
| A window opening or coming back (request from the core, on the active desk) | hyprtile, also when Shell's mixed handler has a pending launch for it (it is told `onTransitionConsumed`, which drops its state) | `windowsIn` + `fadeIn`: `popin N%` (grow from N % around the centre), `slide [edge]` or `fade`; neighbours `windowsMove` |
| A window closing or going to back by request (app finishing, `am force-stop`) | hyprtile | `windowsOut` + `fadeOut`, the reverse |
| Shell-started closes and minimizes (caption buttons, `killactive`, back on a desk's last activity), entering or leaving the desk, home, fullscreen | Shell, as before | Shell's; the re-tile after it is hyprtile's |

A transition is only taken when every change is a standard freeform task on a desk (or the desk
root moving to front): anything else (activities inside a task, wallpaper, display changes, the
first window of a desk, which also activates it) is left to Shell, with a log line saying why.
A closing task can already be gone from `DesktopRepository` when its transition is ready, so a
task whose parent is a desk root counts too. A window dragged with the mouse starts its
animation where it was dropped (`startOverride`), not at its old tile.

Defaults are Hyprland's example configuration: `windows 4.79 easeOutQuint`,
`windowsIn 4.1 easeOutQuint popin 87%`, `windowsOut 1.49 linear popin 87%`, `fadeIn 1.73` and
`fadeOut 1.46 almostLinear`, except workspaces: `2.5 easeOutQuint slide` (Hyprland's default
style, a quarter second) instead of the example's fade.

### Mouse binds

Without captions on tiled windows, Hyprland's `bindm` binds are the mouse way to manage them.
`HyprMouse` is a gesture monitor on the display: a press with the main modifier
(`persist.hypr.mod`, ALT) on a desktop window of the active desk pilfers the pointer (the app
gets a cancel) and:

- **left drag** moves the window: it follows the pointer on its surface; a tiled window dropped
  on another tiled window swaps with it (animated from where it was dropped), dropped elsewhere
  it slides back to its tile; a floating window lands where it is dropped;
- **right drag** resizes from the corner nearest to the press: a floating window (minimum
  200 px or the app's minimum), a tiled one by moving the split lines next to it
  (`HyprTile.resize`: dwindle moves the nearest split of that direction, master changes mfact;
  also Hyprland's `resizeactive`). The new bounds land in one animated transition when the
  button is up;
- **middle click** toggles floating (a floating window gets its caption back, with close).

### Configuration

`DecorationConfig` and `AnimationConfig` are the objects D5's Lua will fill; until then
`HyprStyleProps` reads them from properties, re-read with the tiling properties after every
transition (a bad value is logged and its default kept):

| Property | Hyprland | Default |
|---|---|---|
| `persist.hypr.rounding` | `decoration:rounding` | 10 |
| `persist.hypr.border_size` | `general:border_size` | 2 |
| `persist.hypr.col.active_border` | `general:col.active_border` | `rgba(33ccffee) rgba(00ff99ee) 45deg` |
| `persist.hypr.col.inactive_border` | `general:col.inactive_border` | `rgba(595959aa)` |
| `persist.hypr.active_opacity`, `inactive_opacity` | `decoration:*_opacity` | 1.0, 1.0 |
| `persist.hypr.caption` | (none) | `floating` (`none`, `all`) |
| `persist.hypr.shadow` | `decoration:shadow:enabled` | true (floating windows) |
| `persist.hypr.animations` | `animations:enabled` | true (false: Shell's animations) |
| `persist.hypr.bezier.1`..`.16` | `bezier = NAME, X0, Y0, X1, Y1` | Hyprland's example curves |
| `persist.hypr.animation.1`..`.32` | `animation = NAME, ONOFF, SPEED, CURVE[, STYLE]` | above |

Animation names follow Hyprland's tree (`global` > `windows` > `windowsIn`/`windowsOut`/
`windowsMove`, `fade` > `fadeIn`/`fadeOut`/`fadeSwitch`, `border`, `workspaces` >
`workspacesIn`/`workspacesOut`); a node not set takes its parent's settings, an empty style its
parent's style; speed is in deciseconds as in Hyprland.

### Checked on Waydroid (2026-11-21)

Waydroid is the test target (as for D1 and D2; Cuttlefish was not started): `PcSystemUI.apk`
through the overlay, the empty test data directory, 2538x1058 at density 180, the window
alone on host workspace 4. Mouse binds could not be driven with a modifier from outside
(`input` injects no meta state, and events written into Waydroid's `wl_pointer_events` pipe
move the pointer without producing drag moves), so they were checked with the test hook
`debug.hypr.bindm_test` (`move`, `resize`, `float`: every press acts so, no modifier) and
`input mouse swipe`; ALT plus a press was checked through the pipe (`ALT` + left press and
release on a window swapped it with the one under the release).

| Check | Result |
|---|---|
| Three to six apps tiled | no caption on tiled windows, rounded corners (10 px), 2 px border: focused window `rgba(33ccffee) -> rgba(00ff99ee) 45deg` (cyan at top left, green at bottom right), others grey; a floating window (Contacts' permission dialog task) keeps Shell's caption |
| Focus moves between windows | only the focused window has the gradient (after the patch's fix for caption-less relayouts) |
| Desk restored at boot | windows tiled where Shell put them get their caption removed by the controller's relayout (`caption of task N out of date`) |
| A new app opens (Contacts, Calendar) | `animating request: OPEN, MOVE`: popin from 87 % with fade-in, the neighbours move in the same transition |
| A window closes by request (`am force-stop` of Clock) | `CLOSE, MOVE, MOVE`: shrinks to 87 % and fades in ~150 ms while the neighbours grow |
| `togglefloating` twice, `togglesplit`, swaps | `animating change: MOVE, ...`: the window grows from its tile to the centred float rect and back, bezier `easeOutQuint` |
| `workspace 2` / `workspace 1` | own desk-switch transition, the desks slide by the full display width in 250 ms, both directions |
| MOD + left drag (Calendar onto microG) | they swap; the animation starts where the window was dropped |
| MOD + right drag on a tiled window (Files' right edge, +300 px) | the split moves, all six windows re-tile in one animated transition |
| MOD + right drag on a floating window, MOD + middle click | floating resize lands animated; middle click puts the floating window back into the layout |
| `persist.hypr.rounding 20`, `border_size 4`, `col.active_border` three colours `90deg`, `inactive_opacity 0.85` | applied at the next transition: red-yellow-blue top to bottom on the focused window, unfocused windows translucent |
| SurfaceFlinger timestats over 8 s of re-tiling and desk switches (`dumpsys SurfaceFlinger --timestats`) | 516 frames composited, 0 missed, 0 janky timeline frames at 150 Hz; SurfaceFlinger's frame duration under 2 ms |

Screenshots and recordings: `out/hypr-d3/` (`open3.png`, `corner-*.png`, `t5.png`,
`config1.png`, `resize3.png`, `anim3.mp4`, `close.mp4`, `resize3.mp4`; contact sheets
`s3-open.png`, `s3-float.png`, `s3-ws1.png`, `s3-ws2.png`, `s6-close.png`).

Found and fixed on the way: Shell's mixed handler holds a pending launch for desktop opens
(the open was Shell's animation until hyprtile took it and called `onTransitionConsumed`);
a closing task is gone from the repository before its transition is ready; Shell relaunched
an app into the desk it last lived on, not the active one, and the sync now re-tiles a task
found on another desk than hyprtile thought; bounds applied with `applyTransaction` outside a
transition reach WindowManager but not the task surfaces, so mouse resizes land in one
transition when the button is up.

Known limits:

- Resizing with the mouse is not live: the window (and its neighbours) change when the button
  is up, animated. Live resizing needs Shell's drag-resize path (`setDragResizing` and the
  decoration setting position and crop while resizing).
- Shell-started closes and minimizes (caption buttons, `killactive`, back on the last activity)
  keep Shell's own animation; only the re-tile after them is hyprtile's. Taking them means
  replacing `DesktopMixedTransitionHandler`'s close and minimize paths.
- The border is drawn inside the window (Hyprland draws it outside) and does not animate its
  colour or angle (`border`, `borderangle`).
- The first window of a desk (which also activates the desk) keeps Shell's animation.
- The configuration is read again after transitions, not when a property changes; D5 pushes it
  through `IHyprShell`.
- Cuttlefish at 2560x1080/160 was not run for D3: Waydroid replaced it as the test target, and
  the shell code is the same APK.


## `HyprLauncher` (D4)

Code: `aosp-device/hypr/HyprLauncher/` (Kotlin, `android_app`, `system_ext`, privileged,
platform key, `privapp_whitelist_com.android.hyprlauncher`), `aosp-device/hypr/hyprshell-aidl/`
(`IHyprShell`, `IHyprShellListener`), `PcSystemUI/.../wmshell/HyprShell.kt` (the Shell side),
`aosp-device/hypr/rro/` (`hypr_overlay_frameworks_base_core`) and, in the engine, `Fuzzy.kt`.
Still no `frameworks/base` change.

### Home and recents provider

| Piece | How |
|---|---|
| HOME | `HomeActivity` (HOME filter at priority 1, above Settings' `FallbackHome`): the wallpaper, an empty Hyprland workspace, with a one-line key hint. Back does nothing, HOME again closes the launcher and the overview |
| Recents provider | `config_recentsComponentName` = `com.android.hyprlauncher/.RecentsActivity` in the RRO. SystemUI only uses the package (it binds the package's `QUICKSTEP_SERVICE`); WindowManager gives the package's uid the `recents` permissions and lets it provide insets. The class does not exist on purpose: naming `HomeActivity` would make WindowManager type home as recents, and the overview is a window, not an activity |
| `LauncherService` | the `QUICKSTEP_SERVICE` (permission `STATUS_BAR_SERVICE`, like Quickstep's `TouchInteractionService`), an `ILauncherProxy.Stub`. Of the 29 calls it implements `onInitialize`, `onOverviewToggle`/`Shown`/`Hidden`, `onSystemUiStateChanged` (keyguard) and `onUnbind`; the rest are navigation bar, taskbar and assistant callbacks that a desktop without them never needs. The AIDL is a copy of SystemUI's (`src/com/android/systemui/shared/recents/`), not `SystemUISharedLib`, which would pull most of SystemUI's shared code into the launcher |
| Direct boot | the service is `directBootAware`, as Quickstep's is: `LauncherProxyService` resolves the service once, when SystemUI starts, and with a lock credential that is before the user is unlocked; a non-aware service was never bound (`mIsEnabled=false`). Its preferences live in device-protected storage; the app list reloads at `USER_UNLOCKED` |
| Key gestures | in AOSP 17 Quickstep, not `PhoneWindowManager`, handles Alt+Tab (`KEY_GESTURE_TYPE_RECENT_APPS_SWITCHER`, "gesture 54 is not supported" without it), META+Tab (`RECENT_APPS`) and the all-apps key (`ALL_APPS`); HyprLauncher registers them (`MANAGE_KEY_GESTURES`, `signature\|recents`) |

### `IHyprShell`

`PcSystemUI`'s `HyprShell` registers an `IHyprShell.Stub` with `ShellController.addExternalInterface()`
under its descriptor, so `LauncherProxyService` puts it into the Bundle of
`ILauncherProxy.onInitialize()` next to `IDesktopMode`, `IRecentTasks` and Shell's other
interfaces (the launcher logs the keys it got). Calls check `MANAGE_ACTIVITY_TASKS`, as Shell's
own interfaces do, which the recents provider holds; no permission of our own.

| Call | What |
|---|---|
| `dispatch(name, arg)` | `HyprTileController.dispatch`, plus `focuswindow <task id>` (new: switches to the window's workspace and brings it to front, minimized or not, through `DesktopTasksController.moveTaskToFront`) |
| `setReserved(display, top, bottom)` | the bar's strip (waybar's exclusive zone). Shell's `DisplayLayout.getStableBoundsForDesktopMode()` counts only the navigation bar and the status bar's resource height, not insets other windows provide, so hyprtile takes the larger of Shell's top and the bar's |
| `setConfig(Bundle)` | string values under the `persist.hypr.*` names (`layout`, `gaps_in`, `gaps_out`, `reserved_*`, `float_packages`) over the properties; D5's Lua fills it, and the decoration and animation settings join it then |
| `getState(display)`, listener `onStateChanged` | workspaces (count, active), focused task, and per window its task id, workspace, package, title (`TaskDescription` label), bounds, floating and minimized flags. Pushed 50 ms after every layout sync and every task-info change (titles change without a transition) |
| listener `onEvent` | Hyprland's socket events derived from consecutive states: `workspace>>N`, `activewindow>>package,title`, `openwindow>>id,package`, `closewindow>>id` (for D5's `hl.on`) |
| listener `onDispatch` | the `launcher` and `overview` (`hyprexpo:expo`) dispatchers, which the launcher runs |

New default binds (hyprtile `Binds.kt`): `$mod D` and `$mod SPACE` → `launcher`, `$mod GRAVE`
→ `overview`.

### The bar

A `TYPE_STATUS_BAR_ADDITIONAL` window across the top, 30 dp (33 px on Waydroid's density 180):

- **Apps** button (the launcher), **workspaces** 1..N (N = the desks that exist; the active one
  filled, occupied ones bright, empty ones dim; click switches, the mouse wheel over the bar
  steps `e±1` as in waybar), the **focused window's title** (task label, else the app's name),
  **running apps** (one icon per window, all workspaces; dimmed when not on the active one or
  minimized; click focuses it, switching workspace), **notifications** and **quick settings**
  (`StatusBarManager.expandNotificationsPanel()` / `expandSettingsPanel()`), the **clock**.
- **Window type.** Launcher3's taskbar uses `TYPE_NAVIGATION_BAR_PANEL`, but that layer (25) is
  above the notification shade and the keyguard (17); `STATUS_BAR_ADDITIONAL` (16) is above apps
  and the launcher's panels (system dialogs, 6) and below the shade, where a status bar belongs.
  The keyguard is see-through, so the bar hides while it shows (`onSystemUiStateChanged`,
  keyguard showing and not occluded).
- **Insets.** It provides `statusBars` and `tappableElement` insets of its height (the recents
  provider may), so an app that takes the whole screen lays out below it and an immersive one
  hides it; and it tells hyprtile through `setReserved` (above). Tiles start at
  33 + `gaps_out`.

### The status bar of SystemUI: hidden

Decided: SystemUI's status bar has no height (`status_bar_height_default`, `_portrait`,
`_landscape` = 0 in the RRO). The bar has what a desktop needs from it (clock, notification and
quick settings entries); a second strip above or below it would cost space and duplicate the
clock. Its window and insets are empty; the shade, quick settings, heads-up notifications and the
keyguard still work (they are the notification shade window, opened from the bar). Not kept:
SystemUI's status icons (battery, network, the notification icons); the notification entry and
quick settings show them. A tray with icons is D5 material (bar modules in Lua).

### App launcher

rofi-like: a centred box (system dialog window, dim behind), search field, list of apps
(`queryIntentActivities` MAIN/LAUNCHER, reloaded on package changes). Fuzzy matching is
hyprtile's `fuzzyScore`/`fuzzyFilter` (characters in order, bonuses for consecutive matches, word
starts and the first letter, small penalties for gaps; unit-tested). Keyboard driven: type,
Up/Down or Tab, Page Up/Down, Enter starts, Esc closes; a click starts, a click outside or losing
focus closes. The app starts on the default display; desktop mode puts it on the active
workspace and hyprtile tiles it. Bound to `$mod D`, `$mod SPACE`, the bar's Apps button and the
all-apps key.

### Overview

After hyprexpo: a full-screen panel with every workspace as a display-shaped cell in a grid (up
to 3 columns), each window drawn at its real place from its task snapshot
(`TaskSnapshotManager.takeTaskSnapshot` for visible windows, the cached one for others, the app
icon when there is none; `READ_FRAME_BUFFER` comes with the recents role). Click a window: focus
it (switching workspace); click a cell: that workspace; 1-9 switch; arrows/Tab move the
selection, Enter focuses it, Esc or `` ` `` closes. Bound to `$mod GRAVE`, META+Tab and the
recents key (`onOverviewToggle`). Alt+Tab opens it on the next window (the active workspace's
windows front to back, then the others), Tab steps on, releasing Alt focuses the selection.

### HOME on fresh and existing userdata

- **Fresh userdata:** the role controller picks the only HOME activity besides `FallbackHome`;
  no chooser.
- **Existing userdata** (Smart Dock the HOME role holder, set by H4's setup script): the build
  no longer has Smart Dock or Launcher3, Android drops them at the first boot (new
  fingerprint), and the role controller gives the role to HyprLauncher. `HomeRole.kt` is the
  safety net: once per user, if the holder is empty, Smart Dock, Launcher3 or not installed, it
  takes the role (`MANAGE_ROLE_HOLDERS`, platform key) and starts HOME; a launcher the user chose
  later is kept.
- Smart Dock's other first-boot grants (notification listener, overlay and usage app ops,
  `WRITE_SECURE_SETTINGS`, its accessibility service in `enabled_accessibility_services`) stay
  in such a userdata, pointing at a package that is gone; harmless.

### What left the product

`HyprLauncher` overrides `Launcher3`, `Launcher3QuickStep` (and the Go variants), `hypr.mk`
filters `SmartDock` out of `PRODUCT_PACKAGES`, and `desktop.mk` no longer installs
`smartdock-setup.sh` and `init.desktop.rc` (kept in `aosp-device/desktop/` for the record). The
Launcher3 patches (`aosp17-launcher3-recents-only.patch`, `aosp17-launcher3-taskbar-npe.patch`)
stay in `tools/patches/`, superseded: they only matter for a build that ships Launcher3.
`config_showNavigationBar=false` (I2) and `config_disableLockscreenByDefault` (H4) stay.
`tools/fetch-apps.sh smartdock` still downloads it; it is not installed. Checked on the built
images (`system_ext.img`, `product.img`, `vendor.img`, extracted with `fsck.erofs`):
`HyprLauncher.apk` and its permissions file on system_ext, the RRO on vendor, no Launcher3,
`SmartDock`, `smartdock-setup.sh` or `init.desktop.rc`.

### Checked on Waydroid (2026-11-21)

Waydroid is the test target (the user, 2026-11-21; Cuttlefish was not started). Through the
overlay as for D1-D3: `system_ext/priv-app/{PcSystemUI,HyprLauncher}`,
`system_ext/etc/permissions/com.android.hyprlauncher.xml`,
`vendor/overlay/hypr_overlay_frameworks_base_core.apk`, and overlayfs whiteouts (`mknod c 0 0`)
for `system_ext/priv-app/Launcher3QuickStep` and `product/app/SmartDock` (with `SystemUI`'s from
D1). 2538x1058 at density 180.

| Check | Result |
|---|---|
| D1-D3 test data (existing) | HOME role `com.android.hyprlauncher`, `HomeActivity` resumed, Smart Dock and Launcher3 not installed, the RRO enabled |
| Empty test data (fresh) | the same, no chooser; `query-activities ... HOME` lists only `HomeActivity` and `FallbackHome` |
| The user's data (existing, Smart Dock the holder in `roles.xml`, a lock credential) | after the boot the holder is `com.android.hyprlauncher`; the user is `RUNNING_LOCKED`, so `FallbackHome` until the user unlocks (not done: the credential is the user's); SystemUI is connected to the launcher already (direct boot) and the bar hides under the keyguard |
| SystemUI → launcher | `LauncherProxyService isConnected=true`; `onInitialize` carries `IHyprShell` with Shell's 11 interfaces; `bar reserves top=33` in hyprtile |
| Bar | workspaces, title, running apps, clock; SystemUI's status bar strip gone; clicks switch workspaces (`dispatch workspace 1`) and focus windows; the bell and the gear open the shade and quick settings |
| Launcher | ALT+D and ALT+SPACE open it with focus (`mCurrentFocus=HyprLauncher`), `sett` → Settings first, Enter opens apps that tile below the bar (`[12,45]` top-left) |
| Overview | ALT+` shows workspaces 1-3 with snapshots; a click on a window of another workspace switches there and focuses it (`dispatch focuswindow 41`); Esc closes |
| Alt+Tab | switches between the two most recent windows (`dispatch focuswindow 47`); the recents key toggles the overview |
| Crashes | none from SystemUI or HyprLauncher (the user's data has a DeskClock database downgrade crash from LineageOS 20, unrelated) |

Screenshots: `out/hypr-d4/` (`home.png`, `launcher.png`, `tiled3.png`, `overview.png`,
`shade.png`, `qs.png`, `fresh-tiled.png`, `fresh-overview.png`, `real-keyguard.png`).

Found on the way: the service has to be direct boot aware (above); Launcher3's window type puts
the bar over the keyguard; Alt+Tab is the recents provider's key gesture in this release.

Known limits:

- The overview is static while open (snapshots load once; window moves update the layout
  through the listener). No drag of windows between workspaces in it yet (Hyprexpo has it).
- The bar's tray is two entries, not status icons; battery, network and notification icons are
  in the shade.
- The bar and panels are on the default display only.
- `onEvent` events come from state differences, so two changes inside 50 ms merge.
- Not checked: unlocking the user's own data (their credential), and Cuttlefish at
  2560x1080/160.

## Lua configuration (D5)

Reference with every option and an example: docs/hypr-lua.md. Code:
`aosp-device/hypr/hyprlua/` (engine, `hl` API, translation, tests), `HyprLauncher/.../LuaConfig.kt`
and `Notifier.kt`, and the shell side in `PcSystemUI` (`HyprTileController`, `HyprStyleProps`,
`HyprKeybinds`, `HyprMouse`).

### Decisions

- **Engine: `external/lua` (Lua 5.4.1) with our own JNI binding** (`libhyprlua_jni`, built
  from `:liblua-sources` plus `jni/hyprlua_jni.c`, about 350 lines). Tried and dropped:
  LuaJava and LuaJ are not in the tree (a download, and LuaJ is Lua 5.2 in pure Java with no
  instruction hook); the tree's Lua is what CarService's ScriptExecutor sandboxes the same
  way. The binding opens only base, coroutine, table, string, math and utf8, keeps
  `os.time/date/clock` as `hl.time/date/clock`, drops `dofile`, `loadfile`, `string.dump`,
  compiles text only, caps memory (32 MB allocator) and instructions (a count hook: 20 M for
  init.lua, 2 M per hook call, plus a 2 s deadline; once spent it raises on every instruction,
  so `pcall` cannot swallow it). Strings cross JNI as UTF-8 byte arrays: `NewStringUTF`
  aborts under CheckJNI on bytes a Lua string may hold.
- **API: Hyprland 0.57's own Lua API**, not the names sketched earlier: the host runs
  Hyprland 0.57.2 with a Lua configuration (`hl.config({ general = { ... } })`, `hl.curve`,
  `hl.animation({ leaf = ... })`, `hl.bind("SUPER + Q", hl.dsp.window.close())`,
  `hl.window_rule({ match = { class = ... } })`, `hl.on("window.open", fn)`, generated stubs in
  `/usr/share/hypr/stubs/hl.meta.lua`). Implementing a subset of it means a Hyprland
  configuration loads here with warnings for what Android lacks; Hyprland's option names (407,
  from the stubs) are accepted with a warning, others are errors. This shell's own settings sit
  under `shell.*` (`main_mod`, `default_binds`, `terminal`, `caption`, `tiling`); the bar is
  `hl.bar`/`hl.bar_module`. The old socket event names (`openwindow`, ...) and the sketch's
  `hl.bezier`, `hl.animation(name, opts)` and `require("hypr")` work too.
- **`init.lua` at `/sdcard/Documents/hypr/`** (shared storage, any editor, `adb push`, MTP),
  the default copied on first run; `require` reads files next to it (no `..`, no absolute
  paths). The launcher grants itself all-files access (`MANAGE_EXTERNAL_STORAGE` app op, set
  with `MANAGE_APP_OPS_MODES`, platform key). The last good configuration is kept in
  device-protected storage and sent before the user is unlocked.
- **Reload**: FileObserver on the directory and its sub-directories, and a 3 s
  modification-time poll for writes that bypass FUSE; 300 ms debounce. An error keeps the last
  good state (the old Lua state keeps running its hooks and bar modules) and shows Hyprland's
  red error strip under the bar until a save loads; hook errors and `hl.notify` are
  notifications at the top left (`Notifier`, a no-input window).
- **Wire format**: `IHyprShell.setConfig` keeps D4's Bundle (strings under the
  `persist.hypr.*` names) and gains string arrays (`bezier`, `animation`, `binds`,
  `windowrule`, `workspace`, `gesture`); a Lua configuration is complete (defaults filled), so
  stale properties from D1-D3 never leak in. `binds` is the whole list: the default binds are
  computed in the launcher, so `hl.unbind` and replacing a default work; the shell reinstalls
  its custom key gestures when the list changes. A Lua function bind is dispatcher `lua <id>`,
  which the shell sends back to the launcher (`onDispatch`), so Lua never runs in SystemUI.
- **New in the shell**: window rules (float/tile through `shouldFloat`, workspace once for a
  window opened since the last sync, per-window opacity through `HyprDecorations`), workspace
  rules (per-workspace layout and gaps in `HyprTile.workspaceRules`, by desk), dispatchers
  `movetoworkspacesilent` (a raw WCT plus `DesktopRepository.addTaskToDesk`: Shell records desk
  moves in its own transition handlers only) and `resizeactive`, the mouse binds' modifier
  (`mouse_mod`), and dwindle/master options that were properties only.
- Workspace names and `on_created_empty` are handled in the launcher (bar, events); gestures
  are checked and carried for D6.

### Checked on Waydroid (2026-11-21)

Test data (`XDG_DATA_HOME=~/.local/share/waydroid-test`), APKs through the overlay as in D4;
init.lua edited live with `cat file | sudo waydroid shell -- sh -c 'cat > /sdcard/...'`.
Host tests: 24 hyprlua tests with real Lua (`run-tests.sh` and Soong's `hyprlua-tests`), 62
hyprtile tests.

| Check | Result |
|---|---|
| First start | default init.lua written to `/sdcard/Documents/hypr/`, `init.lua loaded: 35 binds, 0 warnings`, tiling as before |
| Live edit: `gaps_in 15`, `gaps_out 40`, `border_size 6`, red-yellow 90deg gradient, blue inactive, `rounding 0`, `inactive_opacity 0.8` | applied about 0.3 s after the write, no restart: windows re-tiled with the new gaps in one animated change, borders and corners redrawn |
| New binds | `ALT+B` (a Lua function: `hl.notify` with `#hl.get_windows()`) shows "Lua bind: 4 windows"; `ALT+G` switches to workspace 2, firing the `workspace.active` hook |
| Animation | `workspaces` 8 ds with a custom `slowpoke` curve and `slidevert`: mid-switch screenshot shows the old desk sliding up |
| Bar | Lua module with a 1 s timer (`lua 14:40:16` ticking), workspace 2 named `web` by a workspace rule |
| Broken file (`gaps_in = "wide"` on line 122, then a syntax error) | red strip `init.lua:122: general.gaps_in: expected a whole number, got 'wide'` / `init.lua:2: unexpected symbol near <eof>`; the old configuration and its Lua module keep working |
| Fixed file with `require("extra")` (master, `mfact 0.6`, gaps 8, window rules) | loads, strip gone; editing `extra.lua` alone reloads too |
| Window rules | Clock (`float = true`) opens floating with its caption; Calendar (`workspace = "3"`) is moved to a new workspace 3 |
| `resizeactive 200 0`, `movetoworkspacesilent 2` | master column 200 px wider; Settings moves to workspace 2 while workspace 1 stays and re-tiles without a hole |

Screenshots: `out/hypr-d5/shots/`.

Found on the way: right after unlock MediaProvider has no `external_primary` volume yet (writes
fail with EPERM), so the first load retries every 2 s; PackageManager keeps a system app's
native ABI from its last scan while the fingerprint stays the same, so an APK that newly embeds
the library gets none (`primaryCpuAbi=null`) on an existing userdata: `LuaState` falls back to
`System.load("<apk>!/lib/<abi>/libhyprlua_jni.so")`; `waydroid shell` makes a file given as
its stdin root-owned, like its stdout (pipe it).

Known limits:

- Events come from the shell's 50 ms state pushes; two changes in that time give one event.
- A window rule's workspace applies once, after the window opened (it opens on the current
  workspace and moves; with an app that keeps relaunching an activity, as Calendar does with
  microG's sign-in, focus can come back to the old workspace).
- `ALT+S` minimized the focused window on Waydroid before our bind ran (not from this code;
  avoid it for binds until found).
- Spring curves, submaps, special workspaces, groups, layer rules and plugin APIs are not
  supported; their calls warn or fail with a clear message (docs/hypr-lua.md).

## Touch gestures (D6)

Code: `PcSystemUI/.../wmshell/HyprTouch.kt` (the gesture monitor), `HyprWindowMenu.kt` (the
window menu), touch helpers in `HyprTileController` (`windowAt`, `frameAt`, `touchHold`,
`touchDrop`, `dispatchOn`), the recognizer and gesture names in the engine (`Touch.kt`, plain
Kotlin, unit-tested), gesture binds in `hyprlua` (`ConfigBuilder`, `prelude.lua`), and a test
injector (`aosp-device/hypr/tools/touchinject/`). Still no `frameworks/base` change.

### Decisions

- **In the shell, not an accessibility service.** `HyprTouch` is a second gesture monitor on
  the display next to `HyprMouse`'s (`InputManager.monitorGestureInput`): it sees every touch
  in parallel with the window under the finger and only takes one (`pilferPointers`, the app
  gets `ACTION_CANCEL`) once it has recognized a gesture that has a bind. Taps, scrolls,
  flings, long-presses and pinches inside apps are untouched: the gestures are one finger
  from a screen edge, a held finger on a window's frame, three or more fingers, or a pinch
  off the windows.
- **Gestures are binds.** A gesture is a bind with no modifiers and the key
  `GESTURE:<NAME>` (Lua `hl.bind("gesture:edge_bottom", ...)`), so it travels in the
  existing `binds` list of `IHyprShell.setConfig`, takes any dispatcher or Lua function and is
  removed with `hl.unbind`. `HyprKeybinds` skips them; Hyprland's `hl.gesture` lines (touchpad
  gestures there) are turned into the same binds. Without a Lua configuration the defaults
  apply. Reference: docs/hypr-lua.md, "Touch gestures".
- **HarmonyOS PC mode's layout of edges**: bottom edge → app launcher, left/right edge →
  previous/next workspace, top edge → notifications, its right third → quick settings
  (HarmonyOS opens its control centre from the top right). Two new dispatchers,
  `notifications` and `quicksettings` (`StatusBarManager.expandNotificationsPanel` /
  `expandSettingsPanel`, SystemUI holds `EXPAND_STATUS_BAR`). Overview: pinch in on the
  wallpaper or the bar, or three fingers up anywhere; a tap on a window in the overview
  focuses it (the overview's click handler, unchanged).
- **System gesture exclusion respected.** `HyprTouch` registers an
  `ISystemGestureExclusionListener` (as SystemUI's back gesture does): an edge swipe never
  starts in the region WindowManager grants the apps (limited to 200 dp per side edge), and
  no gesture at all starts in the unrestricted region (an app's own swipe area). HyprLauncher's
  overview and app launcher declare themselves excluded, so a long-press in the overview does
  not pick up the window drawn under it. Edge zones are measured from the display's edges; the
  bar's strip at the top is a window like any other (a swipe from the top edge over it opens
  the shade; the bar gets a cancel). On the lock screen only the top edge acts.
- **Window frame long-press.** HarmonyOS moves a window by long-pressing its title bar. Tiled
  windows here have no caption, so the "frame" is 14 dp inside a window's edges, the gap next
  to it, and on floating windows Shell's 40 dp caption. A tiled window's top is its app's
  (toolbars), so only its border counts there. A still finger there for 350 ms
  (`persist.hypr.touch_hold`, below the 400 ms of apps' own long-press, so the app gets a
  cancel before it acts) picks the window up: it follows the finger on its surface (the D3
  mouse-drag path) and on release swaps with the tiled window under the finger, slides back to
  its tile, or (floating) lands there; a floating window dropped on the left or right screen
  edge joins the layout on that side (`onSnap`, as Shell's drag-to-snap does since D2). Released
  without moving, it opens the window menu.
- **Window menu.** A small no-focus system window (`TYPE_SYSTEM_DIALOG`) at the finger: Float
  or Tile, Maximize (tiled; `fullscreen 1`), Fullscreen (Shell's fullscreen), Workspace 1..N+1
  (`movetoworkspacesilent`: the window goes, the view stays), Close. Entries run on that window,
  not the focused one (`dispatchOn`; close goes through Shell's `closeTask`, fullscreen through
  `moveToFullscreen`). A touch outside closes it.
- **Discrete swipes.** A workspace swipe switches once it is recognized, with the D3 slide;
  the workspace does not follow the finger (that would need a desk-switch transition driven by
  the finger, which Shell's desk switch handler cannot do).
- **A floating window's caption stays Shell's.** Shell's caption takes a press on it for its
  own drag (and pilfers the pointer), so a long-press there moves the window with Shell's drag
  instead of opening the menu; dropped on the left or right screen edge it joins the layout
  through the D2 snap routing. The menu of a floating window opens from its border.

### Checked on Waydroid (2026-11-21)

Test data (`XDG_DATA_HOME=~/.local/share/waydroid-test`), APKs through the overlay as in D4,
2538x1058 at density 180, five windows on workspace 1 (Files, Contacts, Settings, a second
Files, Clock) and Calendar with microG's sign-in on workspace 2. The host has no touchscreen, so
touches were injected: one finger with `input touchscreen swipe/tap`, several fingers and
holds with `touchinject.dex` (`aosp-device/hypr/tools/touchinject/`, `build.sh`; `CLASSPATH=/data/local/tmp/touchinject.dex
app_process / TouchInject swipe|pinch|hold|tap ...`, `InputManagerGlobal.injectInputEvent` as
shell, touchscreen source, finger tool type).

| Check | Result |
|---|---|
| Swipe up from the bottom edge | `gesture EDGE_BOTTOM -> launcher`, the launcher has focus |
| From the right edge / the left edge | workspace 1 → 2 → 3 with the slide; from the left edge on workspace 1 the dispatcher refuses (no workspace 0), nothing happens |
| From the top edge / its right third | the notification shade / quick settings open |
| Pinch on workspace 2's windows (Calendar, microG) | nothing: the pinch is the app's |
| Pinch in on empty workspace 3 | the overview opens; a tap on Contacts in it focuses it (`focuswindow 18`, back to workspace 1) |
| Three fingers up over app windows | the overview |
| Long-press in the overview where a window's frame lies under it | nothing picked up (the overview's exclusion rect); the press is the overview's click |
| Long-press in the gap between Files and Contacts, release | the window menu for Files at the finger; Float floats it (centred, with Shell's caption) |
| Long-press on the floating Files' caption, drag to the left screen edge | Shell's caption drag; dropped on the edge, Files joins the layout on the left |
| Long-press on Contacts' bottom border, drag onto Settings | they swap, animated from the drop point |
| Menu → Workspace 2 on Contacts | `movetoworkspacesilent 2`: Contacts goes to workspace 2, workspace 1 re-tiles, the view stays |
| Menu → Close on Clock | Clock closes (Shell's `closeTask`), the layout closes the hole |
| Scroll in Settings, tap Battery, long-press an item, a fast swipe starting on a window's border | the list scrolls, Battery opens, the item's own long-click, the app gets the swipe; `HyprTouch` logs nothing for any of them |
| init.lua `hl.bind("gesture:edge_bottom", hl.dsp.overview())`, `hl.bind("gesture:swipe_4_down", function() hl.notify(...) end)` | loaded at once (43 binds); the bottom edge opens the overview, four fingers down show the Lua notification |
| `shell = { touch = false }` | the bottom edge swipe does nothing; restoring the file brings the gestures back |
| Crashes | none (`logcat -b crash` empty) |

Screenshots: `out/hypr-d6/` (`edge-topright.png`, `pinch-overview.png`, `menu.png`,
`menu-float.png`, `menu-label.png`, `drag-snap.png`, `drag-swap.png`, `scroll-compare.png`, `lua-notify.png`).
Host tests: 70 hyprtile (8 new in `TouchTest`), 25 hyprlua.

Known limits:

- Not checked with a real touchscreen (the host has none); injected events have the same
  source and tool type as a touchscreen's. On the PC, a touchscreen is a desktop-mode extra.
- Workspace swipes are discrete (above); no live resize by touch (D3's mouse resize lands on
  release too, and it has no touch trigger).
- The menu is plain text entries (no icons), on the default display only; `HyprTouch` watches
  the default display only.
- An app's gesture exclusion region is the one WindowManager reports for the whole display:
  a window above an app that set exclusion rects subtracts its own area, so it works for
  stacked windows, but the region updates asynchronously (a gesture right after a window
  moved can see the old region).

## Waydroid and wrap-up (D7)

### The full image on Waydroid

D1-D6 tried new APKs through Waydroid's overlay (`/var/lib/waydroid/overlay`). D7 built the
whole product once more (`tools/aosp17-build.sh`, image steps up to `super.img`, build
`eng.caoanh.20261121.2248`), packed it for Waydroid (`tools/waydroid/aosp17-pack.sh`, which
now creates `out/waydroid-aosp17/` itself) and copied `system.img` and `vendor.img` into
`/var/lib/waydroid/images-aosp17/` (`aosp17-switch.sh install` does the same plus the switch
from LineageOS 20; Waydroid already ran this image, so a copy was enough). The overlay's D1-D6
test entries (`PcSystemUI`, `HyprLauncher`, the hypr RRO, the permissions file and the
whiteouts for `SystemUI`, `Launcher3QuickStep` and `SmartDock`) and a hand-built
`pc_overlay_frameworks_base_core.apk` from 2026-11-19 that shadowed the image's newer one were
moved to `/var/lib/waydroid/overlay-phase-d-tests.20261121-2228/`, so the image's own copies
run. Left in the overlay: the pieces `aosp17-switch.sh` puts there (`vendor/waydroid.prop`, the
LXC mount points) and `vendor/lib64/hw/vulkan.nvidia.so` (identical to the image's).

### End-to-end check (2026-11-21)

The user's data (backed up first, `~/.local/share/waydroid-backup/aosp17-20261121-1716`): the
new fingerprint booted, PackageManager upgraded the system apps, the HOME role is
`com.android.hyprlauncher`, `PcSystemUI` is SystemUI, the hypr RRO is enabled, SystemUI
connected to the launcher (`onInitialize` with `IHyprShell` and Shell's 12 other interfaces),
and the bar hides under the keyguard. The user stays `RUNNING_LOCKED`: the lock credential is
the user's own, so everything behind the lock screen was checked on the test data
(`XDG_DATA_HOME=~/.local/share/waydroid-test`, unlocked) with the same image, plus the Play
Store from the user's Google extras (`data/vendor/pc-extras` copied over). Keys were sent with
`input keycombination` inside Android, touches with `input touchscreen`; helpers in
`out/hypr-d7/` (`k.sh`, `sc.sh`, `tasks.sh`, `soak.sh`), screenshots there too.

| Check | Result |
|---|---|
| HOME | HyprLauncher's empty workspace with the bar (Apps, workspaces, clock, notification and quick settings entries) |
| Launcher (`ALT + D`, type, Enter) | Settings, Play Store and Chromium open tiled, one after another, neighbours re-tile in the same transition; Landroid (the Easter Egg's space game, the one game on the test data) from `am start` (not a launcher entry) |
| Tiling | six windows in dwindle (`tiled5.png`), `ALT` arrows move focus, `ALT SHIFT` arrow swaps, `ALT V` floats and back, `ALT J` flips a split, `ALT F` maximizes and back |
| Workspaces | `ALT SHIFT 3` moves the focused window (Play Store) to workspace 3 and follows it; `ALT 1`..`9` each slide to that desk (desks 4-9 created on demand); the bar marks active, occupied and empty ones |
| Overview (`ALT + grave`) | nine cells, workspace 1 with five snapshots, 2 with Calendar and microG, 3 with the Play Store (`overview.png`) |
| Lua hot reload | an edited init.lua (`gaps_in 10`, `gaps_out 30`, `border_size 5`, red-yellow 90deg border, `rounding 20`) loaded in about 0.3 s and every window re-tiled and redrawn (`lua-edit.png`); the original file restored the same way |
| Touch | bottom edge → launcher, right/left edge → workspace e+1/e-1, top edge → notification shade |
| Apps in tiles | Settings navigates (Network & internet) in its tile; Landroid runs its autopilot (input and rendering in a tile); Chromium draws its first-run screen at tile and maximized sizes (not completed: it asks to accept Google's terms, left to the user); the Play Store opens and tiles but stays blank on the test data, which has no Google account (its GMS calls time out) |
| ~10 minutes of use (`soak.sh 600`: 15 rounds of focus, move, float, split, maximize, open, move to workspace 4, close, overview, launcher, edge swipes, workspace switches) | crash buffer empty, `SystemUI` and `HyprLauncher` kept their pids, no `E` lines from any `Hypr*` tag |
| `SUPER` as `main_mod` | five default binds collide with AOSP's META shortcuts and are skipped; the documented replacement binds work (`META + 2`, `+ comma`, `+ W`); docs/hypr-lua.md, "Waydroid and the host's SUPER key" |
| The final image on the user's data | as at the top of this section; then `waydroid session stop`, `waydroid-container` inactive and disabled |

Fixed in D7:

- **An empty strip under every tile.** Shell's `DisplayLayout.getStableBoundsForDesktopMode()`
  keeps `taskbar_frame_height` (56 dp, 63 px at Waydroid's density 180) free at the bottom
  whenever it differs from the navigation bar's height, and this product has neither. The hypr
  RRO sets `taskbar_frame_height` to 0: tiles now end at `gaps_out` above the bottom edge
  (`[12,45][1264,1046]` instead of `...983]`), and Shell's own placement of floating windows
  gains the space too. Nothing else in the tree reads it but an accessibility panel's
  position.
- `aosp17-pack.sh` failed on a checkout without `out/waydroid-aosp17/`.

Found, not caused by the shell:

- **The host's game pad reaches Android** (already seen in D2). Waydroid passes the user's
  Xbox controller (`/dev/input/event7`, `js0`) into the container and brings it back when the
  pad reconnects; with the user playing on another host workspace during the check, its B
  button sent BACK to the focused Android window, which closed windows and backed out of
  desks in the middle of the soak test. Removing the nodes inside the container stops it until
  the pad reconnects. D5's "`ALT + S` minimizes the focused window" did not happen again
  without the pad: it was the pad, not a key gesture.
  Since fixed on the host side: host pads stay out of the container unless switched on
  (`tools/waydroid/container-guard.sh gamepads on`; docs/waydroid-aosp17.md, "Host game pads").
- `com.android.phone` and `com.android.nfc` restart every few seconds with "failed to complete
  startup" ANRs: the telephony stack waits for a radio HAL that the container does not have
  (`RIL.getRadioServiceProxy` → `waitForDeclaredService`). It predates Phase D on this image
  (Cuttlefish without a modem shows the same) and does not touch the shell.
  Since fixed for Waydroid: the radio and NFC APEXes are off in the container
  (`ro.boot.vendor.apex.com.google.cf.{rild,nfc} = none`, docs/waydroid-aosp17.md, Status
  2026-11-21).
- The user's data has DeskClock crashing on a database from LineageOS 20 (a downgrade), as in D4.

### Keybinds next to the host's Hyprland

`main_mod` stays ALT (D2's decision holds): the host's Hyprland binds SUPER for itself, and of
the ALT combinations it binds only `ALT + TAB`, `ALT + F2`/`F3`/`F11`/`F12` and
`CTRL + ALT + P` (`hyprctl binds`), none of them a default bind here. So in the Waydroid
window every default bind reaches Android except Android's own `ALT + TAB` switcher, which
stays the host's (`ALT + grave` opens the overview instead). Users who want SUPER set
`main_mod = "SUPER"` and add a pass-through submap to the host's configuration: an empty
submap entered while the Waydroid window is focused, with an example and the SUPER keys AOSP
keeps in docs/hypr-lua.md, "Waydroid and the host's SUPER key". The repository does not edit
`~/.config/hypr`. Not checked: keys typed on the host's physical keyboard into the Waydroid
window during D7 (the user was in a full-screen game on the host, and keys sent to an
unfocused window through Hyprland's `sendshortcut` did not reach Android); the path is the
one D2-D6 used, and the injected keys go through the same InputManager key gestures.

### The USB stick build

The same product builds for the stick: the D7 build's `super.img`, boot images and vbmeta
came out of the same run, and `tools/mkusb-aosp17.sh --userdata 64 out/aosp17-usb-d7.img`
made an 8.7 GiB stick image whose `android/build.txt` names build `eng.caoanh.20261121.2248`
(patch level 2026-06-05, unchanged). The stick itself was not touched; the image is ready for
`tools/update-usb-aosp17.sh` (docs/usb-boot-aosp17.md, §10).

## As built

| Piece | Where | Process | What it does |
|---|---|---|---|
| `PcSystemUI` | `aosp-device/hypr/PcSystemUI/` (system_ext, privileged, platform key, `overrides: ["SystemUI"]`) | SystemUI | SystemUI with its own root component, `PcWMComponent` and `PcWMShellModule` (CarSystemUI's pattern); creates the controllers below at ShellInit |
| `HyprTileController` | `PcSystemUI/.../wmshell/` | SystemUI | the newest Shell transition handler and an observer: tiles desktop tasks per desk, dispatchers, workspaces, rules, config from `IHyprShell` |
| `HyprKeybinds`, `HyprSnapHandler` | same | SystemUI | binds as InputManager custom key gestures with private types; Shell's snap/desk gestures and drag-to-snap routed to hyprtile |
| `HyprDecorations`, `HyprAnimator`, `HyprMouse`, `HyprStyleProps` | same | SystemUI | `WindowDecorStyle` (the one Shell patch, `tools/patches/aosp17-shell-decor-style.patch`): no caption on tiles, corners, opacity, gradient borders; bezier animations on transition leashes; MOD + mouse binds |
| `HyprTouch`, `HyprWindowMenu` | same | SystemUI | touch gestures as binds, the window menu |
| `HyprShell` | same | SystemUI | `IHyprShell` as a Shell external interface, handed to the recents provider |
| `hyprtile` | `aosp-device/hypr/hyprtile/` (plain Kotlin, 70 host tests) | (library) | dwindle, master, gaps, rules, binds, gestures, colours, beziers, fuzzy matching |
| `HyprLauncher` | `aosp-device/hypr/HyprLauncher/` (system_ext, privileged, platform key, overrides Launcher3) | its own | HOME, recents provider (`QUICKSTEP_SERVICE`), bar, app launcher, overview, Alt+Tab, notifications and the error strip |
| `hyprlua` | `aosp-device/hypr/hyprlua/` (Lua 5.4 from `external/lua`, JNI, 25 host tests) | HyprLauncher | `init.lua` with Hyprland 0.57's Lua API subset → the configuration Bundle; hooks, timers, bar modules |
| `hyprshell-aidl` | `aosp-device/hypr/hyprshell-aidl/` | | `IHyprShell`, `IHyprShellListener` |
| `hypr_overlay_frameworks_base_core` | `aosp-device/hypr/rro/` (vendor) | | recents component, no status bar height, no taskbar height |
| `hypr.mk` | `aosp-device/hypr/` | | adds the above, drops Smart Dock |

Changes outside `aosp-device/`: one Shell patch (`WindowDecorStyle`, applied to
`aosp/frameworks/base` and built into the `PcSystemUI` APK only) and none in the system image.

## Known limits

Collected from D1-D7 (details in each sub-phase's section):

- **Tiling.** An app's minimum size wins over its tile (it can overlap its neighbour); a
  window the user drags or resizes keeps its bounds until the next layout change; the first
  window of a desk is placed by the observer just after Shell placed it, with Shell's
  animation.
- **Workspaces.** Desks have no numbers of their own (workspace N = the N-th desk) and empty
  ones stay; no special workspaces, groups, `pseudo` or submaps; binds are installed for
  user 0 only.
- **Decoration and animation.** The border is drawn inside the window and does not animate;
  mouse and touch resizes land on release, not live; Shell-started closes and minimizes keep
  Shell's animation.
- **Launcher.** The overview is static while open (no window drag between workspaces); the
  bar has no status icons (battery, network: in the shade); bar, panels and touch gestures are
  on the default display only; events come from 50 ms state pushes, so two changes in that
  time merge.
- **Lua.** No spring curves, submaps, special workspaces, groups, layer rules or plugins; a
  window rule's workspace applies once, after the window opened.
- **Touch.** Checked only with injected events (no touchscreen on the host); workspace swipes
  are discrete.
- **Waydroid.** `ALT + TAB` belongs to the host; SUPER needs the host submap above and loses
  the keys AOSP keeps; the host's game pad reaches Android; the user's own data was checked
  up to its lock screen only (its credential is the user's).
- **Not checked:** Cuttlefish at 2560x1080/160 after D0, the real PC with the stick (the
  stick was not updated in Phase D), CDD/CTS expectations for desktop windowing.

## Waydroid notes

- Waydroid overlay-mounts `/var/lib/waydroid/overlay` on the rootfs and `/vendor`, so a new
  `PcSystemUI` or `HyprLauncher` APK can be tried without repacking the image (D1-D6): the APK
  under `overlay/system_ext/priv-app/<name>/`, overlayfs whiteouts (`mknod c 0 0`) to hide the
  image's `SystemUI`, `Launcher3QuickStep` or `SmartDock`. Remove such entries again once the
  image has the new code (D7 did), or they shadow it.
- The user allowed Waydroid for every Phase D step (2026-11-21) and made it the test target in
  place of Cuttlefish (`tools/waydroid/container-guard.sh check` first).
- init.lua lives in the container's shared storage (`/sdcard/Documents/hypr/`); write it with
  `cat init.lua | sudo waydroid shell -- sh -c 'cat > /sdcard/Documents/hypr/init.lua'`
  (redirecting the file into `waydroid shell` makes it root-owned).
- `waydroid shell` (lxc-attach) makes its stdout and stderr root-owned when they are files:
  pipe its output (`... </dev/null 2>&1 | cat`) when a tool captures it to a file.
- The host's Hyprland tiles the Waydroid window too; move it alone onto a workspace
  (`hyprctl dispatch 'hl.dsp.window.move({ workspace = 4, follow = false, window =
  "address:0x..." })'`) to get the full 2538x1058.
- Remove the passed-through game pad inside the container before tests
  (`sudo waydroid shell -- rm -f /dev/input/event7 /dev/input/js0`; the host's node is
  untouched, a reconnect brings it back).

## Other devices

Google offers no supported way to replace SystemUI on someone else's device. Installing on
another AOSP 17 build needs that build's platform key (some Waydroid images use the public
test keys: check `ro.build.tags` and `framework-res.apk`'s certificate) and a `PcSystemUI`
built from the same AOSP revision. A runtime-hook port (LSPosed) is possible if `hyprtile`
keeps its hook points into Shell few and small, but it is outside Google's guidelines and not
planned.

## Open questions and risks

- ~~Which Shell classes `PcWMShellModule` can replace without touching `frameworks/base`~~:
  answered in "Shell extension points (D0)".
- ~~How Shell's own snapping, tiling and drag-to-desktop interact with `hyprtile`~~: snapping
  and its two-window tiling are routed to hyprtile ("Workspaces and keybinds (D2)").
- ~~Quickstep's recents interfaces are large~~: HyprLauncher implements the few
  `ILauncherProxy` calls a desktop needs ("`HyprLauncher` (D4)").
- CDD and CTS expectations for desktop windowing (configuration changes, insets, multi-window
  callbacks) were not checked yet. Tiling must go through Shell transitions, not shortcuts.
