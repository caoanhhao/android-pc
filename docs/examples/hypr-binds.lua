-- binds.lua: keybinds modelled on a host Hyprland configuration, for HyprAndroid.
--
-- Put it next to init.lua (/sdcard/Documents/hypr/binds.lua) and add require("binds") at the
-- end of init.lua. The host's SUPER binds are on ALT here (main_mod stays ALT: SUPER belongs
-- to the host's compositor under Waydroid). Host binds with no counterpart in this shell
-- (panel widgets) are left out; see docs/hypr-lua.md for what the
-- dispatchers can do. Keys the host keeps for itself under Waydroid (ALT + TAB, ALT + F2/F3/
-- F11/F12, CTRL + ALT + P) never reach Android there: ALT + Tab (the switcher, which also does
-- the host's SUPER + TAB) is for the real PC and works in Waydroid only with injected keys.

local mainMod = "ALT"
local resizeAmount = 40 -- px; the host's 25 logical px at Waydroid's density

------------------------
---- WINDOWS -----------
------------------------

hl.bind(mainMod .. " + Q", hl.dsp.window.close())
hl.bind(mainMod .. " + F", hl.dsp.window.fullscreen())
hl.bind(mainMod .. " + SHIFT + F", hl.dsp.window.fullscreen({ mode = "maximized" }))
hl.bind(mainMod .. " + Space", hl.dsp.window.float({ action = "toggle" }))
-- Pin: a pinned window floats (a tiled one is floated first), stays on top and follows every
-- workspace.
hl.bind(mainMod .. " + CTRL + Space", hl.dsp.window.pin())
-- Float the window, fill the work area (the display without the bar) and pin it, as the host
-- does with the monitor's size.
hl.bind(mainMod .. " + CTRL + SHIFT + F", function()
    local monitor = hl.get_active_monitor()
    if monitor == nil then return end
    local area = monitor.work_area
    hl.dispatch(hl.dsp.window.float({ action = "on" }))
    hl.dispatch(hl.dsp.window.resize({ x = area.width, y = area.height, relative = false }))
    hl.dispatch(hl.dsp.window.move({ x = area.x, y = area.y, relative = false }))
    hl.dispatch(hl.dsp.window.pin({ action = "toggle" }))
end)

------------------------
---- SPECIAL WORKSPACE -
------------------------

-- The host's SUPER + S and SUPER + CTRL + S (Phase E5): the scratchpad, shown over the current
-- workspace (dimmed behind it) and hidden again, and the focused window moved into it (shown);
-- the same keys on a window already there move it back to the current workspace. Named ones:
-- toggle_special("term") and move({ workspace = "special:term" }).
hl.bind(mainMod .. " + S", hl.dsp.workspace.toggle_special())
hl.bind(mainMod .. " + CTRL + S", hl.dsp.window.move({ workspace = "special" }))

------------------------
---- APPS AND PANELS ---
------------------------

hl.bind(mainMod .. " + Return", hl.dsp.exec_cmd("com.termux"))
hl.bind(mainMod .. " + W", hl.dsp.exec_cmd("org.chromium.chrome"))
hl.bind(mainMod .. " + E", hl.dsp.exec_cmd("com.android.documentsui"))
-- The host's SUPER + P (btop in kitty on workspace 5): HyprTop, the btop-style monitor.
hl.bind(mainMod .. " + P", hl.dsp.exec_cmd("com.android.hyprtop", { workspace = 5 }))
-- The host's MOD3 + C (a calculator in a floating terminal); C alone is focus up here.
hl.bind(mainMod .. " + SHIFT + C", hl.dsp.exec_cmd("com.android.calculator2"))
hl.window_rule({ name = "calculator", match = { class = "^com\\.android\\.calculator2$" }, float = true })
-- The host's left and right panels: notifications and quick settings.
hl.bind(mainMod .. " + L", hl.dsp.notifications())
hl.bind(mainMod .. " + R", hl.dsp.quick_settings())
-- The host opens its app launcher on a SUPER tap; D stays the launcher here.
hl.bind(mainMod .. " + D", hl.dsp.launcher())

------------------------
---- BAR ---------------
------------------------

-- The host's SUPER + B (waybar's SIGUSR1) and SUPER + SHIFT + B (waybar restarted). Hiding
-- the bar gives its strip back to the windows. For a bar that hides by itself and comes back
-- when the pointer reaches the top edge: hl.bar({ autohide = true }).
hl.bind(mainMod .. " + B", hl.dsp.bar.toggle())
hl.bind(mainMod .. " + SHIFT + B", hl.dsp.bar.reload())

------------------------
---- FOCUS, MOVE, SIZE -
------------------------

