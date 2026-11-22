-- binds.lua: keybinds modelled on a host Hyprland configuration, for HyprAndroid.
--
-- Put it next to init.lua (/sdcard/Documents/hypr/binds.lua) and add require("binds") at the
-- end of init.lua. The host's SUPER binds are on ALT here (main_mod stays ALT: SUPER belongs
-- to the host's compositor under Waydroid). Host binds with no counterpart in this shell
-- (special workspace, pin, screenshots, bar and panel widgets, lock, zoom, wheel binds) are
-- left out; see docs/hypr-lua.md for what the dispatchers can do. Keys the host keeps for
-- itself under Waydroid (ALT + TAB, ALT + F2/F3/F11/F12, CTRL + ALT + P) never reach Android
-- there: ALT + Tab (the host's SUPER + TAB) is bound for the real PC and works in Waydroid
-- only with injected keys.

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

-- The host's SUPER + TAB: back to the window focused before (also the shell's default).
hl.bind(mainMod .. " + Tab", hl.dsp.focus({ last = "" }))
-- The host's SUPER + GRAVE: the previous workspace. The shell's default overview moves to
-- SHIFT + grave (the overview is also the three-finger swipe up and the pinch).
hl.bind(mainMod .. " + grave", hl.dsp.focus({ workspace = "prev", on_current_monitor = true }))
hl.bind(mainMod .. " + SHIFT + grave", hl.dsp.overview())
-- The host's ALT + TAB (cycle_next with alter_zorder top) is the switcher of Phase E4.

------------------------
---- WORKSPACES --------
------------------------

-- 1-9 (this shell has nine): switch, CTRL moves the window and follows, SHIFT moves it
-- without following.
for i = 1, 9 do
    hl.bind(mainMod .. " + " .. i, hl.dsp.focus({ workspace = i }))
    hl.bind(mainMod .. " + CTRL + " .. i, hl.dsp.window.move({ workspace = i }))
    hl.bind(mainMod .. " + SHIFT + " .. i, hl.dsp.window.move({ workspace = i, follow = false }))
end
