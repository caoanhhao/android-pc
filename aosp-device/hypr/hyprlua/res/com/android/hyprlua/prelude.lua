-- The `hl` API of HyprLauncher's configuration (Phase D5, docs/hypr-lua.md): a subset of
-- Hyprland 0.57's Lua API plus the bar. Runs in a fresh sandboxed state before init.lua.
-- Everything reaches the launcher through __host(op, ...), whose errors are raised again at
-- the line of init.lua that made the call.

local host = __host
__host = nil
local clock = __clock
__clock = nil
local raw_load = load
local type, tostring, pairs, ipairs, error, pcall, select = type, tostring, pairs, ipairs, error, pcall, select
local setmetatable, getmetatable, rawget = setmetatable, getmetatable, rawget
local concat, sort, unpack = table.concat, table.sort, table.unpack
local mtype, format = math.type, string.format

-- Calls the host; an error is raised [level] frames up from here (3: the caller of the API
-- function that called H).
local function H(level, ...)
  local ok, r = pcall(host, ...)
  if not ok then error(r, level) end
  return r
end

local hl = {}

-- Lua functions handed to the launcher (binds, timers, bar modules, gestures), by id.
local fns, next_fn = {}, 0
local function fn_id(f)
  next_fn = next_fn + 1
  fns[next_fn] = f
  return tostring(next_fn)
end

-- Calls the function with id [id]; its first result comes back as a string.
function __hl_call(id, ...)
  local f = fns[tonumber(id)]
  if not f then error("no function " .. tostring(id), 0) end
  local r = f(...)
  if r ~= nil then return tostring(r) end
end

local function str(v)
  if mtype(v) == "float" and v == math.floor(v) and v > -1e15 and v < 1e15 then
    return format("%d", v)
  end
  return tostring(v)
end

---------------------------------------------------------------------------------------------
-- hl.config

