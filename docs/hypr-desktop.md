# A Hyprland-style desktop shell (Phase D)

Status (2026-11-21): D0 done, the `PcSystemUI` skeleton (`aosp-device/hypr/`) replaces
SystemUI with unchanged behaviour; its findings are in "Shell extension points (D0)". This note
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
- Waydroid overlay-mounts `/var/lib/waydroid/overlay` on `/system` and `/vendor`, so a new
  `PcSystemUI` or `HyprLauncher` APK could be tried without repacking the image. Not checked.
- Waydroid still starts only with the user's explicit OK (CLAUDE.md); early tests run on
  Cuttlefish at 2560x1080.

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
