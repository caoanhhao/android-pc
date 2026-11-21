package com.android.systemui.wmshell

import android.hardware.input.InputGestureData
import android.hardware.input.InputManager
import android.hardware.input.InputManager.CUSTOM_INPUT_GESTURE_RESULT_SUCCESS
import android.hardware.input.InputManager.KeyGestureEventHandler
import android.hardware.input.KeyGestureEvent
import android.os.IBinder
import android.os.SystemProperties
import android.util.Log
import android.view.KeyEvent
import com.android.hyprtile.Bind
import com.android.hyprtile.Mod
import com.android.hyprtile.androidKeyName
import com.android.hyprtile.defaultBinds
import com.android.hyprtile.parseBind
import com.android.hyprtile.parseMods
import com.android.wm.shell.common.ShellExecutor
import com.android.wm.shell.desktopmode.DesktopModeKeyGestureHandler
import com.android.wm.shell.sysui.ShellInit
import java.util.Optional

/**
 * Hyprland-style keybinds (Phase D2, docs/hypr-desktop.md).
 *
 * InputManager matches keys against "custom input gestures" (the keyboard shortcuts a user can
 * define in Settings) before the focused app sees them, and sends a matching gesture to the
 * process that registered a handler for its type. The gesture type is any int and is not
 * checked against KeyGestureEvent's list, so each bind gets a private type
 * ([TYPE_BASE] + its index) and this handler maps it back to its dispatcher. Nothing in
 * frameworks/base changes. Binds are stored for user 0 by InputManager; they are removed and
 * added again at every start, so a changed modifier or bind list replaces the old ones.
 *
 * Shell's own desktop key gestures (snap left/right, maximize, previous/next desk) would
 * fight the layout, so this handler takes their types over from
 * [DesktopModeKeyGestureHandler] and routes them to hyprtile; the gestures it does not change
 * (minimize, close, fullscreen, move to next display) go back to Shell's handler.
 *
 * Configuration: HyprLauncher's Lua (D5) sends the whole bind list (`binds`, defaults
 * included, in IHyprShell.setConfig); it is installed again whenever it changes. Without it:
 * `persist.hypr.mod` (the main modifier, default ALT), `persist.hypr.terminal` (package for
 * MOD+Q), `persist.hypr.bind.1` to `.32` (extra binds, each `MODS, key, dispatcher[, arg]`,
 * `$mod` standing for the main modifier; one replaces a default bind on the same keys).
 */
