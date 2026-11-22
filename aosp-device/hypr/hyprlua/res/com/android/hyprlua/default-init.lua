-- hypr: the desktop shell's configuration (HyprLauncher). Saved changes apply at once; a
-- file with an error is not used (the last good configuration stays) and the error shows
-- under the bar. The API is a subset of Hyprland's Lua configuration plus the bar; every
-- option is in docs/hypr-lua.md. Split it up with require("name"), which reads name.lua
-- next to this file (require("config.binds") reads config/binds.lua).

local mainMod = "ALT" -- SUPER belongs to the host's compositor under Waydroid

------------------------
---- LOOK AND FEEL -----
------------------------

hl.config({
    general = {
        gaps_in  = 5,
        gaps_out = 12,
        border_size = 2,
        col = {
            active_border   = { colors = { "rgba(33ccffee)", "rgba(00ff99ee)" }, angle = 45 },
            inactive_border = "rgba(595959aa)",
        },
        layout = "dwindle",
    },

    decoration = {
        rounding = 10,
        active_opacity   = 1.0,
        inactive_opacity = 1.0,
        shadow = { enabled = true },
    },

    animations = { enabled = true },

    dwindle = { preserve_split = false },
    master = { new_status = "slave", mfact = 0.55 },

    -- This shell's own settings.
    shell = {
        main_mod = mainMod,     -- modifier of the default binds and of the mouse binds
        default_binds = true,   -- Hyprland's example binds on main_mod (docs/hypr-lua.md)
        file_manager = "com.android.documentsui",
        -- terminal = "com.termux", -- main_mod + Q
        caption = "none",       -- title bars: "none" (default), "floating" or "all"
    },
})

hl.curve("easeOutQuint",   { type = "bezier", points = { {0.23, 1},    {0.32, 1} } })
hl.curve("easeInOutCubic", { type = "bezier", points = { {0.65, 0.05}, {0.36, 1} } })
hl.curve("linear",         { type = "bezier", points = { {0, 0},       {1, 1}    } })
hl.curve("almostLinear",   { type = "bezier", points = { {0.5, 0.5},   {0.75, 1} } })
hl.curve("quick",          { type = "bezier", points = { {0.15, 0},    {0.1, 1}  } })

hl.animation({ leaf = "global",     enabled = true, speed = 10,   bezier = "default" })
hl.animation({ leaf = "border",     enabled = true, speed = 5.39, bezier = "easeOutQuint" })
hl.animation({ leaf = "windows",    enabled = true, speed = 4.79, bezier = "easeOutQuint" })
hl.animation({ leaf = "windowsIn",  enabled = true, speed = 4.1,  bezier = "easeOutQuint", style = "popin 87%" })
hl.animation({ leaf = "windowsOut", enabled = true, speed = 1.49, bezier = "linear",       style = "popin 87%" })
hl.animation({ leaf = "fadeIn",     enabled = true, speed = 1.73, bezier = "almostLinear" })
hl.animation({ leaf = "fadeOut",    enabled = true, speed = 1.46, bezier = "almostLinear" })
hl.animation({ leaf = "fade",       enabled = true, speed = 3.03, bezier = "quick" })
hl.animation({ leaf = "workspaces", enabled = true, speed = 2.5,  bezier = "easeOutQuint", style = "slide" })

---------------------
---- KEYBINDINGS ----
---------------------

-- The default binds (shell.default_binds) are Hyprland's example ones on main_mod:
-- 1-9 workspace, SHIFT 1-9 move to workspace, arrows focus, SHIFT arrows move, C close,
-- V float, J toggle split, F maximize, SHIFT F fullscreen, E files, D / SPACE launcher,
-- ` overview. A bind on the same keys replaces one; hl.unbind removes one.

-- hl.bind(mainMod .. " + Return", hl.dsp.exec_cmd("com.termux"))
-- hl.bind(mainMod .. " + B", function() hl.notify("Hello from Lua") end)
-- hl.unbind(mainMod .. " + J")

-- Move and resize windows with mainMod + left and right mouse buttons.
hl.bind(mainMod .. " + mouse:272", hl.dsp.window.drag(),   { mouse = true })
hl.bind(mainMod .. " + mouse:273", hl.dsp.window.resize(), { mouse = true })

--------------------------------
---- WINDOWS AND WORKSPACES ----
--------------------------------

-- hl.window_rule({ match = { class = "^org\\.mozilla" }, workspace = "2" })
-- hl.window_rule({ match = { class = "calculator" }, float = true })
-- hl.workspace_rule({ workspace = "3", layout = "master", gaps_in = 0, gaps_out = 0 })

-------------
---- BAR ----
-------------

hl.bar({
    modules_left   = { "apps", "workspaces" },
    modules_center = { "title" },
    modules_right  = { "windows", "tray", "clock" },
    clock_format   = "EEE d MMM  HH:mm",
})

----------------
---- EVENTS ----
----------------

-- Touch gestures (docs/hypr-lua.md): edges, 3-5 finger swipes, pinches on the wallpaper.
-- hl.bind("gesture:edge_bottom", hl.dsp.launcher())
-- hl.bind("gesture:swipe_3_down", hl.dsp.window.close())

-- hl.on("workspace.active", function(ws) hl.notify("Workspace " .. ws.name) end)
-- hl.on("window.open", function(w) print("opened " .. w.class) end)
