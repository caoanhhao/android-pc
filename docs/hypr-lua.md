# hypr Lua configuration reference

The desktop shell of `aosp_pc_x86_64` (Phase D, docs/hypr-desktop.md) is configured with one
Lua file, as Hyprland 0.57 is. The API is a subset of Hyprland's own Lua API (`hl.config`,
`hl.bind`, `hl.dsp.*`, `hl.window_rule`, `hl.on`, ...), so most of a Hyprland configuration
loads as it is; what makes no sense on Android is accepted with a warning or refused with a
clear error. The bar (`hl.bar`, `hl.bar_module`) is this shell's own.

## Where the file is

| | |
|---|---|
| File | `Documents/hypr/init.lua` on the shared storage: `/sdcard/Documents/hypr/init.lua` (`/storage/emulated/0/...`), reachable from any file manager, text editor, `adb push` or MTP |
| First run | HyprLauncher writes a commented default there (the built-in defaults, spelled out) |
| Split files | `require("name")` reads `name.lua` next to init.lua; `require("config.binds")` reads `config/binds.lua`. A module runs once; its return value is cached. Names are letters, digits, `_`, `-` and dots, never `..` or `/` |
| Reload | on save (FileObserver on the directory and its sub-directories, plus a modification-time check every 3 s for writes that bypass the FUSE mount); 300 ms after the last write, so an editor's several writes give one reload |
| Errors | a syntax or runtime error, a bad value or an unknown option: the new file is not used, the last good configuration stays, and a red strip under the bar shows `init.lua: config error` and the message with its line (`init.lua:12: general.gaps_in: expected a whole number, got 'wide'`), until a save loads again. Errors in event hooks, Lua binds and timers show as notifications |
| Warnings | options that exist in Hyprland but do nothing here (`decoration.blur.*`, `input.*`, ...), unsupported bind flags, ignored rule effects: the file loads, a yellow notification lists them, logcat (`HyprLua`) has all |
| Before unlock | the shared storage is not mounted; the last good configuration (kept in device-protected storage) is applied until the user unlocks, then init.lua loads |

Edit live from the host (Waydroid or adb):

```sh
adb push init.lua /sdcard/Documents/hypr/init.lua
adb logcat -s HyprLua
```

## Engine and sandbox

Lua 5.4 from the AOSP tree (`external/lua`), built into HyprLauncher as `libhyprlua_jni`
(`aosp-device/hypr/hyprlua/`). It runs in the launcher's process on its own thread, never in
SystemUI: the result is a declarative configuration sent to the shell through
`IHyprShell.setConfig`, so a broken script cannot take SystemUI down.

- Libraries: `base`, `coroutine`, `table`, `string`, `math`, `utf8`. No `os`, `io`, `debug`,
  `package`/`loadlib`, `dofile`, `loadfile`, `string.dump`; `load` compiles text only (no
  bytecode). The time functions are `hl.time()`, `hl.date(fmt)`, `hl.clock()` (Lua's
  `os.time`, `os.date`, `os.clock`).
- Limits: 20 million instructions for init.lua with its requires, 2 million for each hook, bind,
  timer or bar module call, and 2 s of wall time for either; 32 MB of memory for the state.
  Past the instruction or time limit every further instruction raises the error, so a `pcall`
  in the script cannot swallow it.
- `print(...)` goes to logcat (`HyprLua: lua: ...`).

## Options: `hl.config`

`hl.config(table)` sets options by Hyprland's names; it can be called any number of times,
later values win. Nested tables are sections (`general = { col = { active_border = ... } }`
is `general.col.active_border`). `hl.get_config("general.gaps_in")` reads one back.