-- Arrows and h/n/c/t (left, right, up, down), as on the host.
local dirs = {
    { "left", "h", "l" },
    { "right", "n", "r" },
    { "up", "c", "u" },
    { "down", "t", "d" },
}
for _, d in ipairs(dirs) do
    local arrow, letter, dir = d[1], d[2], d[3]
    hl.bind(mainMod .. " + " .. arrow, hl.dsp.focus({ direction = dir }))
    hl.bind(mainMod .. " + " .. letter, hl.dsp.focus({ direction = dir }))
    hl.bind(mainMod .. " + CTRL + " .. arrow, hl.dsp.window.move({ direction = dir }))
    hl.bind(mainMod .. " + CTRL + " .. letter, hl.dsp.window.move({ direction = dir }))
end

hl.bind(mainMod .. " + SHIFT + left", hl.dsp.window.resize({ x = -resizeAmount, y = 0 }))
hl.bind(mainMod .. " + SHIFT + right", hl.dsp.window.resize({ x = resizeAmount, y = 0 }))
hl.bind(mainMod .. " + SHIFT + up", hl.dsp.window.resize({ x = 0, y = -resizeAmount }))
hl.bind(mainMod .. " + SHIFT + down", hl.dsp.window.resize({ x = 0, y = resizeAmount }))

------------------------
---- WINDOW HISTORY ----
------------------------

-- As the host: switching to the active workspace goes back to the previous one.
hl.config({ binds = { workspace_back_and_forth = 1, allow_workspace_cycles = 1 } })

-- The host's SUPER + TAB: back to the window focused before (also the shell's default). With
-- main_mod = ALT the ALT + Tab switcher below owns the key, and a quick tap does just this; the
-- bind runs only with shell.alt_tab = "bind".
hl.bind(mainMod .. " + Tab", hl.dsp.focus({ last = "" }))
-- The host's SUPER + GRAVE: the previous workspace. The shell's default overview moves to
-- SHIFT + grave (the overview is also the three-finger swipe up and the pinch).
hl.bind(mainMod .. " + grave", hl.dsp.focus({ workspace = "prev", on_current_monitor = true }))
hl.bind(mainMod .. " + SHIFT + grave", hl.dsp.overview())
-- The host's ALT + TAB (cycle_next with alter_zorder top) and SUPER + TAB on one key: hold ALT
-- and press Tab to walk the focus history in a switcher, release ALT to focus the selection
-- (Phase E4). "cycle" would be the host's ALT + TAB exactly (each Tab cycles and raises).
hl.config({ shell = { alt_tab = "switcher" } })

------------------------
---- WORKSPACES --------
------------------------