-- A gradient table { colors = {...}, angle = 45 } as Hyprland's string.
local function gradient(t, level)
  if type(t.colors) ~= "table" then error("a gradient needs colors = { ... }", level) end
  local parts = {}
  for _, c in ipairs(t.colors) do
    parts[#parts + 1] = type(c) == "number" and format("0x%08x", c) or tostring(c)
  end
  if t.angle ~= nil then parts[#parts + 1] = str(t.angle) .. "deg" end
  return concat(parts, " ")
end

local function flatten(prefix, t, out, level)
  local keys = {}
  for k in pairs(t) do
    if type(k) ~= "string" then error("option names must be strings (in " .. prefix .. ")", level) end
    keys[#keys + 1] = k
  end
  sort(keys)
  for _, k in ipairs(keys) do
    local v = t[k]
    local key = prefix == "" and k or prefix .. "." .. k
    if type(v) == "table" then
      if v.colors ~= nil then
        out[#out + 1] = { key, gradient(v, level + 1), "string" }
      else
        flatten(key, v, out, level + 1)
      end
    elseif mtype(v) == "integer" and key:find("col", 1, true) then
      out[#out + 1] = { key, format("0x%08x", v), "string" }
    elseif type(v) == "function" then
      error(key .. ": a function is not an option value", level)
    else
      out[#out + 1] = { key, str(v), type(v) }
    end
  end
  return out
end

function hl.config(t)
  if type(t) ~= "table" then error("hl.config takes a table", 2) end
  for _, e in ipairs(flatten("", t, {}, 3)) do
    H(3, "set", e[1], e[2], e[3])
  end
end

function hl.get_config(key)
  return H(3, "get", key)
end

---------------------------------------------------------------------------------------------
-- Curves and animations

-- hl.curve(name, { type = "bezier", points = { {x0, y0}, {x1, y1} } }), or the numbers directly.
function hl.curve(name, spec, ...)
  if type(name) ~= "string" then error("hl.curve needs a name", 2) end
  if type(spec) == "number" then
    local y0, x1, y1 = ...
    H(3, "curve", name, str(spec), str(y0), str(x1), str(y1))
    return
  end
  if type(spec) ~= "table" then error("hl.curve(name, { type = 'bezier', points = ... })", 2) end
  local kind = spec.type or "bezier"
  if kind == "spring" then
    H(3, "spring", name)
    return
  end
  if kind ~= "bezier" then error("unknown curve type " .. tostring(kind), 2) end
  local p = spec.points
  if type(p) ~= "table" or type(p[1]) ~= "table" or type(p[2]) ~= "table" then
    error("a bezier needs points = { {x0, y0}, {x1, y1} }", 2)
  end
  H(3, "curve", name, str(p[1][1]), str(p[1][2]), str(p[2][1]), str(p[2][2]))
end
hl.bezier = hl.curve

-- hl.animation({ leaf = "windows", enabled = true, speed = 4, bezier = "x", style = "popin" }),
-- or hl.animation("windows", { speed = 4, curve = "x" }).
function hl.animation(spec, opts)
  if type(spec) == "string" then
    local t = { leaf = spec }
    for k, v in pairs(opts or {}) do t[k] = v end
    spec = t
  end
  if type(spec) ~= "table" or type(spec.leaf) ~= "string" then
    error("hl.animation({ leaf = 'windows', speed = 4, bezier = 'default' })", 2)
  end
  local enabled = spec.enabled
  if enabled == nil then enabled = true end
  local curve = spec.bezier or spec.curve
  if spec.spring ~= nil then
    H(3, "warn", "animation " .. spec.leaf .. ": spring curves are not supported, using easeOutQuint")
    curve = curve or "easeOutQuint"
  end
  H(3, "animation", spec.leaf, enabled and "1" or "0",
    spec.speed ~= nil and str(spec.speed) or nil, curve, spec.style)
end

---------------------------------------------------------------------------------------------
-- Dispatchers

local DSP = { __name = "hl.dispatcher" }
DSP.__index = DSP
local function dsp(name, arg)
  return setmetatable({ name = name, arg = arg ~= nil and str(arg) or "" }, DSP)
end
local function is_dsp(v) return getmetatable(v) == DSP end
function DSP.__tostring(d) return "hl.dsp(" .. d.name .. (d.arg ~= "" and " " .. d.arg or "") .. ")" end

local DIRS = {
  l = "l", left = "l", r = "r", right = "r", u = "u", up = "u", top = "u",
  d = "d", down = "d", bottom = "d",
}
local function dir(d, level)
  local r = DIRS[tostring(d):lower()]
  if not r then error("bad direction '" .. tostring(d) .. "' (left, right, up, down)", level + 1) end
  return r
end

local function window_id(w, level)
  if type(w) == "table" and w.id then return str(w.id) end
  local id = tostring(w):match("^taskid:(%d+)$") or tostring(w):match("^(%d+)$")
  if not id then error("bad window '" .. tostring(w) .. "' (a window from hl.get_windows or taskid:N)", level + 1) end
  return id
end

-- A window selector (Phase E3): a window from hl.get_windows, a task id, or Hyprland's
-- selectors as text (class:, title:, initialclass:, initialtitle:, address:, pid:, stableid:,
-- activewindow, floating, tiled); the shell checks the text.
local function window_sel(w, level)
  if type(w) == "table" and w.id then return "taskid:" .. str(w.id) end
  if type(w) == "number" then return "taskid:" .. str(w) end
  if type(w) ~= "string" or w == "" then
    error("bad window '" .. tostring(w) .. "' (a window, or class:, title:, address:, pid:, ...)", level + 1)
  end
  return w
end

local FLOAT_ACTIONS = { toggle = "togglefloating", on = "setfloating", off = "settiled",
  enable = "setfloating", disable = "settiled" }
local PIN_ACTIONS = { toggle = "toggle", on = "on", off = "off", enable = "on", disable = "off" }

-- One coordinate of move/resize: pixels (a number) or "N%" of the work area (exact only).
local function coord(v, exact, level)
  if type(v) == "number" then return str(math.floor(v + 0.5)) end
  if exact and type(v) == "string" and v:match("^%d+%%$") then return v end
  error("bad coordinate '" .. tostring(v) .. "' (pixels" .. (exact and ", or 'N%'" or "") .. ")", level + 1)
end

-- { x = , y = , relative = false } as "exact X Y", else "DX DY".
local function xy(t, level)
  local exact = t.relative == false or t.exact == true
  local x, y = t.x or t[1] or 0, t.y or t[2] or 0
  return (exact and "exact " or "") .. coord(x, exact, level + 1) .. " " .. coord(y, exact, level + 1)
end

-- A namespace whose missing members are Hyprland dispatchers this shell does not have.
local function namespace(name, t)
  return setmetatable(t, { __index = function(_, k)
    return function() error(name .. "." .. tostring(k) .. " is not supported by hypr on Android", 2) end
  end })
end

local window = namespace("hl.dsp.window", {
  close = function() return dsp("killactive") end,
  kill = function() return dsp("killactive") end,
  float = function(t)
    local name = FLOAT_ACTIONS[tostring(t and t.action or "toggle")]
    if not name then error("hl.dsp.window.float: action is 'toggle', 'on' or 'off'", 2) end
    return dsp(name, t and t.window ~= nil and "taskid:" .. window_id(t.window, 2) or nil)
  end,
  pin = function(t)
    local action = tostring(t and t.action or "toggle")
    if not PIN_ACTIONS[action] then error("hl.dsp.window.pin: action is 'toggle', 'on' or 'off'", 2) end
    local arg = PIN_ACTIONS[action]
    if t and t.window ~= nil then arg = arg .. " taskid:" .. window_id(t.window, 2) end
    return dsp("pin", arg)
  end,
  fullscreen = function(t)
    local mode = t and t.mode
    if mode == nil or mode == 0 or mode == "fullscreen" then return dsp("fullscreen", "0") end
    if mode == 1 or mode == "maximized" or mode == "maximize" then return dsp("fullscreen", "1") end
    error("hl.dsp.window.fullscreen: mode is 'fullscreen' or 'maximized'", 2)
  end,
  move = function(t)
    if type(t) ~= "table" then error("hl.dsp.window.move({ workspace = N } or { direction = 'left' })", 2) end
    if t.window ~= nil then error("hl.dsp.window.move: window = is not supported (it moves the active window)", 2) end
    if t.workspace ~= nil then
      local silent = t.silent or t.follow == false
      return dsp(silent and "movetoworkspacesilent" or "movetoworkspace", t.workspace)
    end
    if t.direction ~= nil then return dsp("movewindow", dir(t.direction, 2)) end
    if t.x ~= nil or t.y ~= nil then return dsp("moveactive", xy(t, 2)) end
    error("hl.dsp.window.move needs workspace =, direction = or x =, y =", 2)
  end,
  swap = function(t)
    if type(t) ~= "table" then error("hl.dsp.window.swap({ direction = 'left' } or { next = true })", 2) end
    if t.direction ~= nil then return dsp("movewindow", dir(t.direction, 2)) end
    local target = t.target or t.with or t.other
    if target ~= nil then return dsp("swapwindow", window_sel(target, 2)) end
    if t.next then return dsp("swapnext") end
    if t.prev then return dsp("swapnext", "prev") end
    error("hl.window.swap: unrecognized arguments. Expected one of: direction, target/with/other, next, prev", 2)
  end,
  -- Hyprland's cyclenext: { next = false } goes back; tiled, floating, hist (focus history
  -- order) and visible (every display's active workspace) as its old arguments.
  cycle_next = function(t)
    t = type(t) == "table" and t or {}
    if t.window ~= nil then error("hl.dsp.window.cycle_next: window = is not supported (it cycles from the active window)", 2) end
    local w = {}
    if t.next == false or t.prev == true then w[#w + 1] = "prev" end
    for _, k in ipairs({ "tiled", "floating", "hist", "visible" }) do
      if t[k] then w[#w + 1] = k end
    end
    return dsp("cyclenext", table.concat(w, " "))
  end,
  alter_zorder = function(t)
    if type(t) ~= "table" then error("hl.window.alter_zorder: expected a table { mode, window? }", 2) end
    if t.mode ~= "top" and t.mode ~= "bottom" then error("hl.window.alter_zorder: mode is 'top' or 'bottom'", 2) end
    return dsp("alterzorder", t.mode .. (t.window ~= nil and "," .. window_sel(t.window, 2) or ""))
  end,
  drag = function() return dsp("__drag") end,
  resize = function(t)
    if t == nil then return dsp("__resize") end
    if type(t) ~= "table" then error("hl.dsp.window.resize({ x = 40, y = 0 })", 2) end
    return dsp("resizeactive", xy(t, 2))
  end,
})

hl.dsp = namespace("hl.dsp", {
  exec_cmd = function(cmd, rules)
    if type(cmd) ~= "string" then error("hl.dsp.exec_cmd needs a package name", 2) end
    if rules ~= nil then H(3, "warn", "exec_cmd: rules are ignored (use hl.window_rule)") end
    return dsp("exec", cmd)
  end,
  exec_raw = function(cmd) return dsp("exec", cmd) end,
  focus = function(t)
    if type(t) ~= "table" then error("hl.dsp.focus({ direction = ... } or { workspace = ... })", 2) end
    if t.direction ~= nil then return dsp("movefocus", dir(t.direction, 2)) end
    -- Workspaces are per display here: on_current_monitor changes nothing.
    if t.workspace ~= nil then return dsp("workspace", t.workspace) end
    if t.window ~= nil then return dsp("focuswindow", window_sel(t.window, 2)) end
    if t.urgent_or_last then return dsp("focusurgentorlast") end
    if t.last then return dsp("focuscurrentorlast") end
    error("hl.focus: unrecognized arguments. Expected one of: direction, workspace, window, urgent_or_last, last", 2)
  end,
  layout = function(msg)
    if msg == "togglesplit" then return dsp("togglesplit") end
    error("hl.dsp.layout: only 'togglesplit' is supported", 2)
  end,
  -- HyprLauncher's own: the rofi-like launcher and the hyprexpo-like overview.
  launcher = function() return dsp("launcher") end,
  overview = function() return dsp("overview") end,
  -- SystemUI's notification shade and quick settings (the touch gestures' top edge).
  notifications = function() return dsp("notifications") end,
  quick_settings = function() return dsp("quicksettings") end,
  -- Full-screen zoom around the pointer (Phase E10; Hyprland's cursor:zoom_factor):
  -- { delta = 0.5 } changes the factor, { factor = 1 } sets it (1 is no zoom).
  zoom = function(t)
    if type(t) ~= "table" then error("hl.dsp.zoom({ delta = 0.5 } or { factor = 1 })", 2) end
    if type(t.delta) == "number" then
      return dsp("zoom", (t.delta >= 0 and "+" or "") .. str(t.delta))
    end
    if type(t.factor) == "number" then
      if t.factor < 1 then error("hl.dsp.zoom: factor is 1 or more", 2) end
      return dsp("zoom", str(t.factor))
    end
    error("hl.dsp.zoom needs delta = or factor = (a number)", 2)
  end,
  -- Any dispatcher by its Hyprland name: hl.dsp.raw("movefocus", "l").
  raw = function(name, arg) return dsp(tostring(name), arg) end,
  window = window,
  workspace = namespace("hl.dsp.workspace", {}),
  cursor = namespace("hl.dsp.cursor", {}),
  group = namespace("hl.dsp.group", {}),
})

-- A dispatcher or a function as (name, arg).
local function action(d, what, level)
  if type(d) == "function" then return "lua", fn_id(d) end
  if is_dsp(d) then return d.name, d.arg end
  error(what .. ": expected an hl.dsp dispatcher or a function", level + 1)
end

function hl.dispatch(d)
  if type(d) == "function" then return d() end
  if not is_dsp(d) then error("hl.dispatch takes an hl.dsp dispatcher or a function", 2) end
  H(3, "dispatch", d.name, d.arg)
end

function hl.exec_cmd(cmd)
  if type(cmd) ~= "string" then error("hl.exec_cmd needs a package name", 2) end
  H(3, "dispatch", "exec", cmd)
end

---------------------------------------------------------------------------------------------
-- Binds

local BIND_OPTS = { "repeating", "locked", "release", "non_consuming", "transparent", "ignore_mods",
  "dont_inhibit", "long_press", "submap_universal", "click", "drag", "allow_input_capture" }

local Keybind = {}
Keybind.__index = Keybind
function Keybind:remove() H(3, "bind_remove", self.id) end
Keybind.unbind = Keybind.remove
function Keybind:set_enabled(on) H(3, "bind_enabled", self.id, on and "1" or "0") end
function Keybind:is_enabled() return H(3, "bind_is_enabled", self.id) == "1" end

-- hl.bind("ALT + Q", hl.dsp.window.close(), { description = "..." }); the dispatcher may be a
-- Lua function.
function hl.bind(keys, d, opts)
  if type(keys) ~= "string" then error("hl.bind: keys are a string like 'ALT + Q'", 2) end
  if opts ~= nil and type(opts) ~= "table" then error("hl.bind: options are a table", 2) end
  opts = opts or {}
  local name, arg = action(d, "hl.bind", 2)
  local flags = {}
  for _, o in ipairs(BIND_OPTS) do if opts[o] then flags[#flags + 1] = o end end
  local id = H(3, "bind", keys, name, arg, opts.mouse and "1" or "0", concat(flags, ","),
    opts.description or opts.desc)
  return setmetatable({ id = id, keys = keys }, Keybind)
end

function hl.unbind(keys)
  if type(keys) ~= "string" then error("hl.unbind: keys are a string like 'ALT + Q'", 2) end
  H(3, "unbind", keys)
end

---------------------------------------------------------------------------------------------
-- Rules, gestures, monitors

local Rule = {}
Rule.__index = Rule
function Rule:set_enabled(on) H(3, "rule_enabled", self.id, on and "1" or "0") end
function Rule:is_enabled() return H(3, "rule_is_enabled", self.id) == "1" end

local function pairs_of(spec, level)
  local out = {}
  for _, e in ipairs(flatten("", spec, {}, level + 1)) do
    out[#out + 1] = e[1]
    out[#out + 1] = e[2]
  end
  return out
end

-- hl.window_rule({ match = { class = "firefox" }, workspace = "2 silent", float = true })
function hl.window_rule(spec)
  if type(spec) ~= "table" then error("hl.window_rule takes a table", 2) end
  return setmetatable({ id = H(3, "windowrule", unpack(pairs_of(spec, 2))) }, Rule)
end

-- hl.workspace_rule({ workspace = "3", layout = "master", gaps_in = 0 })
function hl.workspace_rule(spec)
  if type(spec) ~= "table" then error("hl.workspace_rule takes a table", 2) end
  return setmetatable({ id = H(3, "workspacerule", unpack(pairs_of(spec, 2))) }, Rule)
end

-- hl.gesture({ fingers = 3, direction = "horizontal", action = "workspace" }); the action may
-- also be a dispatcher or a function. Played by the touch support (D6).
function hl.gesture(spec)
  if type(spec) ~= "table" then error("hl.gesture takes a table", 2) end
  local t = {}
  for k, v in pairs(spec) do t[k] = v end
  if type(t.action) ~= "string" then
    local name, arg = action(t.action, "hl.gesture action", 2)
    t.action = arg ~= "" and name .. " " .. arg or name
  end
  H(3, "gesture", unpack(pairs_of(t, 2)))
end

-- hl.monitor({ output = "", reserved = { top = 0, bottom = 40 } })
function hl.monitor(spec)
  if type(spec) ~= "table" then error("hl.monitor takes a table", 2) end
  H(3, "monitor", unpack(pairs_of(spec, 2)))
end

local function ignored(name)
  return function() H(3, "warn", name .. " is ignored on Android") end
end
hl.env = ignored("hl.env")
hl.device = ignored("hl.device")
hl.permission = ignored("hl.permission")
hl.layer_rule = function() H(3, "warn", "hl.layer_rule is ignored on Android") return setmetatable({ id = "0" }, Rule) end
hl.define_submap = function() error("hl.define_submap is not supported by hypr on Android", 2) end

---------------------------------------------------------------------------------------------
-- The bar (HyprLauncher's own, waybar-like)

-- hl.bar({ modules_left = { "apps", "workspaces" }, modules_center = { "title" },
--          modules_right = { "windows", "tray", "clock" }, clock_format = "HH:mm" })
function hl.bar(spec)
  if type(spec) ~= "table" then error("hl.bar takes a table", 2) end
  local t = {}
  for k, v in pairs(spec) do
    if type(v) == "table" and k:match("^modules_") then
      local names = {}
      for _, m in ipairs(v) do names[#names + 1] = tostring(m) end
      t[k] = concat(names, ",")
    else
      t[k] = v
    end
  end
  H(3, "bar", unpack(pairs_of(t, 2)))
end

-- hl.bar_module("battery", { text = function() return "..." end, interval = 5000,
--                            on_click = hl.dsp.exec_cmd("com.android.settings") })
function hl.bar_module(name, spec)
  if type(name) ~= "string" then error("hl.bar_module needs a name", 2) end
  if type(spec) ~= "table" then error("hl.bar_module(name, { text = ... })", 2) end
  local text, text_fn = nil, nil
  if type(spec.text) == "function" then text_fn = fn_id(spec.text)
  elseif spec.text ~= nil then text = tostring(spec.text) end
  local click_name, click_arg
  if spec.on_click ~= nil then click_name, click_arg = action(spec.on_click, "on_click", 2) end
  H(3, "barmodule", name, text, text_fn, spec.interval ~= nil and str(spec.interval) or nil,
    click_name, click_arg, spec.tooltip ~= nil and tostring(spec.tooltip) or nil)
end

---------------------------------------------------------------------------------------------
-- Events

local EVENTS = {
  ["window.open"] = "window.open", openwindow = "window.open",
  ["window.close"] = "window.close", closewindow = "window.close",
  ["window.active"] = "window.active", activewindow = "window.active",
  ["workspace.active"] = "workspace.active", workspace = "workspace.active",
  ["config.reloaded"] = "config.reloaded", configreloaded = "config.reloaded",
  ["hyprland.start"] = "hyprland.start",
}
local handlers = {}

local Subscription = {}
Subscription.__index = Subscription
function Subscription:remove() self.active = false end
function Subscription:is_active() return self.active end

-- hl.on("window.open", function(w) ... end); Hyprland's socket names (openwindow, ...) work too.
function hl.on(event, fn)
  local e = EVENTS[event]
  if not e then
    error("unknown event '" .. tostring(event) .. "' (window.open, window.close, window.active, " ..
      "workspace.active, config.reloaded, hyprland.start)", 2)
  end
  if type(fn) ~= "function" then error("hl.on needs a function", 2) end
  local sub = setmetatable({ fn = fn, active = true }, Subscription)
  local list = handlers[e] or {}
  handlers[e] = list
  list[#list + 1] = sub
  return sub
end

local function split(s, sep)
  local out = {}
  if s == nil or s == "" then return out end
  local start = 1
  while true do
    local i = s:find(sep, start, true)
    if not i then
      out[#out + 1] = s:sub(start)
      return out
    end
    out[#out + 1] = s:sub(start, i - 1)
    start = i + 1
  end
end

-- State records from the host: "S" active workspaces focused, then "W" per window.
local function snapshot()
  local recs = split(H(4, "state"), "\30")
  local head = split(recs[1], "\31")
  local s = { active = tonumber(head[2]) or 0, count = tonumber(head[3]) or 0,
    focused = tonumber(head[4]) or -1, windows = {}, names = {} }
  for i = 2, #recs do
    local f = split(recs[i], "\31")
    if f[1] == "W" then
      local id = tonumber(f[2])
      s.windows[#s.windows + 1] = {
        id = id, address = "taskid:" .. f[2], stable_id = f[2],
        workspace_id = tonumber(f[3]), class = f[4], initial_class = f[4], title = f[5],
        initial_title = f[5], floating = f[6] == "1", hidden = f[7] == "1",
        at = { x = tonumber(f[8]), y = tonumber(f[9]) },
        size = { x = tonumber(f[10]), y = tonumber(f[11]) },
        active = id == s.focused, mapped = true, visible = f[7] ~= "1", pinned = f[12] == "1",
      }
    elseif f[1] == "M" then
      local w, h = tonumber(f[3]), tonumber(f[4])
      local l, t, r, b = tonumber(f[5]), tonumber(f[6]), tonumber(f[7]), tonumber(f[8])
      s.monitor = { id = tonumber(f[2]), name = "display-" .. f[2], x = 0, y = 0, width = w, height = h,
        scale = 1, transform = 0, focused = true, disabled = false,
        -- hyprctl's order: left, top, right, bottom; also by name.
        reserved = { l, t, w - r, h - b, left = l, top = t, right = w - r, bottom = h - b },
        work_area = { x = l, y = t, width = r - l, height = b - t } }
    elseif f[1] == "N" then
      s.names[tonumber(f[2])] = f[3]
    elseif f[1] == "X" then
      s.existing = {}
      for j = 2, #f do s.existing[#s.existing + 1] = tonumber(f[j]) end
    elseif f[1] == "H" or f[1] == "U" then
      local ids = {}
      for j = 2, #f do ids[#ids + 1] = tonumber(f[j]) end
      if f[1] == "H" then s.history = ids else s.urgent = ids end
    elseif f[1] == "L" then
      s.last_workspace = tonumber(f[2]) or 0
    end
  end
  return s
end

local function workspace_of(s, n)
  local count = 0
  for _, w in ipairs(s.windows) do if w.workspace_id == n then count = count + 1 end end
  return { id = n, name = s.names[n] or tostring(n), windows = count, is_empty = count == 0,
    active = n == s.active, visible = n == s.active, special = false }
end

local function with_workspaces(s)
  for _, w in ipairs(s.windows) do w.workspace = workspace_of(s, w.workspace_id) end
  return s
end

function hl.get_windows(filter)
  local s = with_workspaces(snapshot())
  if filter == nil then return s.windows end
  local out = {}
  for _, w in ipairs(s.windows) do
    local ok = true
    if filter.class and not w.class:find(filter.class) then ok = false end
    if filter.title and not w.title:find(filter.title) then ok = false end
    if filter.floating ~= nil and w.floating ~= filter.floating then ok = false end
    if filter.workspace ~= nil and w.workspace_id ~= tonumber(filter.workspace) then ok = false end
    if ok then out[#out + 1] = w end
  end
  return out
end

function hl.get_active_window()
  for _, w in ipairs(with_workspaces(snapshot()).windows) do if w.active then return w end end
  return nil
end

function hl.get_window(selector)
  local id = tonumber(tostring(selector):match("(%d+)$"))
  for _, w in ipairs(with_workspaces(snapshot()).windows) do if w.id == id then return w end end
  return nil
end

-- The workspaces that exist (hyprctl workspaces), in number order, the active one included.
function hl.get_workspaces()
  local s = snapshot()
  local out = {}
  local ids = s.existing
  if ids == nil then
    ids = {}
    for n = 1, s.count do ids[#ids + 1] = n end
  end
  local seen = {}
  local list = {}
  for _, n in ipairs(ids) do if not seen[n] then seen[n] = true; list[#list + 1] = n end end
  if s.active > 0 and not seen[s.active] then list[#list + 1] = s.active end
  table.sort(list)
  for _, n in ipairs(list) do out[#out + 1] = workspace_of(s, n) end
  return out
end

-- The display (one: the default display) with its work area; nil before the shell answers.
function hl.get_active_monitor()
  local s = snapshot()
  local m = s.monitor
  if m then m.active_workspace = s.active > 0 and workspace_of(s, s.active) or nil end
  return m
end

function hl.get_monitors()
  local m = hl.get_active_monitor()
  return m and { m } or {}
end

-- Phase E3: the window focused before the active one, the most recently urgent window and
-- the workspace `workspace previous` goes to.
local function window_by_id(s, id)
  for _, w in ipairs(s.windows) do if w.id == id then return w end end
  return nil
end

function hl.get_last_window()
  local s = with_workspaces(snapshot())
  for _, id in ipairs(s.history or {}) do
    if id ~= s.focused then return window_by_id(s, id) end
  end
  return nil
end

function hl.get_urgent_window()
  local s = with_workspaces(snapshot())
  local u = s.urgent or {}
  for i = #u, 1, -1 do
    local w = window_by_id(s, u[i])
    if w then return w end
  end
  return nil
end

function hl.get_last_workspace()
  local s = snapshot()
  if (s.last_workspace or 0) < 1 then return nil end
  return workspace_of(s, s.last_workspace)
end

function hl.get_active_workspace()
  local s = snapshot()
  if s.active == 0 then return nil end
  return workspace_of(s, s.active)
end

function hl.get_workspace(n)
  local s = snapshot()
  n = tonumber(n)
  if n == nil or n < 1 then return nil end
  return workspace_of(s, n)
end

-- Runs the handlers of [event]; errors are collected (one handler failing does not stop the
-- others) and returned joined by newlines.
function __hl_fire(event, a, b, c, d)
  local list = handlers[event]
  if not list then return nil end
  local arg
  if event == "workspace.active" then
    local s = snapshot()
    arg = workspace_of(s, tonumber(a) or s.active)
  elseif event == "window.open" or event == "window.close" or event == "window.active" then
    local id = tonumber(a)
    arg = id and (hl.get_window(id) or { id = id, address = "taskid:" .. a, class = b or "",
      initial_class = b or "", title = c or "", workspace_id = tonumber(d) }) or nil
  end
  local errors = {}
  for _, sub in ipairs(list) do
    if sub.active then
      local ok, err = pcall(sub.fn, arg)
      if not ok then errors[#errors + 1] = tostring(err) end
    end
  end
  if #errors > 0 then return concat(errors, "\n") end
end

---------------------------------------------------------------------------------------------
-- Notifications, timers, misc

-- hl.notify("text", { timeout = 3000, color = "rgba(ff5555ff)" })
function hl.notify(text, opts)
  opts = opts or {}
  H(3, "notify", tostring(text), opts.timeout ~= nil and str(opts.timeout) or nil,
    opts.color ~= nil and tostring(opts.color) or nil)
end

hl.notification = {
  create = function(opts)
    if type(opts) ~= "table" then error("hl.notification.create({ text = ..., timeout = ms })", 2) end
    H(3, "notify", tostring(opts.text or ""), opts.timeout ~= nil and str(opts.timeout) or nil,
      opts.color ~= nil and tostring(opts.color) or nil)
  end,
}

local Timer = {}
Timer.__index = Timer
function Timer:set_enabled(on) H(3, "timer_enabled", self.id, on and "1" or "0") end
function Timer:is_enabled() return H(3, "timer_is_enabled", self.id) == "1" end
function Timer:set_timeout(ms) H(3, "timer_timeout", self.id, str(ms)) end

-- hl.timer(function() ... end, { timeout = 1000, type = "repeat" })
function hl.timer(fn, opts)
  if type(fn) ~= "function" then error("hl.timer needs a function", 2) end
  if type(opts) ~= "table" or opts.timeout == nil then error("hl.timer(fn, { timeout = ms, type = 'repeat' })", 2) end
  local id = fn_id(fn)
  H(3, "timer", id, str(opts.timeout), opts.type or "oneshot")
  return setmetatable({ id = id }, Timer)
end

function hl.version() return "hypr-android 1 (Hyprland 0.57 Lua API subset)" end
hl.time = clock.time
hl.date = clock.date
hl.clock = clock.clock

function print(...)
  local parts = {}
  for i = 1, select("#", ...) do parts[#parts + 1] = tostring((select(i, ...))) end
  H(3, "log", concat(parts, "\t"))
end

-- load() reads text only, never bytecode.
function load(chunk, name, mode, ...)
  if select("#", ...) > 0 then return raw_load(chunk, name, "t", (...)) end
  return raw_load(chunk, name, "t")
end

-- require("config.binds") reads hypr/config/binds.lua next to init.lua.
local loaded = {}
function require(name)
  if type(name) ~= "string" then error("require needs a module name", 2) end
  if name == "hypr" then return hl end
  local r = loaded[name]
  if r ~= nil then return r end
  local src, path = H(3, "readmodule", name), name:gsub("%.", "/") .. ".lua"
  local f, err = raw_load(src, "@" .. path, "t")
  if not f then error(err, 0) end
  loaded[name] = true
  r = f(name)
  if r == nil then r = true end
  loaded[name] = r
  return r
end

-- Unknown members of hl are an error at the line that uses them.
setmetatable(hl, { __index = function(_, k)
  error("hl." .. tostring(k) .. " does not exist (or is not supported by hypr on Android)", 2)
end })
_G.hl = hl
