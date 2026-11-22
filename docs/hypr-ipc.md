# Hyprland IPC and hyprctl (Phase E12)

The shell answers Hyprland's IPC: the request socket that `hyprctl` talks to and the event
socket that bars and scripts subscribe to, with Hyprland's request syntax, its text and `-j`
JSON output (Hyprland's field names) and its `EVENT>>DATA` lines. `hyprctl` is on the image
(`/system_ext/bin/hyprctl`), so the same scripts work from `adb shell`, from Termux, from a bind
(`hl.dsp.exec_raw`) and from the host (`tools/hyprctl.sh`).

Reference: Hyprland's [IPC](https://wiki.hypr.land/ipc/) and
[Using hyprctl](https://wiki.hypr.land/Configuring/Using-hyprctl/) pages (Hyprland 0.57, the
version whose Lua API docs/hypr-lua.md follows).

## Sockets

| Hyprland | Here |
|---|---|
| `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock` | `@hypr/.socket.sock` (abstract UNIX socket) |
| `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock` | `@hypr/.socket2.sock` (abstract UNIX socket) |
| `HYPRLAND_INSTANCE_SIGNATURE` | `android` (one instance per device; set for `execr` commands) |

PcSystemUI (`HyprIpc.kt`, next to `HyprShell`) listens on both. Abstract sockets need no
directory, no init `socket` line and no file labels, so the same code works in Waydroid and on
the PC image, and from any process in the device's network namespace (adb shell, apps,
SystemUI's own children). The protocol is Hyprland's:

- **Request socket:** connect, write `[flags]/command args` (or
  `[[BATCH]]cmd1;cmd2`, `;` escaped as `\;`), read the answer until the socket closes. The
  request ends when the client shuts its side down, or 100 ms after its last byte (clients that
  write and wait, like Hyprland's own hyprctl). Flags: `j` JSON, `r`, `a` (accepted).
- **Event socket:** connect and read lines; nothing is written to it.

```sh
echo -n 'j/clients' | socat - ABSTRACT-CONNECT:hypr/.socket.sock
socat -U - ABSTRACT-CONNECT:hypr/.socket2.sock | while read -r line; do echo "$line"; done
```

A script written for `$XDG_RUNTIME_DIR/hypr/...` paths can get them back with a bridge, e.g.
in Termux:

```sh
mkdir -p "$PREFIX/tmp/hypr/android"
socat UNIX-LISTEN:$PREFIX/tmp/hypr/android/.socket2.sock,fork ABSTRACT-CONNECT:hypr/.socket2.sock &
```

## Who may use them

Hyprland's sockets are files only the user can open. Here every process of every app shares
the abstract namespace, so `HyprIpc` checks the peer's uid (`SO_PEERCRED`) on every connection,
both sockets:

| Peer | Allowed |
|---|---|
| root (adb root, `waydroid shell`), shell (`adb shell`), system, SystemUI itself (`execr`) | always |
| apps whose package is in `ipc.allow` | yes |
| other apps | no: `permission denied`, and a log line in `HyprIpc` |

`ipc.allow` is a Lua option (`hl.config({ ipc = { allow = "com.termux,org.example.bar" } })`),
comma-separated packages, `*` for every app; without a Lua configuration
`persist.hypr.ipc_allow` is read. The default is `com.termux,com.android.hyprlauncher`. The
socket gives a caller what hyprctl gives a desktop user: window titles of every app, starting
apps, killing them, running shell commands as SystemUI (`execr`), so add only apps you trust.

SELinux (`aosp-device/hypr/sepolicy/hypr_ipc.te`): `shell`, `untrusted_app_all`, `priv_app`
and `system_app` may `connectto` `platform_app` (SystemUI's domain; abstract sockets are checked
against the listening process), `shell` may read and write the connection. The uid check
above is what decides; SELinux only lets the domains reach it. Waydroid runs without SELinux,
so there only the uid check applies.

## hyprctl

`aosp-device/hypr/hyprctl/` (Rust, `rust_binary`, `required` by PcSystemUI): builds the request
from its arguments as Hyprland's hyprctl does and prints the answer. Flags before the command:
`-j`, `-r`, `--batch "cmd ; cmd"` (`-j` goes to each command without flags), `-i android`,
`-q` (no `ok`). The exit status is 1 for an error answer, 3 when the socket cannot be reached.

Client-side commands, beyond Hyprland's:

| Command | What it does |
|---|---|
| `hyprctl events` | the event socket on stdout, one line per event (what `socat` gives) |
| `hyprctl rollinglog [-f]` | `logcat` of the shell's tags (`HyprIpc`, `HyprShell`, `HyprTile`, `HyprFocus`, `HyprLua`, `HyprKeybinds`); Android has no log file of its own for it |
| `hyprctl repl` | the interactive REPL: one `repl` request per line of stdin |

It is in `/system_ext/bin` (generic_system.mk keeps `/system` for the generic system image),
which is on `adb shell`'s PATH; Termux's PATH does not have it, so call it by its full path there
or link it into `$PREFIX/bin`.

From the host, `tools/hyprctl.sh ARGS` runs it in Waydroid's container (`sudo waydroid shell`)
when that runs, else on the adb device (`HYPRCTL_TARGET=waydroid|adb`).

From Lua, `hl.dsp.exec_raw("hyprctl ...")` is Hyprland's `execr`: a shell command run with
`/system/bin/sh -c` as SystemUI (`hl.dsp.exec_cmd` keeps starting packages, as before).

```lua
hl.bind("ALT + SHIFT + N", hl.dsp.exec_raw("hyprctl notify 1 3000 0 'hello from a bind'"))
```

## Requests

Text output follows Hyprland's layout (tab-indented blocks), JSON its field names. Windows
are Android tasks: the address is the task id in hex (`0x2a` is task 42, and `address:0x2a`
selects it anywhere a window selector is taken), `class` and `initialClass` the package,
`title` the task's label, `pid` the app's process.

| Request | Here |
|---|---|
| `dispatch NAME ARGS` | every dispatcher of the shell (HyprTileController, HyprFocus, system and host actions: `workspace`, `movetoworkspace[silent]`, `togglefloating`, `setfloating`, `settiled`, `fullscreen`, `pin`, `killactive`, `forcekillactive`, `exec`, `execr`, `focuswindow`, `cyclenext`, `cycleprev`, `swapnext`, `swapwindow`, `alterzorder`, `focuscurrentorlast`, `focusurgentorlast`, `togglespecialworkspace`, `movefocus`, `movewindow`, `resizeactive`, `moveactive`, `togglesplit`, `zoom`, `bar`, `volume`, `brightness`, `media`, `inputmethod`, `display`, `screenshot`, `lock`, ...). Window selectors in the arguments (`address:`, `class:`, `title:`, `initialclass:`, `initialtitle:`, `pid:`) are resolved to the task. Also `movetoworkspace[silent] WS,WINDOW`, `closewindow WINDOW`, `killwindow WINDOW`. `ok`, else `Invalid dispatcher or argument` / `Window not found` |
| `dispatch 'hl.dsp...'` | Hyprland 0.57's Lua form: `hl.dispatch(...)` run by HyprLauncher's Lua |
| `eval LUA`, `repl [LUA]` | Lua in the loaded configuration's state (HyprLauncher): `ok` or `error: ...`; repl prints the results through `tostring()` and what the code printed |
| `keyword SECTION:OPTION VALUE` | sets one option as `hl.config` would (`general:gaps_in 9`, nested `decoration:shadow:enabled false`); applies at once, kept until the next reload (as in Hyprland) |
| `reload` | init.lua again (`configreloaded` follows) |
| `getoption SECTION:OPTION` | `int:`/`float:`/`str:`/`custom type:` and `set:` (booleans are ints, colors custom) |
| `descriptions` | the options the shell uses, JSON (no descriptions: the option list is the docs/hypr-lua.md tables) |
| `configerrors` | the init.lua error in force and the last load's warnings |
| `clients`, `activewindow` | every window of every display, or the focused one (`Invalid` / `{}` without one): `address`, `mapped`, `hidden` (minimized), `at`, `size`, `workspace{id,name}`, `floating`, `pseudo`, `monitor`, `class`, `title`, `initialClass`, `initialTitle`, `pid`, `xwayland`, `pinned`, `fullscreen` (0, 1 maximized, 2 fullscreen: from the bounds), `fullscreenClient`, `grouped`, `tags`, `swallowing`, `focusHistoryID`, `inhibitingIdle`, `xdgTag`, `xdgDescription`, `contentType` |
| `workspaces`, `activeworkspace` | the workspaces that exist (E2) and the special ones with windows (E5, negative ids, `special:NAME`): `id`, `name` (a workspace rule's name, else the number), `monitor`, `monitorID`, `windows`, `hasfullscreen`, `lastwindow`, `lastwindowtitle`, `ispersistent`, `tiledLayout` |
| `monitors [all]` | Android's displays: `DISPLAY-<id>`, mode, refresh rate, physical size from the DPI, active and special workspace, `reserved` (the bar and system bars), `scale` (density / 160), `transform` (rotation), `focused`, `dpmsStatus`, `availableModes`; `all` adds disabled displays |
| `devices` | Android's input devices: mice and touchpads, alphabetic keyboards (language tag and layout type, the first one `main`), touch screens, styluses as tablets |
| `binds` | the bind list in force (Lua's or the defaults'): `modmask` with Hyprland's bits, `key`, `dispatcher`, `arg`, `m` for `bindm` drags |
| `layers` | HyprLauncher's windows as layer surfaces: the bar (top), notifications, launcher, overview and ALT + Tab switcher (overlay), the desktop (background), with their geometry and pid |
| `cursorpos` | InputManager's pointer position on the focused display |
| `version` | `Hyprland 0.57.0 built from branch android-pc at commit <build incremental>`; JSON with Hyprland's keys, `flags: ["android"]` |
| `splash`, `instances`, `layouts`, `submap`, `locked`, `globalshortcuts`, `animations`, `workspacerules`, `decorations WINDOW` | as Hyprland (`submap` is always `default`, `globalshortcuts` empty, `locked` the keyguard) |
| `notify ICON MS COLOR TEXT`, `dismissnotify [N]` | HyprLauncher's notifications (the ones `hl.notify` shows): Hyprland's icons as a symbol and accent color, `fontsize:N`, colors `rgb()`, `rgba()`, `0xAARRGGBB`, `0` for the default |
| `seterror COLOR TEXT`, `seterror disable` | the error strip under the bar; reset by the next successful reload, as in Hyprland |
| `setprop WINDOW PROP VALUE`, `getprop WINDOW PROP` | `alpha`/`opacity`, `alphainactive`/`opacity_inactive`, `alphaoverride` (`unset` clears), read-only `pinned`, `floating`, `fullscreen`; other properties answer `prop not found` |
| `kill` | Hyprland's kill mode: the next click or tap on a window force-stops its app (`kill` event); a right click or 15 s leaves it |
| `switchxkblayout DEVICE next\|prev` | the input method's next or previous layout (the LANGUAGE_SWITCH key, as `hl.dsp.input_method`) |
| `output create [headless\|auto] [WxH]`, `output remove DISPLAY-N` | Android's overlay displays (the developer option `overlay_display_devices`), the closest thing to a headless output |

### Not available on Android

| Request or event | Answer | Why |
|---|---|---|
| `setcursor` | `error: ...` | Android draws its own pointer; no cursor themes |
| `plugin` | `error: ...` | no compositor plugins |
| `switchxkblayout DEVICE N` | `error: ...` | Android switches layouts in order; `next`/`prev` work |
| `output create wayland` | `error: ...` | no nested Wayland backend |
| `hyprpaper`, `hyprsunset` and other tools' requests | `hyprpaper` runs the `wallpaper` dispatcher; the others `unknown request` | |
| `submap` event | never sent | the shell has no submaps |
| `togglegroup`, `moveintogroup`, `moveoutofgroup`, `lockgroups`, `ignoregrouplock` | never sent | no window groups |
| `openlayer`, `closelayer` | never sent | HyprLauncher's panels are app windows the shell does not track as they open |
| `screencast(v2)`, `activelayout`, `bell`, `renameworkspace`, `moveworkspace(v2)` | never sent | no hook in the shell for them (MediaProjection, keyboard layouts, bell, renaming, moving workspaces between displays) |

## Events

Built from the difference between two states of the default display (HyprShell's debounced
push after every layout sync, about 50 ms after a change), plus a few sent directly:

| Event | Data | When |
|---|---|---|
| `workspace`, `workspacev2` | `NAME` / `ID,NAME` | the active workspace changed |
| `createworkspace(v2)`, `destroyworkspace(v2)` | `NAME` / `ID,NAME` | a workspace appeared or went (E2 makes them on demand and removes empty ones; special ones while they hold windows) |
| `activespecial`, `activespecialv2` | `NAME,MON` / `ID,NAME,MON` (empty when closed) | a special workspace shown or hidden |
| `activewindow`, `activewindowv2` | `CLASS,TITLE` / `ADDRESS` (empty without one) | the focus moved, or the focused window's title changed (v1 only) |
| `openwindow` | `ADDRESS,WORKSPACENAME,CLASS,TITLE` | a task appeared |
| `closewindow` | `ADDRESS` | a task went |
| `movewindow`, `movewindowv2` | `ADDRESS,WORKSPACENAME` / `ADDRESS,ID,NAME` | a window changed workspace |
| `changefloatingmode` | `ADDRESS,0/1` | tiled ↔ floating |
| `pin` | `ADDRESS,0/1` | pinned or unpinned |
| `fullscreen` | `0/1` | a window entered or left fullscreen or maximized |
| `minimized` | `ADDRESS,0/1` | a window minimized or restored |
| `windowtitle`, `windowtitlev2` | `ADDRESS` / `ADDRESS,TITLE` | a task's label changed |
| `urgent` | `ADDRESS` | a window became urgent (E3's urgency) |
| `focusedmon`, `focusedmonv2` | `MON,WORKSPACENAME` / `MON,ID` | the focused display changed |
| `monitoradded(v2)`, `monitorremoved(v2)` | `MON` / `ID,MON,DESCRIPTION` | DisplayManager's display added or removed |
| `configreloaded` | (empty) | a new configuration reached the shell (not after `keyword`) |
| `kill` | `ADDRESS` | a window killed in kill mode |

Addresses in events are hex without `0x`, as Hyprland prints them.

## Code

| File | Role |
|---|---|
| `aosp-device/hypr/hyprtile/src/.../Ipc.kt` | data classes, Hyprland's text and JSON formats, request and batch parsing, the event diff (host-tested: `IpcTest`) |
| `aosp-device/hypr/PcSystemUI/src/.../wmshell/HyprIpc.kt` | the sockets, the uid check, requests on Shell's main thread, events, kill mode |
| `aosp-device/hypr/PcSystemUI/src/.../wmshell/HyprShell.kt` | creates it, feeds it the state push and `setConfig`, forwards launcher requests |
| `aosp-device/hypr/hyprshell-aidl/` | `IHyprShellListener.onRequest` and `IHyprReply` |
| `aosp-device/hypr/HyprLauncher/src/.../IpcRequests.kt` | the launcher's part: Lua, notifications, error strip, layers |
| `aosp-device/hypr/hyprlua/src/.../HyprLua.kt` | `eval`, `keyword`, `option` (host-tested: `IpcLuaTest`) |
| `aosp-device/hypr/hyprctl/` | the client, and `ipc-test.sh`, the on-device check below |
| `tools/hyprctl.sh` | the host wrapper |

## Checked on Waydroid (2026-11-22)

Test data (`XDG_DATA_HOME=~/.local/share/waydroid-test`), PcSystemUI and HyprLauncher from the
working tree in the overlay, `hyprctl` in `overlay/system_ext/bin/`; the user's data was put
back afterwards. `aosp-device/hypr/hyprctl/ipc-test.sh`, run as root in the container
(`sudo waydroid shell -- sh /data/local/tmp/ipc-test.sh`), went through every request above and
recorded `hyprctl events` meanwhile: **120 checks passed, 0 failed** (third run; the first two
found the problems below).

| Check | Result |
|---|---|
| Info requests | `version`, `splash`, `instances`, `layouts`, `submap`, `locked`, `globalshortcuts`, `monitors [all]`, `devices`, `binds`, `layers` (the bar at the top level with its geometry), `cursorpos`, `animations`, `workspacerules`, `decorations`, `descriptions`, `configerrors`, text and `-j` |
| Lua | `getoption general:gaps_in` (`int: 5`), `keyword general:gaps_in 9` then `getoption` 9 (put back), a bad value answers `error: ...`, `eval`/`repl` (`42`, `print` captured), a Lua error as `error: ...` |
| Messages | `notify` with icon and color and with `fontsize:20`, `dismissnotify [1]`, `seterror` and `seterror disable` |
| Windows | Settings and Files from `dispatch exec`, `clients`/`activewindow` with Hyprland's fields, `activeworkspace`/`workspaces` on workspace 2, `focuswindow address:0x..` and `class:`, `togglefloating` (`floating: 1`), `pin` (`pinned: 1`, `getprop pinned`), `setprop alpha 0.8` / `getprop alpha` / `unset`, `fullscreen 0` (`fullscreen: 2`), `movetoworkspacesilent 3,address:0x..`, the Lua form `dispatch 'hl.dsp.focus({ workspace = 3 })'`, `togglespecialworkspace` (`special workspace: -99 (special:special)` in `monitors`), `cyclenext`, `focusurgentorlast`, `closewindow address:`, unknown dispatcher and window answers |
| Others | `--batch` with and without `-j`, `execr` (a `hyprctl notify` run by SystemUI), `hl.dsp.exec_raw` from Lua the same way, `switchxkblayout current next`, `output create headless` (an `Overlay #1` display, `DISPLAY-2`) and `output remove DISPLAY-2`, `reload`, `kill` then a tap on Files (force-stopped), `setcursor`/`plugin`/unknown requests answer errors, `rollinglog` |
| Events | `workspace(v2)`, `createworkspace(v2)`, `destroyworkspace(v2)`, `activewindow(v2)`, `openwindow`, `closewindow`, `movewindow(v2)`, `changefloatingmode`, `pin`, `fullscreen`, `activespecial(v2)`, `configreloaded`, `kill`, `monitoradded(v2)`, `monitorremoved(v2)` |
| Not provoked | `focusedmon` (one display), `urgent` (as in E3, no app on the test data posts an alerting notification on demand), `minimized` (no minimize dispatcher), `windowtitle` (no test app relabels its task) |
| Permissions | `su shell hyprctl` answers; Termux's uid (10085) and HyprLauncher's (10099) answer; another app's uid (10123) gets `permission denied` on both sockets (exit 1, `hyprctl events` exit 6) and a `HyprIpc` warning |
| Host | `tools/hyprctl.sh -j cursorpos`, `dispatch workspace 1`, `--batch` through `sudo waydroid shell` |

Found and fixed on the way:

- `/system/bin/hyprctl` broke every build's Soong analysis (generic_system.mk's artifact path
  requirement): it is `system_ext_specific` now.
- Android's `ProcessBuilder` has no `Redirect.DISCARD` (`execr` redirects to `/dev/null`).
- `fullscreen 0` moves the task out of Shell's desk (Shell's fullscreen), so it vanished from
  `clients` and the events said `closewindow`/`openwindow`: fullscreen tasks of the display are
  added as windows of the last active workspace, `fullscreen: 2`.
- A refused client got a connection reset instead of `permission denied`: its request is read
  before the answer.

Not checked: SELinux (Waydroid runs without it; `selinux_policy` builds with
`hypr_ipc.te`), a client that writes and waits without shutting its side down (toybox `nc`
has no abstract sockets; `hyprctl` and socat's usual use shut down), several displays.