-- Twenty workspaces, as on the host (the shell's default is ten; up to 1000).
hl.config({ shell = { workspaces = 20 } })

-- 1-9 and 0 (10): switch, CTRL moves the window and follows, SHIFT moves it without
-- following.
for i = 1, 10 do
    local key = tostring(i % 10)
    hl.bind(mainMod .. " + " .. key, hl.dsp.focus({ workspace = i }))
    hl.bind(mainMod .. " + CTRL + " .. key, hl.dsp.window.move({ workspace = i }))
    hl.bind(mainMod .. " + SHIFT + " .. key, hl.dsp.window.move({ workspace = i, follow = false }))
end

-- The keypad (Num Lock off, as the host names its keys): 11-20, the same modifiers.
local keypad = { "KP_End", "KP_Down", "KP_Next", "KP_Left", "KP_Begin", "KP_Right",
    "KP_Home", "KP_Up", "KP_Prior", "KP_Insert" }
for i, key in ipairs(keypad) do
    local ws = i + 10
    hl.bind(mainMod .. " + " .. key, hl.dsp.focus({ workspace = ws }))
    hl.bind(mainMod .. " + CTRL + " .. key, hl.dsp.window.move({ workspace = ws }))
    hl.bind(mainMod .. " + SHIFT + " .. key, hl.dsp.window.move({ workspace = ws, follow = false }))
end

------------------------
---- MOUSE -------------
------------------------

-- The wheel over anything, with mainMod: next and previous workspace, as on the host. The
-- default mouse binds stay: mainMod + left drags a window, + right resizes it, + middle
-- floats it. The SHIFT + wheel and SHIFT + middle-button binds are the zoom's, below.
hl.bind(mainMod .. " + mouse_down", hl.dsp.focus({ workspace = "e+1" }))
hl.bind(mainMod .. " + mouse_up", hl.dsp.focus({ workspace = "e-1" }))

------------------------
---- ZOOM --------------
------------------------

-- The host's zoom (cursor:zoom_factor, around the pointer): the whole screen is magnified and
-- follows the pointer. A delta changes the factor, factor = 1 resets it. As on the host,
-- wheel down zooms in and wheel up zooms out.
hl.bind(mainMod .. " + SHIFT + equal", hl.dsp.zoom({ delta = 0.5 }), { repeating = true })
hl.bind(mainMod .. " + SHIFT + minus", hl.dsp.zoom({ delta = -0.5 }), { repeating = true })
hl.bind(mainMod .. " + SHIFT + mouse_down", hl.dsp.zoom({ delta = 0.2 }))
hl.bind(mainMod .. " + SHIFT + mouse_up", hl.dsp.zoom({ delta = -0.2 }), { repeating = true })
hl.bind(mainMod .. " + SHIFT + mouse:274", hl.dsp.zoom({ factor = 1 }))

------------------------
---- SYSTEM ------------
------------------------

-- The host's SUPER + SHIFT + V (clipboard manager): SystemUI's clipboard history, in memory
-- only, sensitive clips (passwords) never kept. Enter pastes, SHIFT + Enter only copies.
hl.bind(mainMod .. " + SHIFT + V", hl.dsp.clipboard_history())
-- The host's MOD3 + T (translate): the selection, or else the clipboard, to Google Translate
-- (installed by the user; a notification says so when it is missing). ALT + CTRL + T is
-- "move down" here, so SHIFT.
hl.bind(mainMod .. " + SHIFT + T", hl.dsp.translate())
-- Screenshots as the host's custom binds have them: SHIFT + S an area, CTRL + SHIFT + S the
-- whole screen.
hl.bind(mainMod .. " + SHIFT + S", hl.dsp.screenshot({ mode = "area" }))
hl.bind(mainMod .. " + CTRL + SHIFT + S", hl.dsp.screenshot({ mode = "full" }))
-- The host's SUPER + SHIFT + Escape (hyprlock) and SUPER + Escape (its user panel).
hl.bind(mainMod .. " + SHIFT + Escape", hl.dsp.lock())
hl.bind(mainMod .. " + Escape", hl.dsp.user_switcher())

------------------------
---- HOST FUNCTIONS ----
------------------------

-- What the host's scripts do (wpctl, brightnessctl, fcitx5, hyprctl, the dual-screen and
-- wallpaper scripts), with Android's own volume and brightness dialogs. Under Waydroid the
-- host keeps ALT + F2/F3/F11/F12 and ALT + CTRL + P; on the real PC they reach these binds.
-- The plain media keys (XF86Audio*, XF86MonBrightness*) need no bind: Android handles them.
-- A held key steps once (InputManager's custom key gestures fire on the first key down).
hl.bind(mainMod .. " + F12", hl.dsp.volume("up"))
hl.bind(mainMod .. " + F11", hl.dsp.volume("down"))
hl.bind(mainMod .. " + F3", hl.dsp.brightness("up"))
hl.bind(mainMod .. " + F2", hl.dsp.brightness("down"))
-- Displays: the host's SUPER + CTRL + SHIFT + P (both), SUPER + CTRL + P (the PC's own display
-- only) and ALT + CTRL + P (the external one only; on SHIFT here, CTRL + P being primary).
hl.bind(mainMod .. " + CTRL + SHIFT + P", hl.dsp.display("extend"))
hl.bind(mainMod .. " + CTRL + P", hl.dsp.display("primary"))
hl.bind(mainMod .. " + SHIFT + P", hl.dsp.display("external"))
-- The host's MOD3 binds on ALT + SHIFT (as translate above): the next keyboard layout or
-- input method (fcitx5-remote -t), force-stop the focused app (hyprctl kill), init.lua again
-- (hyprctl reload).
hl.bind(mainMod .. " + SHIFT + Space", hl.dsp.input_method("next"))
hl.bind(mainMod .. " + SHIFT + Q", hl.dsp.window.kill())
hl.bind(mainMod .. " + SHIFT + R", hl.dsp.reload())
-- A random wallpaper from /sdcard/Pictures/Wallpapers (one wallpaper for every workspace, so
-- both binds do the same).
hl.bind(mainMod .. " + SHIFT + W", hl.dsp.wallpaper({ workspace = "current" }))
hl.bind(mainMod .. " + CTRL + SHIFT + W", hl.dsp.wallpaper({ workspace = "all" }))
-- The keypad's Enter: Termux, and Termux floating.
hl.bind(mainMod .. " + KP_Enter", hl.dsp.exec_cmd("com.termux"))
hl.bind(mainMod .. " + CTRL + KP_Enter", hl.dsp.exec_cmd("com.termux", { float = true }))
