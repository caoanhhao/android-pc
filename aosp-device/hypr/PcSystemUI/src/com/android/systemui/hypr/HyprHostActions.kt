package com.android.systemui.hypr

import android.app.ActivityManager
import android.app.ActivityManager.RunningTaskInfo
import android.content.Context
import android.hardware.display.DisplayManager
import android.hardware.display.DisplayManagerGlobal
import android.hardware.input.InputManager
import android.hardware.input.InputManagerGlobal
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.provider.Settings
import android.util.Log
import android.view.Display
import android.view.InputDevice
import android.view.KeyCharacterMap
import android.view.KeyEvent
import android.widget.Toast
import com.android.hyprtile.DisplayMode
import com.android.hyprtile.HostKey

/**
 * The dispatchers of Phase E11 for what a desktop's own scripts do on the host (wpctl,
 * brightnessctl, playerctl, fcitx5-remote, the dual-screen script, `hyprctl kill`):
 *
 * - `volume up|down|mute`, `brightness up|down`, `media play|next|previous|stop` and
 *   `inputmethod next|previous` send the matching key (VOLUME_UP, BRIGHTNESS_UP, MEDIA_NEXT,
 *   LANGUAGE_SWITCH, ...) as a keyboard would, so Android does exactly what it does for the
 *   real keys: AudioService with SystemUI's volume dialog, PhoneWindowManager's brightness
 *   step with the brightness dialog, the active media session, the input method's next
 *   layout or subtype.
 * - `display extend|mirror|primary|external`: connected displays on or off, mirroring
 *   (`Settings.Secure.MIRROR_BUILT_IN_DISPLAY`) and the default display's power, with
 *   DisplayManager's MANAGE_DISPLAYS calls.
 * - `forcekillactive`: force-stops the app of the focused window (`hyprctl kill` picks a window
 *   with the next click; on a desktop without a host compositor the focused one is the target).
 *
 * Nothing here needs SystemUI's Dagger graph, so [HyprTileController] calls it directly.
 */
object HyprHostActions {
    private const val TAG = "HyprHostActions"

    /** The dispatchers [dispatch] runs (`forcekillactive` goes through [forceStop]). */
    val NAMES = setOf("volume", "brightness", "media", "inputmethod", "display")

    private val main by lazy { Handler(Looper.getMainLooper()) }

    /** Runs dispatcher [name]; false for an unknown name or argument. Any thread. */
    fun dispatch(context: Context, name: String, arg: String): Boolean {
        if (name == "display") {
            val mode = DisplayMode.parse(arg) ?: return false
            main.post { display(context, mode) }
            return true
        }
        val key = HostKey.parse(name, arg) ?: return false
        Log.i(TAG, "$name $arg: key ${KeyEvent.keyCodeToString(key.keyCode)}")
        sendKey(key.keyCode, if (key.shift) KeyEvent.META_SHIFT_ON or KeyEvent.META_SHIFT_LEFT_ON else 0)
        return true
    }

    /** Force-stops [task]'s app for the task's user, as Settings' Force stop does. */
    fun forceStop(context: Context, task: RunningTaskInfo) {
        val pkg = task.baseActivity?.packageName ?: task.topActivity?.packageName ?: return
        if (pkg == context.packageName || pkg == "android") return
        Log.i(TAG, "forcekillactive: $pkg (task ${task.taskId}, user ${task.userId})")
        try {
            context.getSystemService(ActivityManager::class.java)?.forceStopPackageAsUser(pkg, task.userId)
        } catch (e: SecurityException) {
            // FORCE_STOP_PACKAGES is privileged: missing until the package is rescanned with it.
            Log.w(TAG, "forcekillactive: $pkg: $e")
        }
    }

    /**
     * A key press (down and up) from the virtual keyboard. The bind's own modifiers are still
     * held on the real keyboard, but an injected event carries its own [meta] state, so the
     * system sees the plain key (KeyGestureController handles BRIGHTNESS_* and LANGUAGE_SWITCH
     * only without modifiers).
     */
    private fun sendKey(keyCode: Int, meta: Int) {
        val now = SystemClock.uptimeMillis()
        val im = InputManagerGlobal.getInstance()
        for (action in intArrayOf(KeyEvent.ACTION_DOWN, KeyEvent.ACTION_UP)) {
            val ev = KeyEvent(now, now, action, keyCode, 0, meta, KeyCharacterMap.VIRTUAL_KEYBOARD,
                0, KeyEvent.FLAG_FROM_SYSTEM, InputDevice.SOURCE_KEYBOARD)
            ev.displayId = Display.INVALID_DISPLAY
            im.injectInputEvent(ev, InputManager.INJECT_INPUT_EVENT_MODE_ASYNC)
        }
    }

    /**
     * The host's dual-screen script on Android: `extend` (dualscreen) turns every connected
     * display on with mirroring off, `mirror` the same with the default display mirrored,
     * `primary` turns the external displays off, `external` keeps them on and powers the
     * default display off (DisplayManager cannot disable the default display; its windows stay
     * on it, dark, until moved). Each mode first gives the default display its power back.
     */
    private fun display(context: Context, mode: DisplayMode) {
        val dm = context.getSystemService(DisplayManager::class.java) ?: return
        val externals = dm.getDisplays(DisplayManager.DISPLAY_CATEGORY_ALL_INCLUDING_DISABLED)
            .filter { it.displayId != Display.DEFAULT_DISPLAY && it.type == Display.TYPE_EXTERNAL }
        Log.i(TAG, "display ${mode.arg}: external displays ${externals.map { it.displayId }}")
        if (externals.isEmpty()) {
            toast(context, "No external display connected")
            return
        }
        val global = DisplayManagerGlobal.getInstance()
        if (mode != DisplayMode.EXTERNAL) global.requestDisplayPower(Display.DEFAULT_DISPLAY, Display.STATE_UNKNOWN)
        when (mode) {
            DisplayMode.PRIMARY -> externals.forEach { dm.disableConnectedDisplay(it.displayId) }
            DisplayMode.EXTEND, DisplayMode.MIRROR, DisplayMode.EXTERNAL -> {
                Settings.Secure.putIntForUser(context.contentResolver, Settings.Secure.MIRROR_BUILT_IN_DISPLAY,
                    if (mode == DisplayMode.MIRROR) 1 else 0, ActivityManager.getCurrentUser())
                externals.forEach { dm.enableConnectedDisplay(it.displayId) }
            }
        }
        if (mode == DisplayMode.EXTERNAL) {
            // Give the external display a moment to come up before the default one goes dark.
            main.postDelayed({
                val on = dm.getDisplays().any { it.type == Display.TYPE_EXTERNAL && it.state == Display.STATE_ON }
                if (on) {
                    global.requestDisplayPower(Display.DEFAULT_DISPLAY, Display.STATE_OFF)
                } else {
                    Log.w(TAG, "display external: no external display is on, the default one stays on")
                    toast(context, "No external display is on")
                }
            }, EXTERNAL_SETTLE_MS)
        }
    }

    private fun toast(context: Context, text: String) {
        Toast.makeText(context, text, Toast.LENGTH_SHORT).show()
    }

    private const val EXTERNAL_SETTLE_MS = 1500L
}
