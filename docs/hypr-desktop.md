# A Hyprland-style desktop shell (Phase D)

Status (2026-11-21): D0 done, the `PcSystemUI` skeleton (`aosp-device/hypr/`) replaces
SystemUI with unchanged behaviour; its findings are in "Shell extension points (D0)". D1 done:
`hyprtile` (dwindle and master) tiles desktop tasks on open, close and minimize, checked on
Waydroid ("`hyprtile` and dynamic tiling (D1)"). This note
records what was decided in the
brainstorm that started in `~/projects/hyprland-android` (an empty repo, now folded into this
one) and what AOSP 17 already offers. Plan and sub-phases: PLAN.md, "Phase D".

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
- edge-swipe and pinch gestures for touch.

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
  Event hooks (`hl.on("openwindow", ...)`) run here, asynchronously. Engine: LuaJava (Lua 5.4
  over JNI) unless something better fits; `os` and `io` are not exposed.

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

Not yet checked in Waydroid (the user allowed Waydroid tests for Phase D after D0 was
verified). Trying `PcSystemUI` there needs the overlay to add
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
- Only the active desk of each display is tiled; one tree per desk is D2.

## Configuration sketch

```lua
local hl = require("hypr")

hl.general {
  gaps_in = 5, gaps_out = 12, border_size = 2,
  active_border = { "#33ccffee", "#00ff99ee", angle = 45 },
  layout = "dwindle",
}
hl.decoration { rounding = 10, inactive_opacity = 0.95 }

hl.bezier("snappy", 0.05, 0.9, 0.1, 1.05)
hl.animation("windows", { speed = 4, curve = "snappy", style = "popin 80%" })
hl.animation("workspaces", { speed = 5, curve = "snappy", style = "slide" })

hl.bind("ALT, Return", hl.exec("com.termux"))
hl.bind("ALT, Q", hl.dispatch("killactive"))
hl.bind("ALT, 1", hl.dispatch("workspace", 1))
hl.bind("gesture:edge_bottom", hl.dispatch("launcher"))

hl.windowrule("org.mozilla.firefox", { workspace = 2 })

hl.on("workspace", function(ws) hl.notify("Workspace " .. ws.id) end)
```

Whether the names should follow Hyprland's own Lua configuration closely is still open.

## Waydroid

- The host's Hyprland takes SUPER bindings before Waydroid sees them. Either the shell uses
  another modifier by default (ALT above) or the host gets a pass-through submap while the
  Waydroid window has focus.
- Waydroid overlay-mounts `/var/lib/waydroid/overlay` on the rootfs and `/vendor`, so a new
  `PcSystemUI` or `HyprLauncher` APK can be tried without repacking the image: checked in D1
  (the APK under `overlay/system_ext/priv-app/PcSystemUI/`, a whiteout hiding `SystemUI`).
- The user allowed Waydroid for every Phase D step (2026-11-21) and made it the test target in
  place of Cuttlefish (`tools/waydroid/container-guard.sh check` first).
- `waydroid shell` (lxc-attach) makes its stdout and stderr root-owned when they are files:
  pipe its output (`... </dev/null 2>&1 | cat`) when a tool captures it to a file.
- The host's Hyprland tiles the Waydroid window too; move it alone onto a workspace
  (`hyprctl dispatch 'hl.dsp.window.move({ workspace = 4, follow = false, window =
  "address:0x..." })'`) to get the full 2538x1058.

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
- How Shell's own snapping, tiling and drag-to-desktop interact with `hyprtile`; they may have
  to be turned off in the variant.
- Quickstep's recents interfaces are large; HyprLauncher needs only the part SystemUI requires
  to bind it.
- CDD and CTS expectations for desktop windowing (configuration changes, insets, multi-window
  callbacks) were not checked yet. Tiling must go through Shell transitions, not shortcuts.
