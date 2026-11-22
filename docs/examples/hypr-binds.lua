-- binds.lua: keybinds modelled on a host Hyprland configuration, for HyprAndroid.
--
-- Put it next to init.lua (/sdcard/Documents/hypr/binds.lua) and add require("binds") at the
-- end of init.lua. The host's SUPER binds are on ALT here (main_mod stays ALT: SUPER belongs
-- to the host's compositor under Waydroid). Host binds with no counterpart in this shell
-- (special workspace, pin, bar and panel widgets, wheel binds) are
-- left out; see docs/hypr-lua.md for what the dispatchers can do. Keys the host keeps for
-- itself under Waydroid (ALT + TAB, ALT + F2/F3/F11/F12, CTRL + ALT + P) never reach Android
-- there: ALT + Tab (the switcher, which also does the host's SUPER + TAB) is for the real PC
-- and works in Waydroid only with injected keys.

local mainMod = "ALT"
local resizeAmount = 40 -- px; the host's 25 logical px at Waydroid's density

------------------------
---- WINDOWS -----------
------------------------

hl.bind(mainMod .. " + Q", hl.dsp.window.close())
hl.bind(mainMod .. " + F", hl.dsp.window.fullscreen())
hl.bind(mainMod .. " + SHIFT + F", hl.dsp.window.fullscreen({ mode = "maximized" }))
hl.bind(mainMod .. " + Space", hl.dsp.window.float({ action = "toggle" }))

------------------------
---- APPS AND PANELS ---
------------------------

hl.bind(mainMod .. " + Return", hl.dsp.exec_cmd("com.termux"))
hl.bind(mainMod .. " + W", hl.dsp.exec_cmd("org.chromium.chrome"))
hl.bind(mainMod .. " + E", hl.dsp.exec_cmd("com.android.documentsui"))
-- The host's left and right panels: notifications and quick settings.
hl.bind(mainMod .. " + L", hl.dsp.notifications())
hl.bind(mainMod .. " + R", hl.dsp.quick_settings())
-- The host opens its app launcher on a SUPER tap; D stays the launcher here.
hl.bind(mainMod .. " + D", hl.dsp.launcher())

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