| Option | Type, range | Default | What |
|---|---|---|---|
| `general.layout` | `"dwindle"`, `"master"` | `dwindle` | tiling layout (per workspace: `hl.workspace_rule`) |
| `general.gaps_in` | int 0..500 | 5 | gap between windows (each side, so neighbours are 2x apart) |
| `general.gaps_out` | int 0..500 | 12 | gap between windows and the work area's edge |
| `general.border_size` | int 0..50 | 2 | border width, px; 0 for none |
| `general.col.active_border` | gradient | `rgba(33ccffee) rgba(00ff99ee) 45deg` | focused window's border |
| `general.col.inactive_border` | gradient | `rgba(595959aa)` | other windows' border |
| `decoration.rounding` | int 0..200 | 10 | corner radius, px |
| `decoration.active_opacity` | 0..1 | 1.0 | focused window |
| `decoration.inactive_opacity` | 0..1 | 1.0 | other windows |
| `decoration.shadow.enabled` | bool | true | Shell's drop shadow under floating windows |
| `animations.enabled` | bool | true | false: Shell's own animations |
| `dwindle.split_width_multiplier` | 0.1..10 | 1.0 | a box splits top/bottom when height x this > width |
| `dwindle.default_split_ratio` | 0.1..1.9 | 1.0 | 1.0 splits in the middle |
| `dwindle.preserve_split` | bool | false | keep a split's direction when its box changes shape |
| `dwindle.force_split` | 0..2 | 2 | 1: new window left/top, 2: right/bottom; 0 (follow the mouse) acts as 2 |
| `master.mfact` | 0.05..0.95 | 0.55 | master area's share |
| `master.new_status` | `"master"`, `"slave"`, `"inherit"` | `slave` | `master`: a new window becomes the master |
| `master.new_on_top` | bool | false | a new stack window goes to the top of the stack |
| `binds.workspace_back_and_forth` | bool, 0..2 | 0 | switching to the active workspace goes to the previous one (2: the previous one on this display) |
| `binds.pass_mouse_when_bound` | bool | false | bound mouse buttons and wheel steps reach the app too |
| `binds.scroll_event_delay` | 0..2000 | 300 | ms between two firings of a wheel bind (not `repeating` ones) |
| `binds.allow_workspace_cycles` | bool | false | accepted; as in Hyprland 0.57's most-recently-used workspace history it changes nothing |
| `shell.main_mod` | modifiers | `ALT` | modifier of the default binds, and of the mouse binds unless `mouse:` binds say otherwise. SUPER belongs to the host's compositor under Waydroid |
| `shell.default_binds` | bool | true | install Hyprland's example binds on `main_mod` (below) |
| `shell.terminal` | package | none | `main_mod + Q` starts it (AOSP has no terminal) |
| `shell.file_manager` | package | `com.android.documentsui` | `main_mod + E` |
| `ipc.allow` | packages, comma-separated | `com.termux,com.android.hyprlauncher` | apps that may use hyprctl's sockets besides root, shell and system; `*` for every app (docs/hypr-ipc.md) |
| `shell.caption` | `"none"`, `"floating"`, `"all"` | `floating` | which windows keep Shell's title bar (close, maximize, drag) |
| `shell.tiling` | bool | true | false: no tiling, Shell's own desktop windowing |
| `shell.touch` | bool | true | false: no touch gestures (edge swipes, pinches, the window frame's long-press) |
| `shell.workspaces` | 1..1000 | 10 | how many workspaces there are (E2); binds and rules past it load with a warning and do nothing |
| `shell.alt_tab` | `"switcher"`, `"cycle"`, `"bind"`, `"overview"`, `"off"` | `switcher` | what `ALT + Tab` does (E4): `switcher` holds ALT and walks the focus history in a strip of icons and titles, releasing ALT focuses the selection, a quick tap is the window focused before; `cycle` runs `cyclenext` on every Tab (SHIFT: `cyclenext prev`), as the host binds `ALT + TAB`; `bind` runs the bind on `ALT + Tab` on every Tab (E3's way; the overview when there is none); `overview` is D4's overview on the next window; `off` nothing. See "ALT + Tab" below |
| `decoration.dim_special` | 0..1 | 0.2 | how dark the windows behind a shown special workspace get (E5; the wallpaper is not dimmed) |
| `dwindle.special_scale_factor`, `master.special_scale_factor` | 0.1..1 | 1.0 | share of the work area a special workspace tiles in, centred (the active layout's value applies) |
| `binds.hide_special_on_workspace_change` | bool | false | switching workspace hides a shown special workspace (else it stays over the new one) |
| `cursor.zoom_factor` | number, 1 or more | 1.0 | Hyprland's zoom: magnifies the screen around the pointer (Phase E10; above 8 acts as 8). A change applies at once, so the host's `hl.config({ cursor = { zoom_factor = z } })` bind functions work; binds are better off with `hl.dsp.zoom` |

Types: integers must be whole numbers; booleans are `true`/`false` (or 0/1); modifiers are
`ALT`, `SUPER` (`META`, `WIN`), `CTRL`, `SHIFT`, joined by spaces or `+`.

Colours: `rgba(33ccffee)`, `rgb(33ccff)`, `0xee33ccff` (ARGB, also as a Lua number),
`#33ccff`, `#33ccffee`. A gradient is a string of colours with an optional `Ndeg`
(`"rgba(33ccffee) rgba(00ff99ee) 45deg"`) or a table `{ colors = { ... }, angle = 45 }`;
the angle is evaluated as Hyprland's border shader does.

Every other option name of Hyprland 0.57 (`decoration.blur.*`, `input.*`, `misc.*`,
`general.resize_on_border`, ...) is accepted with a warning. A name that is not Hyprland's
either is an error.

## Curves and animations

```lua
hl.curve("snappy", { type = "bezier", points = { {0.05, 0.9}, {0.1, 1.05} } })
hl.bezier("snappy", 0.05, 0.9, 0.1, 1.05)           -- the same
hl.animation({ leaf = "windowsIn", enabled = true, speed = 4, bezier = "snappy", style = "popin 80%" })
hl.animation("workspaces", { speed = 5, curve = "snappy", style = "slidevert" })  -- the same form
```

- Curves are cubic beziers (overshoot allowed). `type = "spring"` is not supported: it
  becomes `easeOutQuint` with a warning. Predefined: `default`, `linear`, `easeOutQuint`,
  `easeInOutCubic`, `almostLinear`, `quick`.
- `speed` is in deciseconds (Hyprland's unit: 4 = 400 ms).
- Leaves played here, in Hyprland's tree (a leaf not set takes its parent's settings, an empty
  style its parent's style): `global` > `windows` > `windowsIn`, `windowsOut`, `windowsMove`;
  `global` > `fade` > `fadeIn`, `fadeOut`, `fadeSwitch`; `global` > `border`; `global` >
  `workspaces` > `workspacesIn`, `workspacesOut`; `global` > `layers` > `layersIn`,
  `layersOut` (the bar sliding in and out). Other Hyprland leaves (`fadeLayers*`,
  `specialWorkspace*`, `zoomFactor`, ...) warn.
- Window styles: `popin N%`, `slide [left|right|top|bottom]`, `fade`. Workspace styles:
  `slide`, `slidevert`, `fade`, `slidefade N%`, `slidefadevert N%`. Layers (the bar): `fade`
  fades, anything else slides from the top.

Defaults (Hyprland's example configuration, but a slide for workspaces):
`global 10 default`, `border 5.39 easeOutQuint`, `windows 4.79 easeOutQuint`,
`windowsIn 4.1 easeOutQuint popin 87%`, `windowsOut 1.49 linear popin 87%`,
`fadeIn 1.73 almostLinear`, `fadeOut 1.46 almostLinear`, `fade 3.03 quick`,
`workspaces 2.5 easeOutQuint slide`, `layers 3.81 easeOutQuint slide`, `layersIn 4
easeOutQuint`, `layersOut 1.5 linear` (Hyprland's example fades layers; the bar slides here).

## Binds

```lua
local mainMod = "ALT"
hl.bind(mainMod .. " + Return", hl.dsp.exec_cmd("com.termux"))
hl.bind(mainMod .. " + SHIFT + 3", hl.dsp.window.move({ workspace = 3 }))
hl.bind(mainMod .. " + B", function() hl.notify("hello") end)   -- a Lua function
local b = hl.bind("CTRL ALT, T", hl.dsp.exec_cmd("com.termux"))  -- Hyprland's old key syntax works too
b:set_enabled(false)                                               -- also b:remove(), b:is_enabled()
hl.unbind(mainMod .. " + J")
hl.bind(mainMod .. " + mouse:272", hl.dsp.window.drag(),   { mouse = true })
hl.bind(mainMod .. " + mouse:273", hl.dsp.window.resize(), { mouse = true })
```

- Keys: modifiers and one key joined by `+`. Key names are xkb's as Hyprland uses them, any
  case: letters, digits, `F1`..`F12`, `Return`, `Escape`, `Tab`, `space`, `grave`, `left`/
  `right`/`up`/`down`, `Home`, `End`, `Prior`/`Next`, `Delete`, `BackSpace`, `minus`, `equal`,
  `bracketleft`, `bracketright`, `comma`, `period`, `slash`, `semicolon`, `apostrophe`,
  `Print`, the keypad (`KP_0`..`KP_9`, and with Num Lock off `KP_End`, `KP_Down`, `KP_Next`,
  `KP_Left`, `KP_Begin`, `KP_Right`, `KP_Home`, `KP_Up`, `KP_Prior`, `KP_Insert` for
  Android's `NUMPAD_1`..`NUMPAD_0`, which it sends either way), `KP_Enter`, the media keys
  (`XF86AudioRaiseVolume`, `XF86AudioLowerVolume`, `XF86AudioMute`, `XF86AudioPlay`,
  `XF86AudioNext`, `XF86AudioPrev`, `XF86AudioStop`, `XF86MonBrightnessUp`/`Down`), ..., or
  any Android key code name (`KEYCODE_MEDIA_PLAY_PAUSE` or `MEDIA_PLAY_PAUSE`). An unknown key
  is an error. Android handles the plain media keys itself (volume dialog, media session,
  brightness dialog, holding repeats) and InputManager refuses binds on the volume and media
  keys, so leave them unbound (E11).
- A bind on keys that already have one replaces it (the defaults included); `hl.unbind` and a
  disabled bind leave the keys free. At most 256 binds are installed.
- Options: `description` is kept; `repeating`, `locked`, `release`, `non_consuming`, ... warn
  (InputManager's key gestures fire once, on release of the trigger).
- Binds are InputManager custom key gestures (docs/hypr-desktop.md, "Keybinds without a
  framework patch"): keys that a user shortcut or AOSP (all META shortcuts) already
  uses are skipped with a log line in SystemUI (`HyprKeybinds`). `ALT + Tab`, `ALT + SHIFT +
  Tab` and `SUPER + Tab` are InputManager's recents gestures, handled in HyprLauncher: `ALT +
  Tab` belongs to `shell.alt_tab` (E4, "ALT + Tab" below), and a bind on it runs only with
  `alt_tab = "bind"`; a bind on `SUPER + Tab` runs when Tab goes down (E3, docs/hypr-desktop.md,
  "ALT + Tab"). With `main_mod = "SUPER"`
  five default binds hit AOSP's own META shortcuts and are skipped: `SUPER + E` (email),
  `+ C` (calendar), `+ F` (files) are AOSP's app bookmarks, `+ SPACE` the assistant and
  `+ left` BACK; bind those dispatchers to other keys (below, "Waydroid and the host's SUPER
  key"). D5 saw `ALT + S` minimize a window on Waydroid; in D7 it did nothing without a game
  pad passed into the container, so it was the pad (docs/hypr-desktop.md, D7). `ALT + S` is
  the special workspace's toggle in the host-style binds (E5).
- Mouse binds (E6, next section): `mouse:272`..`mouse:276` and the wheel, `mouse_up`,
  `mouse_down`, `mouse_left`, `mouse_right`, with any modifiers and any dispatcher.

## Mouse binds

```lua
hl.bind("ALT + mouse:272", hl.dsp.window.drag(), { mouse = true })    -- bindm movewindow
hl.bind("ALT + mouse:273", hl.dsp.window.resize(), { mouse = true })  -- bindm resizewindow
hl.bind("ALT + mouse_down", hl.dsp.focus({ workspace = "e+1" }))
hl.bind("ALT + mouse_up", hl.dsp.focus({ workspace = "e-1" }))
hl.bind("CTRL + mouse:274", function() hl.notify("middle") end, { non_consuming = true })
hl.bind("ALT + mouse:275", hl.dsp.overview())                         -- side button (back)
hl.config({ binds = { pass_mouse_when_bound = false, scroll_event_delay = 300 } })
```

| Key | Input |
|---|---|
| `mouse:272`, `mouse:273`, `mouse:274` | left, right, middle button |
| `mouse:275`, `mouse:276` | side buttons back (BTN_SIDE) and forward (BTN_EXTRA) |
| `mouse_up`, `mouse_down` | one wheel step away from you / towards you |
| `mouse_left`, `mouse_right` | wheel tilt (horizontal scroll) |

- Modifiers must match exactly, as for keys; none is allowed (a bare `mouse_down` bind takes
  every wheel step from the apps).
- A button bind fires on the press; the app under the pointer gets a cancel instead of the
  click, unless the bind is `non_consuming` or `binds.pass_mouse_when_bound` is true. A wheel
  bind fires per wheel step and the app does not get the step (same exceptions). Unbound
  combinations reach the app unchanged.
- `binds.scroll_event_delay` (ms, default 300, Hyprland's): a wheel bind fires at most once
  in that time; steps in between are still consumed. `repeating = true` on a wheel bind fires
  on every step.
- `{ mouse = true }` with `hl.dsp.window.drag()` / `resize()` on `mouse:272`..`274` is
  Hyprland's `bindm`: the window under the pointer follows it while the button is held.
  `hl.dsp.window.float()` on a button toggles the window under the pointer; other dispatchers
  act on the focused window, as key binds do (clicking does not focus first).
- The side buttons reach Android as BACK and FORWARD keys, so their binds are key binds: they
  also catch a keyboard's BACK/FORWARD keys, are always consumed, and need the key free in
  InputManager (as any key bind).
- Defaults (`shell.default_binds`): `main_mod + mouse:272` drag, `+ mouse:273` resize,
  `+ mouse:274` float, the binds of Hyprland's example configuration; `hl.unbind` frees them.
- Other options (`release`, `locked`, ...) warn and are ignored.

Default binds (`shell.default_binds`, `$mod` = `shell.main_mod`):

| Keys | Dispatcher |
|---|---|
| `$mod + 1..9`, `$mod + 0` (10) | `hl.dsp.focus({ workspace = N })` |
| `$mod + SHIFT + 1..9`, `0` | `hl.dsp.window.move({ workspace = N })` |
| `$mod + arrows` | `hl.dsp.focus({ direction = ... })` |
| `$mod + SHIFT + arrows` | `hl.dsp.window.move({ direction = ... })` |
| `$mod + C` | `hl.dsp.window.close()` |
| `$mod + V` | `hl.dsp.window.float()` |
| `$mod + J` | `hl.dsp.layout("togglesplit")` |
| `$mod + F` / `$mod + SHIFT + F` | `hl.dsp.window.fullscreen({ mode = "maximized" })` / `hl.dsp.window.fullscreen()` |
| `$mod + E` | `hl.dsp.exec_cmd(shell.file_manager)` |
| `$mod + Q` | `hl.dsp.exec_cmd(shell.terminal)`, when set |
| `$mod + D`, `$mod + SPACE` | `hl.dsp.launcher()` |
| `$mod + grave` | `hl.dsp.overview()` |
| `$mod + Tab` | `hl.dsp.focus({ last = true })`, the window focused before (E3; runs in HyprLauncher, see "Binds"). With the default `main_mod = "ALT"` the switcher (`shell.alt_tab`) owns the key and a quick tap does the same |

### ALT + Tab

`ALT + Tab` is InputManager's recents gesture (it starts when Tab goes down and ends when ALT
is released); HyprLauncher handles it as `shell.alt_tab` says (Phase E4, docs/hypr-desktop.md,
"ALT + Tab switcher"). With the default `"switcher"`:

| Keys | What |
|---|---|
| `ALT + Tab` tapped | the window focused before (as `focuscurrentorlast`), no strip shown |
| hold `ALT`, `Tab` again | a strip of the display's windows (icon, title, workspace; most recently focused first) shows; each Tab selects the next one, deeper into the history |
| `SHIFT + Tab` (or `grave`, left/right) while `ALT` is held | back / forward |
| release `ALT` | focus the selected window (its workspace becomes active, it comes to front) |
| `Esc` while `ALT` is held, a click outside | close without switching; a click on a window focuses it |

The first press cannot go backwards: InputManager reports `ALT + SHIFT + Tab` as `ALT + Tab`.
Binds on `ALT + <key>` still run while the strip is up (with the example binds `ALT + Escape`
is the user switcher, so there Esc does not close the strip: step back to the current window,
the first entry, and release ALT, or click outside).
`"cycle"` gives the host's own `ALT + TAB` (`cyclenext`, which brings the window to front as
the host's `alterzorder top`, on every Tab while ALT is held; `SHIFT + Tab` goes back); `"bind"`
runs your bind on `ALT + Tab` (and on `ALT + SHIFT + Tab` for SHIFT) on every Tab, e.g.
`hl.bind("ALT + Tab", hl.dsp.window.cycle_next({ hist = true }))`. On Waydroid the host's
compositor keeps `ALT + TAB`, so the switcher is reached there only with injected keys (`keys`
of `aosp-device/hypr/tools/touchinject`); on the real PC no compositor sits in front and it
works with the keyboard.

With `shell.default_binds` the default touch gestures are bound too (next section).

## Waydroid and the host's SUPER key

Under Waydroid the host's compositor sees every key first. With the default `main_mod = "ALT"`
the binds reach Android, except what the host binds itself: on this host's Hyprland that is
`ALT + TAB` (window cycling), `ALT + F2`/`F3`/`F11`/`F12` and `CTRL + ALT + P`
(`hyprctl binds`), so in the Waydroid window `ALT + Tab` (by default the switcher) does
not reach Android; it was tested with injected keys inside the container. To use SUPER as in Hyprland, set it here and let the host pass SUPER through while
the Waydroid window has focus:

```lua
-- init.lua (Android)
local mainMod = "SUPER"
hl.config({ shell = { main_mod = mainMod } })
-- the defaults AOSP keeps for itself (E, C, F, SPACE, left), on other keys:
hl.bind(mainMod .. " + W", hl.dsp.window.close())
hl.bind(mainMod .. " + R", hl.dsp.launcher())
hl.bind(mainMod .. " + Z", hl.dsp.window.fullscreen({ mode = "maximized" }))
hl.bind(mainMod .. " + O", hl.dsp.exec_cmd("com.android.documentsui"))
hl.bind(mainMod .. " + comma", hl.dsp.focus({ direction = "left" }))
```

On the host, a submap with no binds but its own way out, entered whenever the Waydroid window
is focused (Hyprland 0.57 Lua; add it to the host's own configuration, e.g. a new file
`require`d from `~/.config/hypr/hyprland.lua`; this repo does not edit it):

```lua
-- ~/.config/hypr/... (host): SUPER goes to Waydroid while its window has focus
hl.define_submap("waydroid", function()
    hl.bind("SUPER + ALT + Escape", hl.dsp.submap("reset"))   -- leave without unfocusing
end)
hl.on("window.active", function()
    local w = hl.get_active_window()
    local inWaydroid = w ~= nil and w.class == "Waydroid"
    local current = hl.get_current_submap()
    if inWaydroid and current ~= "waydroid" then
        hl.dispatch(hl.dsp.submap("waydroid"))
    elseif not inWaydroid and current == "waydroid" then
        hl.dispatch(hl.dsp.submap("reset"))
    end
end)
```

In the submap the host's own SUPER binds (workspaces, terminal, ...) are off, so switch away
with the mouse, `SUPER + ALT + Escape` and then the usual bind, or a bind added inside the
submap (for example `hl.bind("SUPER + ALT + 1", hl.dsp.focus({ workspace = 1 }))`). Waydroid's
window class is `Waydroid` (`hyprctl clients`; its multi-window mode uses one class per app
instead). Checked: `hl.define_submap` with a bind inside loads through `hyprctl eval` on
Hyprland 0.57.2 and the bind lists under its submap; on the Android side this init.lua loads
with 35 binds installed (the five defaults skipped), and `META + 2`, `META + comma` and
`META + W` switch workspace, move focus and close.

SUPER keys AOSP keeps (system shortcuts and app bookmarks, checked by binding every key on
Waydroid): `A B C E F H I L M N P Q S U`, `Tab`, `Return`, `Escape`, `slash`, `space` and
`left`. Free: `D G J K O R T V W X Y Z`, digits, `grave`, `comma`, `period`, `right`, `up`,
`down`; with SHIFT the ones tried were free (`SUPER + SHIFT + Q`, `+ S`, `+ 1`, `+ left`),
with CTRL `SUPER + CTRL + left` is AOSP's (split-screen navigation).

## Example: binds modelled on a host Hyprland configuration

`docs/examples/hypr-binds.lua` carries a desktop Hyprland configuration's binds over with
`main_mod` kept at ALT: the host's `SUPER` binds are on `ALT`. Copy it next to init.lua as
`binds.lua` and add `require("binds")` at the end of init.lua.

| Keys | Action |
|---|---|
| `ALT + Q` | close the focused window (instead of the default `ALT + C`) |
| `ALT + F` / `ALT + SHIFT + F` | fullscreen / maximize (the default's order swapped, as on the host) |
| `ALT + Space` | float toggle (`ALT + V` still works); `ALT + D` opens the launcher |
| `ALT + CTRL + Space` | pin toggle (a tiled window is floated first) |
| `ALT + CTRL + SHIFT + F` | float, fill the work area and pin: a Lua function with `hl.get_active_monitor().work_area`, `resize`/`move` with `relative = false` and `pin` (the host uses the monitor's size; here the bar stays visible) |
| `ALT + Return`, `+ W`, `+ E` | Termux, Chromium, Files |
| `ALT + P` | the system monitor (HyprTop) on workspace 5, as the host's btop (Phase E8) |
| `ALT + SHIFT + C` | the calculator, floating (a window rule on `com.android.calculator2`; the host's `MOD3 + C`) |
| `ALT + L`, `ALT + R` | notifications, quick settings (the host's left and right panels) |
| `ALT + arrows`, `ALT + h n c t` | focus left, right, up, down |
| `ALT + CTRL + arrows`, `ALT + CTRL + h n c t` | move the window |
| `ALT + SHIFT + arrows` | resize by 40 px |
| `ALT + 1..9, 0`, `+ CTRL + ...`, `+ SHIFT + ...` | workspaces 1-10: switch, move the window and follow, move it silently |
| `ALT + KP_End..KP_Insert`, `+ CTRL`, `+ SHIFT` | workspaces 11-20 on the keypad (Android's `NUMPAD_1..NUMPAD_0`), the same three (E2; the file sets `shell.workspaces = 20`) |
| `ALT + wheel down / up` | next / previous workspace (E6) |
| `ALT + Tab` | the switcher (`shell.alt_tab`, E4): tapped, the window focused before (the host's `SUPER + TAB`); held, walk the focus history, release to switch. The file's `ALT + Tab` bind is what `alt_tab = "bind"` would run. Not under Waydroid, where the host keeps `ALT + TAB` |
| `ALT + grave`, `ALT + SHIFT + grave` | the previous workspace (the host's `SUPER + GRAVE`; `binds.workspace_back_and_forth` too), the overview (E3) |
| `ALT + B`, `ALT + SHIFT + B` | hide/show the bar, re-create it (Phase E7) |
| `ALT + S`, `ALT + CTRL + S` | the special workspace over the current one, toggled; the focused window into it (again: back out; Phase E5) |
| `ALT + SHIFT + equal / minus`, `ALT + SHIFT + wheel down / up`, `ALT + SHIFT + middle button` | zoom in / out by 0.5, by 0.2, reset (Phase E10) |

Left out, with no counterpart here: screenshots and screen
recording, panel widgets, lock and suspend. The shell's default `ALT + grave` (overview)
moves to `ALT + SHIFT + grave` in this file, as the host puts the previous workspace there.
With the later steps' binds (E1's pin binds among them) the user's copy loaded with 81 binds on
Waydroid (2026-11-22; the only warnings: E10's zoom binds, `repeating` ignored). Before them it
loaded with no warnings and 67 binds (60 keys and the 7 default gestures) on the host build of
hyprlua.
Checked on Waydroid (2026-11-22, user data): all 60 key binds installed, none taken by AOSP;
open, focus (`h`/`n`), move, resize, float, maximize, silent and following moves, the shade
(`ALT + L` and `ALT + R` open SystemUI's large-screen shade, quick settings and
notifications side by side) and close all work.

## Touch gestures

```lua
hl.bind("gesture:edge_bottom", hl.dsp.launcher())
hl.bind("gesture:swipe_4_down", function() hl.notify("four fingers") end)
hl.unbind("gesture:edge_top_right")          -- the whole top edge opens notifications
hl.config({ shell = { touch = true } })
```

A gesture is bound like a key: `gesture:<name>`, no modifiers, any dispatcher or a Lua
function. `hl.unbind` frees it (an unbound gesture leaves the touch to the app).

| Gesture | What | Default |
|---|---|---|
| `edge_bottom` | one finger swiping up from the bottom edge | `hl.dsp.launcher()` |
| `edge_left` | from the left edge, to the right | `hl.dsp.focus({ workspace = "e-1" })` |
| `edge_right` | from the right edge, to the left | `hl.dsp.focus({ workspace = "e+1" })` |
| `edge_top` | from the top edge, down | `hl.dsp.notifications()` |
| `edge_top_right` | from the right third of the top edge (when unbound, `edge_top`) | `hl.dsp.quick_settings()` |
| `swipe_<N>_<dir>` | N = 3, 4 or 5 fingers swiping `up`, `down`, `left` or `right`, anywhere | `swipe_3_up`: `hl.dsp.overview()` |
| `pinch_in`, `pinch_out` | two fingers on the wallpaper or the bar (on a window a pinch is the app's zoom) | `pinch_in`: `hl.dsp.overview()` |

- An edge swipe starts within 20 dp of the edge and goes 72 dp inward within 0.7 s. Where an
  app asked for priority (`View.setSystemGestureExclusionRects`, as WindowManager limits it on
  the side edges) there is no edge swipe and no other gesture. The lock screen takes only
  `edge_top`/`edge_top_right`.
- `hl.dsp.notifications()` and `hl.dsp.quick_settings()` open SystemUI's shade (also usable on
  keys).
- Not a bind: a long-press (350 ms, `persist.hypr.touch_hold`) on a window's frame (14 dp inside
  its edges, the gap next to it, a floating window's caption) picks the window up. Dragging it
  swaps it with the tiled window under the finger (a floating window lands where it is
  dropped, or joins the layout when dropped on the left or right screen edge); releasing
  without moving opens the window menu: Float/Tile, Maximize (tiled), Fullscreen, Workspace
  1..N (moves it there, the view stays), Close.
- `hl.gesture` lines (Hyprland's touchpad gestures) become these binds: `fingers` 3..5 with
  `up`, `down`, `left`, `right`, `horizontal`, `vertical` or `swipe`, or `fingers = 2` with
  `pinch`/`pinchin`/`pinchout`; action `workspace` (swipe left or up: next workspace, right or
  down: previous), `close`, `fullscreen`, `float`, a dispatcher or a function. Swipes are
  discrete (the action runs once the swipe is recognized; the workspace does not follow the
  finger).

## Dispatchers: `hl.dsp`

A dispatcher is a value for `hl.bind`, `hl.dispatch`, `on_click` and gestures. Hyprland
dispatchers this shell does not have (`hl.dsp.workspace.*` other than `toggle_special`,
`hl.dsp.group.*`, `hl.dsp.cursor.*`, ...) are an error where they are used.

| Lua | Hyprland's name | What |
|---|---|---|
| `hl.dsp.exec_raw("cmd")` | `execr` | a shell command (`/system/bin/sh -c`, as SystemUI), e.g. `"hyprctl notify 1 3000 0 hi"` (docs/hypr-ipc.md) |
| `hl.dsp.focus({ workspace = N })` | `workspace` | switch to workspace N (1..`shell.workspaces`, made if it does not exist); `"e+1"`/`"e-1"` the next/previous workspace that exists (wrapping), `"+1"`/`"-1"` (`"r+1"`) the next/previous number |
| `hl.dsp.focus({ direction = "left" })` | `movefocus` | focus the neighbour (`left`, `right`, `up`, `down`, or `l r u d`) |
| `hl.dsp.focus({ window = w })` | `focuswindow` | a window from `hl.get_windows()`, a task id, or a selector (`"class:^org\\.chromium"`, `"title:..."`, `"initialclass:"`, `"initialtitle:"`, `"pid:N"`, `"address:0x<hex task id>"`, `"taskid:N"`, `"activewindow"`, `"floating"`, `"tiled"`; `negative:` inverts a regex): its workspace, to front |
| `hl.dsp.focus({ workspace = "previous" })` | `workspace previous` | the workspace before this one (Hyprland 0.57's most-recently-used list; again to come back); `"previous_per_monitor"` the one before on this display; also for `window.move` |
| `hl.dsp.focus({ last = true })` | `focuscurrentorlast` | the window focused before this one (across workspaces); default on `$mod + Tab` |
| `hl.dsp.focus({ urgent_or_last = true })` | `focusurgentorlast` | the newest urgent window (an unfocused window whose app posted an alerting notification), else the last one |
| `hl.dsp.window.cycle_next({ next = false, tiled = true, floating = true, hist = true, visible = true })` | `cyclenext prev tiled ...` | the next (previous) window of the workspace in opening order; `tiled`/`floating` only those, `hist` in focus history order, `visible` every display's active workspace; `hl.dsp.raw("cycleprev")` goes back |
| `hl.dsp.window.swap({ next = true })`, `({ prev = true })` | `swapnext [prev]` | the focused tiled window swaps with the next (previous) tiled one; pressed again it walks on |
| `hl.dsp.window.swap({ target = w })` | `swapwindow` | swap the focused tiled window with that one (also `with =`, `other =`) |
| `hl.dsp.window.alter_zorder({ mode = "top" \| "bottom", window = w })` | `alterzorder` | raise the window (it takes the focus) or put it below the other windows of its workspace |
| `hl.dsp.window.move({ workspace = N })` | `movetoworkspace` | move the focused window there and follow it |
| `hl.dsp.window.move({ workspace = N, follow = false })` | `movetoworkspacesilent` | move it without following (also `silent = true`) |
| `hl.dsp.workspace.toggle_special()`, `("name")` | `togglespecialworkspace [name]` | show the special workspace (`special`, or `special:name`) over the active workspace, or hide it (E5, below) |
| `hl.dsp.focus({ workspace = "special:name" })` | `workspace special:name` | the same toggle |
| `hl.dsp.window.move({ workspace = "special[:name]" })` | `movetoworkspace special[:name]` | move the focused window to that special workspace and show it; a window already there goes back to the active workspace; `follow = false` (`movetoworkspacesilent`) moves it there hidden |
| `hl.dsp.window.move({ direction = "left" })` | `movewindow` | tiled: swap with the neighbour; floating: to that edge |
| `hl.dsp.window.swap({ direction = "left" })` | `movewindow` | the same |
| `hl.dsp.window.float({ action = "toggle" })` | `togglefloating` | also `"on"` (`setfloating`), `"off"` (`settiled`); `window = w` for another window than the focused one |
| `hl.dsp.window.pin({ action = "toggle" })` | `pin` | a pinned window floats (a tiled one leaves its layout, centred), stays on top of the other windows and shows on every workspace (it follows the active one); `"on"`, `"off"`, `window = w`; unpinned it stays floating |
| `hl.dsp.window.fullscreen({ mode = "maximized" })` | `fullscreen 1` | fill the work area, keep the tile; again to go back |
| `hl.dsp.window.fullscreen()` | `fullscreen 0` | Android's fullscreen, out of the desktop and back |
| `hl.dsp.window.close()` | `killactive` | close the focused window |
| `hl.dsp.window.kill()` | `forcekillactive` | force-stop the focused window's app, as Settings' Force stop (E11; the host's `hyprctl kill` picks a window by click) |
| `hl.dsp.window.resize({ x = 40, y = 0 })` | `resizeactive` | tiled: move its splits; floating: grow (px) |
| `hl.dsp.window.resize({ x = W, y = H, relative = false })` | `resizeactive exact` | floating: that size (px, or `"N%"` of the work area), the top-left corner stays |
| `hl.dsp.window.move({ x = X, y = Y, relative = false })` | `moveactive exact` | floating: to that display position (px, or `"N%"` across the work area); without `relative = false` it moves by X, Y |
| `hl.dsp.layout("togglesplit")` | `togglesplit` | dwindle: flip the focused window's split |
| `hl.dsp.exec_cmd("com.termux")` | `exec` | start a package's launcher activity (a package name, not a shell command); it opens on the active workspace |
| `hl.dsp.exec_cmd("com.android.hyprtop", { workspace = 5 })` | `exec [workspace 5] pkg` | Hyprland's exec rules: the package's next window (within 30 s) opens on workspace 5 and is followed; `silent = true` leaves it there (`[workspace 5 silent]`); a rule string works too. Other rules warn: use `hl.window_rule` |
| `hl.dsp.exec_cmd("com.termux", { float = true })` | `exec [float] pkg` | the package's next window opens floating (`float = false`: tiled), with `workspace` too (`[workspace 2; float]`; E11) |
| `hl.dsp.launcher()` | `launcher` | the rofi-like app launcher |
| `hl.dsp.overview()` | `overview` | the hyprexpo-like overview |
| `hl.dsp.notifications()` | `notifications` | SystemUI's notification shade |
| `hl.dsp.quick_settings()` | `quicksettings` | SystemUI's quick settings |
| `hl.dsp.bar.toggle()`, `.show()`, `.hide()` | `bar toggle\|show\|hide` | the bar; hidden, it gives its strip back to the windows (waybar's `SIGUSR1`) |
| `hl.dsp.bar.reveal()` | `bar reveal` | an auto-hiding bar comes out for 3 s (or `autohide_delay`, if longer) |
| `hl.dsp.bar.reload()` | `bar reload` | re-creates the bar from the configuration in force (its views, Lua modules and configured visibility), without running init.lua again |
| `hl.dsp.zoom({ delta = 0.5 })` | `zoom +0.5` | zoom the whole screen around the pointer by that much (negative: out), between 1 and 8 (Phase E10, ours: Hyprland zooms with `cursor:zoom_factor`) |
| `hl.dsp.zoom({ factor = 1 })` | `zoom 1` | set the zoom factor; 1 is no zoom |
| `hl.dsp.clipboard_history()` | `clipboardhistory` | SystemUI's clipboard history picker (Phase E9, ours): type to filter, Enter pastes, SHIFT + Enter copies, Delete removes; text only, in memory, sensitive clips never kept |
| `hl.dsp.translate()` | `translate` | the focused app's selection (copied with CTRL + C), or else the clipboard's text, to Google Translate's floating popup; a notification when the app is not installed (E9) |
| `hl.dsp.screenshot({ mode = "area" })` | `screenshot area` | SystemUI's region screenshot (its large-screen pre-capture UI); `mode = "full"` (`screenshot full`) the whole screen with the usual preview and editor (E9) |
| `hl.dsp.lock()` | `lock` | the lock screen now, as the power menu's Lock (E9) |
| `hl.dsp.user_switcher()` | `userswitcher` | SystemUI's user switcher dialog, also with one user (E9) |
| `hl.dsp.volume("up")`, `"down"`, `"mute"` | `volume up\|down\|mute` | the VOLUME key, as a keyboard sends it: AudioService and SystemUI's volume dialog (E11; the host runs wpctl) |
| `hl.dsp.brightness("up")`, `"down"` | `brightness up\|down` | the BRIGHTNESS key: Android's brightness step and its brightness dialog (E11) |
| `hl.dsp.media("play_pause")`, `"next"`, `"previous"`, `"stop"` | `media ...` | the media key, to the active media session (E11; the host runs playerctl) |
| `hl.dsp.input_method("next")`, `"previous"` | `inputmethod next\|previous` | the LANGUAGE_SWITCH key: the input method's next keyboard layout or subtype (E11; fcitx5-remote -t) |
| `hl.dsp.display("extend")`, `"mirror"`, `"primary"`, `"external"` | `display ...` | external displays on (each its own desktop, or mirroring the default display), off, or alone with the default display powered off (E11; docs/hypr-desktop.md) |
| `hl.dsp.reload()` | `reload` | `hyprctl reload`: init.lua and its modules again, now |
| `hl.dsp.wallpaper({ workspace = "all", dir = "/sdcard/Pictures/Wallpapers" })` | `wallpaper random all [dir]` | a random picture of the folder becomes the wallpaper; one wallpaper for every workspace, so `"current"` does the same |
| `hl.dsp.raw(name, arg)` | any of the above | by Hyprland's name: `hl.dsp.raw("movefocus", "l")` |
| a Lua function | `lua` | runs in the launcher's Lua state |

`hl.dispatch(dsp)` runs one now; `hl.exec_cmd("pkg"[, rules])` is `hl.dispatch(hl.dsp.exec_cmd("pkg"[, rules]))`.

## Window rules

```lua
hl.window_rule({ name = "firefox-web", match = { class = "^org\\.mozilla\\.firefox$" }, workspace = "2" })
hl.window_rule({ match = { class = "calculator" }, float = true })
hl.window_rule({ match = { title = "Picture-in-picture" }, float = true, opacity = "0.9 0.7" })
local r = hl.window_rule({ match = { class = "termux" }, workspace = "3 silent" })
r:set_enabled(false)
```

| Field | What |
|---|---|
| `match.class` (`initial_class`) | Java regular expression on the package name, searched (`firefox` matches `org.mozilla.firefox`; anchor with `^...$`). In a Lua string a regex backslash is `\\` |
| `match.title` (`initial_title`) | regular expression on the window title (the task's label) |
| `float = true` / `tile = true` (`float = false`) | float or tile when it opens; a rule wins over the float defaults (dialogs, translucent activities), but a window that cannot be resized always floats |
| `workspace = "N"` / `"N silent"` | open on workspace N (1..1000, up to `shell.workspaces`): moved there and followed, or moved without following |
| `workspace = "special[:name]"` / `"special:name silent"` | open on that special workspace, shown, or hidden in it (E5) |
| `opacity = "A [I]"` | active and inactive opacity of the window (`override` words are ignored) |
| `name`, `enabled` | a name for messages; `enabled = false` keeps it off (also `r:set_enabled`) |

Matching on anything else (`xwayland`, `float`, `workspace`, ...) is an error; other effects
(`pin`, `size`, `move`, `no_focus`, `suppress_event`, ...) warn. When several rules match,
the last one that sets an effect wins. The workspace effect applies once, to windows opened
after the rule; opacity and floating are checked on every layout pass.

## Workspace rules

```lua
hl.workspace_rule({ workspace = "3", layout = "master", gaps_in = 0, gaps_out = 0, default_name = "code" })
hl.workspace_rule({ workspace = "4", on_created_empty = "com.termux" })
```

| Field | What |
|---|---|
| `workspace` | 1..1000, or `special[:name]` (E5: `layout`, `gaps_in`, `gaps_out` apply to it); Hyprland's selectors such as `w[tv1]` are an error |
| `layout`, `gaps_in`, `gaps_out` | that workspace's own layout and gaps (whole pixels) |
| `default_name` (`name`) | shown in the bar and given to Lua as `ws.name` |
| `on_created_empty` | a package started when you switch to the workspace and it has no window |

Several rules for one workspace merge, later fields winning. Other fields warn.

## Special workspaces

Hyprland's special workspaces (scratchpads), Phase E5: `special` (Hyprland's unnamed one,
`special:special`) and any `special:name`. One shows over the active workspace of its display,
toggled on and off without leaving that workspace; its windows tile in a layout of its own
(own `hl.workspace_rule` gaps and layout, `special_scale_factor`), stay above the workspace's
windows, and those windows are dimmed (`decoration.dim_special`). Windows moved to a special
workspace disappear from their workspace. While one is shown, new windows open on it, as on
the focused workspace in Hyprland; it stays shown over another workspace when you switch
(unless `binds.hide_special_on_workspace_change`), hides when its last window closes, and a
window of a hidden one that comes to front (its app started again, `focus({ window = })`, the
switcher) shows it. One special workspace per display at a time.

```lua
hl.bind("ALT + S", hl.dsp.workspace.toggle_special())                     -- special
hl.bind("ALT + CTRL + S", hl.dsp.window.move({ workspace = "special" }))  -- again: back out
hl.bind("ALT + T", hl.dsp.workspace.toggle_special("term"))
hl.window_rule({ match = { class = "^com\\.termux$" }, workspace = "special:term silent" })
hl.workspace_rule({ workspace = "special:term", gaps_out = 60 })
```

The bar shows `S` (or `S:name`) after the numbered workspaces for each special workspace with
windows or shown, highlighted when shown; a click toggles it. The overview has a cell for each.
`hl.get_workspaces()` lists them after the numbered ones (`special = true`, `id` negative as in
Hyprland: -99, -98, ...; `name = "special:name"`), and a window on one has that workspace.

## Gestures and monitors

```lua
hl.gesture({ fingers = 3, direction = "horizontal", action = "workspace" })
hl.gesture({ fingers = 4, direction = "up", action = hl.dsp.overview() })
hl.monitor({ output = "", reserved = { top = 0, bottom = 40 } })
```

- `hl.gesture`: `fingers` 2..5, `direction` `left`, `right`, `up`, `down`, `horizontal`,
  `vertical`, `swipe`, `pinch`, `pinchin`, `pinchout`, `action` a Hyprland gesture action name,
  a dispatcher or a function. Played as touch gesture binds ("Touch gestures" above); two-finger
  swipes (app scrolling) and `mods` are errors.
- `hl.monitor`: only `reserved`/`reserved_area` (a number or `{ top, bottom }`, px) is used,
  space kept free besides the bar; `mode`, `position`, `scale`, ... are Android's and warn.

## The bar

```lua
hl.bar({
    modules_left   = { "apps", "workspaces" },
    modules_center = { "title" },
    modules_right  = { "windows", "battery", "tray", "clock" },
    clock_format   = "EEE d MMM  HH:mm",
    height = 30,                       -- dp, 16..64
    background = "rgba(1e1e2ee6)", foreground = "#cdd6f4", accent = "#33ccff",
    autohide = false,                  -- true: hidden until the pointer reaches the top edge
    autohide_delay = 500,              -- ms the auto-hiding bar stays after the pointer left
})

hl.bar_module("battery", {
    text = function() return "⚡ " .. hl.date("%H:%M") end,  -- or a fixed string
    interval = 5000,                                           -- ms, 250 or more (default 1000)
    on_click = hl.dsp.exec_cmd("com.android.settings"),       -- a dispatcher or a function
    tooltip = "Battery",
})
```

Built-in modules: `apps` (the launcher button), `workspaces` (the active and the occupied
workspaces, as waybar's `hyprland/workspaces`; click to switch, scroll to step through them;
names from workspace rules), `title` (the focused window), `windows` (running apps,
click to focus), `notifications` and `quicksettings` (the shade), `tray` (both), `clock`
(`clock_format` is an Android date pattern: `HH:mm`, `EEE d MMM`, ...). The centre group takes
the room the others leave. An unknown module name is an error.

Hiding and auto-hide (Phase E7). `hl.dsp.bar.hide()` / `.show()` / `.toggle()` (the
example binds: `ALT + B`) take the bar away and give its strip back: the windows re-tile
into the whole display and apps lose the status bar inset. With `autohide = true` the bar
starts hidden; when the pointer touches the top edge (a 2 dp strip the hidden bar keeps) it
slides in over the windows, without moving them, and slides out `autohide_delay` ms after
the pointer left it. A touch on that strip brings it out too, for 3 s. Showing an
auto-hiding bar with `bar show` (or `toggle`) pins it: it reserves its strip again until
`bar hide`. The slide follows the `layersIn`/`layersOut` animations. A swipe down from the
top edge stays `gesture:edge_top` (notifications); to have it bring the bar instead:
`hl.bind("gesture:edge_top", hl.dsp.bar.reveal())`. `hl.dsp.bar.reload()` (`ALT + SHIFT +
B`) re-creates the bar as a waybar restart does; a new configuration keeps the user's
hidden/shown choice unless `autohide` changed.

## Events

```lua
local sub = hl.on("window.open", function(w) print("opened " .. w.class .. " on " .. w.workspace.id) end)
hl.on("workspace.active", function(ws) hl.notify("Workspace " .. ws.name) end)
sub:remove()
```

| Event (Hyprland's socket name) | Argument |
|---|---|
| `window.open` (`openwindow`) | the window |
| `window.close` (`closewindow`) | `{ id = N }` (the window is gone) |
| `window.active` (`activewindow`) | the focused window |
| `workspace.active` (`workspace`) | the workspace |
| `config.reloaded` | none; after every successful load |
| `hyprland.start` | none; after the first successful load of the launcher's process (Hyprland's `exec-once` place) |

Events come from the shell's state 50 ms after it changes, so two changes in that time give
one event per kind. Each handler runs protected: one failing shows a notification and the
others still run, unless it hit the instruction limit, which ends that event.

A window: `{ id, address = "taskid:N", class, initial_class, title, initial_title, floating,
hidden (minimized), visible, active, at = { x, y }, size = { x, y }, workspace }`. A workspace:
`{ id, name, windows (count), is_empty, active, visible }`.

## Queries, notifications, timers

Scripts outside the configuration get the same information, and the dispatchers, through
Hyprland's IPC: `hyprctl` on the image and the `@hypr/.socket.sock` / `@hypr/.socket2.sock`
sockets (docs/hypr-ipc.md); `hyprctl eval` and `hyprctl repl` run Lua in this configuration's
state.

| Call | Returns |
|---|---|
| `hl.get_windows([{ class =, title =, floating =, workspace = }])` | windows, every workspace; `class`/`title` are Lua patterns |
| `hl.get_active_window()` | the focused window or nil |
| `hl.get_last_window()`, `hl.get_urgent_window()` | the window focused before the active one, the newest urgent one, or nil (E3) |
| `hl.get_last_workspace()` | the workspace `previous` goes to, or nil |
| `hl.get_window("taskid:N")` | one window or nil; windows carry `pinned` |
| `hl.get_active_monitor()`, `hl.get_monitors()` | the display: `width`, `height`, `x = 0`, `y = 0`, `scale = 1`, `reserved` (hyprctl's left, top, right, bottom, also by name: the bar and system bars) and `work_area` (`x`, `y`, `width`, `height`: the display without them); nil (an empty list) before the shell answers |
| `hl.get_workspaces()`, `hl.get_active_workspace()`, `hl.get_workspace(N)` | the workspaces that exist (as `hyprctl workspaces`: the active one and the ones with windows), the active one, any one by number |
| `hl.get_config(name)` | an option's value as a string |
| `hl.notify(text, { timeout = ms, color = "rgba(...)" })` | a notification under the bar (also `hl.notification.create({ text, timeout, color })`) |
| `hl.timer(fn, { timeout = ms, type = "repeat" \| "oneshot" })` | a timer (`:set_enabled(b)`, `:set_timeout(ms)`, `:is_enabled()`); 100 ms or more; all stop when the file reloads |
| `hl.version()` | `hypr-android 1 (Hyprland 0.57 Lua API subset)` |

`hl.config`, `hl.bind`, rules and the bar may also be called from hooks, binds and timers:
the configuration is sent to the shell again afterwards.

Ignored with a warning: `hl.env`, `hl.device`, `hl.permission`, `hl.layer_rule`. Not
supported (an error): `hl.define_submap`, `hl.layout.register`, `hl.plugin`. Any other
missing `hl.*` is an error at the line that uses it.

## Example configuration

```lua
-- /sdcard/Documents/hypr/init.lua
local mainMod = "ALT"
local terminal = "com.termux"

hl.config({
    general = {
        gaps_in = 4, gaps_out = 10, border_size = 3, layout = "dwindle",
        col = {
            active_border = { colors = { "rgba(cba6f7ff)", "rgba(89b4faff)" }, angle = 45 },
            inactive_border = "rgba(45475aaa)",
        },
    },
    decoration = { rounding = 12, inactive_opacity = 0.92 },
    dwindle = { preserve_split = true },
    shell = { main_mod = mainMod, terminal = terminal },
})

hl.curve("snappy", { type = "bezier", points = { {0.05, 0.9}, {0.1, 1.05} } })
hl.animation({ leaf = "windows",    speed = 4, bezier = "snappy", style = "popin 80%" })
hl.animation({ leaf = "workspaces", speed = 4, bezier = "snappy", style = "slidevert" })

require("binds")       -- binds.lua next to this file

hl.window_rule({ match = { class = "^org\\.mozilla" }, workspace = "2" })
hl.window_rule({ match = { class = "calculator" }, float = true })
hl.workspace_rule({ workspace = "2", default_name = "web" })
hl.workspace_rule({ workspace = "3", layout = "master", default_name = "code", on_created_empty = terminal })

hl.bar_module("uptime", {
    text = function() return string.format("up %d min", hl.clock() // 60) end,
    interval = 60000,
})
hl.bar({ modules_right = { "uptime", "windows", "tray", "clock" }, clock_format = "HH:mm" })

hl.on("workspace.active", function(ws)
    if ws.id == 9 then hl.notify("Workspace 9: scratch", { timeout = 1500 }) end
end)
hl.on("hyprland.start", function() hl.notify("hypr ready") end)
```

```lua
-- /sdcard/Documents/hypr/binds.lua
local mainMod = "ALT"
hl.bind(mainMod .. " + Return", hl.dsp.exec_cmd("com.termux"))
hl.bind(mainMod .. " + SHIFT + Q", hl.dsp.window.close())
hl.bind(mainMod .. " + minus", hl.dsp.window.resize({ x = -60, y = 0 }))
hl.bind(mainMod .. " + equal", hl.dsp.window.resize({ x = 60, y = 0 }))
hl.bind(mainMod .. " + G", function()
    local n = #hl.get_windows({ workspace = hl.get_active_workspace().id })
    hl.notify(n .. " windows here")
end)
```

## Implementation

| Piece | Where |
|---|---|
| Lua state, sandbox, JNI | `aosp-device/hypr/hyprlua/jni/hyprlua_jni.c` (`libhyprlua_jni`, on `:liblua-sources`), `LuaState.kt` |
| The `hl` API | `hyprlua/res/com/android/hyprlua/prelude.lua` |
| Lua → configuration | `hyprlua/src/.../ConfigBuilder.kt` (options, binds, rules, bar), `HyprLua.kt` (runtime: load, keep the last good state, hooks, timers) |
| Hyprland's option names | `hyprlua/res/.../hyprland-options.txt` (from Hyprland 0.57's `hl.meta.lua`) |
| Default init.lua | `hyprlua/res/.../default-init.lua` |
| In the launcher | `HyprLauncher/.../LuaConfig.kt` (file, reload, events, bar modules), `Notifier.kt` (error strip, notifications), `Bar.kt` |
| In the shell | `PcSystemUI/.../HyprTileController.kt` (tiling, window and workspace rules), `HyprStyleProps.kt` (decoration, animations), `HyprKeybinds.kt` (binds), `HyprMouse.kt` |
| Tests | `hyprlua/run-tests.sh` (real Lua on the host; `hyprlua-tests` under Soong), `hyprtile/run-tests.sh` (`RulesTest`) |

The configuration on the wire (`IHyprShell.setConfig`): string values under the
`persist.hypr.*` names without the prefix (`layout`, `gaps_in`, `gaps_out`, `border_size`,
`col.active_border`, `col.inactive_border`, `rounding`, `active_opacity`, `inactive_opacity`,
`shadow`, `caption`, `animations`, `split_width_multiplier`, `default_split_ratio`,
`preserve_split`, `force_split`, `mfact`, `new_status`, `new_on_top`, `mod`, `mouse_mod`,
`tiling`, `reserved_top`, `reserved_bottom`, `float_packages`, `source` = `lua`) and string
arrays `bezier` and `animation` (Hyprland's line syntax), `binds` (the whole list, defaults
included, `MODS, KEY, dispatcher[, arg]`), `windowrule`, `workspace`, `gesture` (fields
`key=value` separated by U+001F). A Lua configuration is always complete, so the
`persist.hypr.*` properties (D1-D3's stand-ins) only matter without one.
