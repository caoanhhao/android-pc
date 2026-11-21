# A Hyprland-style desktop shell (Phase D)

Status (2026-11-21): design only, nothing built. This note records what was decided in the
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

- Exactly which Shell classes `PcWMShellModule` can replace without touching
  `frameworks/base` (desktop task placement, window decoration, desk switching).
- How Shell's own snapping, tiling and drag-to-desktop interact with `hyprtile`; they may have
  to be turned off in the variant.
- Quickstep's recents interfaces are large; HyprLauncher needs only the part SystemUI requires
  to bind it.
- CDD and CTS expectations for desktop windowing (configuration changes, insets, multi-window
  callbacks) were not checked yet. Tiling must go through Shell transitions, not shortcuts.