class HyprKeybinds(
    shellInit: ShellInit,
    private val inputManager: InputManager,
    private val controller: HyprTileController,
    private val shellHandler: Optional<DesktopModeKeyGestureHandler>,
    private val mainExecutor: ShellExecutor,
) : KeyGestureEventHandler {

    /** The binds in force; index i is gesture type TYPE_BASE + i. */
    @Volatile
    private var binds: List<Bind> = emptyList()
    /** The bind list [binds] was installed from. */
    private var wanted: List<Bind>? = null

    init {
        shellInit.addInitCallback(::onInit, this)
    }

    private fun onInit() {
        if (!controller.enabled) {
            Log.i(TAG, "tiling is off, Shell keeps its key gestures")
            return
        }
        shellHandler.ifPresent { inputManager.unregisterKeyGestureEventHandler(it) }
        val types = SHELL_TYPES + (0 until MAX_BINDS).map { TYPE_BASE + it }
        inputManager.registerKeyGestureEventHandler(types, this)
        installBinds()
        controller.onConfigChanged = {
            val list = readBinds()
            if (list != wanted) installBinds()
        }
    }

    /** Replaces the custom input gestures of earlier starts with the current bind list. */
    fun installBinds() {
        for (old in inputManager.getCustomInputGestures(null)) {
            if (old.action.keyGestureType in TYPE_BASE until TYPE_BASE + MAX_BINDS) {
                inputManager.removeCustomInputGesture(old)
            }
        }
        val list = readBinds()
        wanted = list
        val installed = ArrayList<Bind>()
        for (bind in list.take(MAX_BINDS)) {
            val keycode = keyCode(bind.key)
            if (keycode == KeyEvent.KEYCODE_UNKNOWN) {
                Log.w(TAG, "bind $bind: unknown key ${bind.key}")
                continue
            }
            val gesture = InputGestureData.Builder()
                .setTrigger(InputGestureData.createKeyTrigger(keycode, metaState(bind.mods)))
                .setKeyGestureType(TYPE_BASE + installed.size)
                .build()
            val result = inputManager.addCustomInputGesture(gesture)
            if (result == CUSTOM_INPUT_GESTURE_RESULT_SUCCESS) {
                installed += bind
            } else {
                // 2: the user has a shortcut on these keys, 4: reserved by the system.
                Log.w(TAG, "bind $bind not installed: result $result")
            }
        }
        binds = installed
        Log.i(TAG, "${installed.size} binds installed, main modifier ${mainMod()}")
    }

    override fun handleKeyGestureEvent(event: KeyGestureEvent, focusedToken: IBinder?) {
        if (event.action != KeyGestureEvent.ACTION_GESTURE_COMPLETE) return
        val type = event.keyGestureType
        if (type >= TYPE_BASE) {
            val bind = binds.getOrNull(type - TYPE_BASE) ?: return
            mainExecutor.execute {
                if (!controller.dispatch(bind.dispatcher, bind.arg)) {
                    Log.w(TAG, "bind $bind: unknown dispatcher or argument")
                }
            }
            return
        }
        val routed = when (type) {
            KeyGestureEvent.KEY_GESTURE_TYPE_SNAP_LEFT_FREEFORM_WINDOW -> "movewindow" to "l"
            KeyGestureEvent.KEY_GESTURE_TYPE_SNAP_RIGHT_FREEFORM_WINDOW -> "movewindow" to "r"
            KeyGestureEvent.KEY_GESTURE_TYPE_TOGGLE_MAXIMIZE_FREEFORM_WINDOW -> "fullscreen" to "1"
            KeyGestureEvent.KEY_GESTURE_TYPE_SWITCH_TO_PREVIOUS_DESK -> "workspace" to "e-1"
            KeyGestureEvent.KEY_GESTURE_TYPE_SWITCH_TO_NEXT_DESK -> "workspace" to "e+1"
            else -> null
        }
        if (routed != null && controller.enabled) {
            mainExecutor.execute { controller.dispatch(routed.first, routed.second) }
        } else {
            shellHandler.ifPresent { it.handleKeyGestureEvent(event, focusedToken) }
        }
    }

    private fun mainMod(): Set<Mod> =
        runCatching { parseMods(controller.configOverrides.getString("mod") ?: SystemProperties.get(PROP_MOD, DEFAULT_MOD)) }
            .getOrNull()?.takeIf { it.isNotEmpty() } ?: parseMods(DEFAULT_MOD)

    private fun readBinds(): List<Bind> {
        controller.configOverrides.getStringArray("binds")?.let { lines ->
            return lines.mapNotNull { line ->
                runCatching { parseBind(line) }.onFailure { Log.w(TAG, "bad bind '$line': ${it.message}") }
                    .getOrNull()
            }
        }
        val mod = mainMod()
        val terminal = SystemProperties.get(PROP_TERMINAL).takeIf { it.isNotEmpty() }
        val extra = (1..MAX_EXTRA_BINDS).map { SystemProperties.get("$PROP_BIND_PREFIX$it").trim() }
            .filter { it.isNotEmpty() }
            .mapNotNull { line ->
                val modText = mod.joinToString(" ")
                runCatching { parseBind(line.replace("\$mod", modText, ignoreCase = true)) }
                    .onFailure { Log.w(TAG, "bad bind '$line': ${it.message}") }.getOrNull()
            }
        // An extra bind replaces a default one on the same keys.
        val taken = extra.map { it.mods to it.key }.toSet()
        return extra + defaultBinds(mod, terminal).filter { (it.mods to it.key) !in taken }
    }

    /** Android's key code for a Hyprland key name (`LEFT`, `RETURN`, `Q`, `KEYCODE_...`). */
    private fun keyCode(key: String): Int = KeyEvent.keyCodeFromString("KEYCODE_${androidKeyName(key)}")

    private fun metaState(mods: Set<Mod>): Int {
        var meta = 0
        if (Mod.SHIFT in mods) meta = meta or KeyEvent.META_SHIFT_ON
        if (Mod.CTRL in mods) meta = meta or KeyEvent.META_CTRL_ON
        if (Mod.ALT in mods) meta = meta or KeyEvent.META_ALT_ON
        if (Mod.SUPER in mods) meta = meta or KeyEvent.META_META_ON
        return meta
    }

    private companion object {
        const val TAG = "HyprKeybinds"
        const val PROP_MOD = "persist.hypr.mod"
        const val PROP_TERMINAL = "persist.hypr.terminal"
        /** persist.hypr.bind.1 .. .32, one bind each (a property value holds 92 bytes). */
        const val PROP_BIND_PREFIX = "persist.hypr.bind."
        const val MAX_EXTRA_BINDS = 32
        /**
         * ALT, not SUPER: on Waydroid the host's Hyprland takes SUPER before Android sees it,
         * and AOSP's own system shortcuts are all on META.
         */
        const val DEFAULT_MOD = "ALT"
        /** "HY" << 16: far from AOSP's gesture types (below 100). */
        const val TYPE_BASE = 0x48590000
        const val MAX_BINDS = 128

        /** The types DesktopModeKeyGestureHandler registers; this handler takes them over. */
        val SHELL_TYPES = listOf(
            KeyGestureEvent.KEY_GESTURE_TYPE_MOVE_TO_NEXT_DISPLAY,
            KeyGestureEvent.KEY_GESTURE_TYPE_SNAP_LEFT_FREEFORM_WINDOW,
            KeyGestureEvent.KEY_GESTURE_TYPE_SNAP_RIGHT_FREEFORM_WINDOW,
            KeyGestureEvent.KEY_GESTURE_TYPE_TOGGLE_MAXIMIZE_FREEFORM_WINDOW,
            KeyGestureEvent.KEY_GESTURE_TYPE_MINIMIZE_FREEFORM_WINDOW,
            KeyGestureEvent.KEY_GESTURE_TYPE_SWITCH_TO_PREVIOUS_DESK,
            KeyGestureEvent.KEY_GESTURE_TYPE_SWITCH_TO_NEXT_DESK,
            KeyGestureEvent.KEY_GESTURE_TYPE_QUIT_FOCUSED_DESKTOP_TASK,
            KeyGestureEvent.KEY_GESTURE_TYPE_TOGGLE_FULLSCREEN,
        )
    }
}
