package com.android.hyprlauncher

import android.content.Context
import android.graphics.PixelFormat
import android.graphics.Rect
import android.graphics.drawable.GradientDrawable
import android.text.TextUtils
import android.util.Log
import android.view.Gravity
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.view.WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN
import android.view.WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE
import android.view.WindowManager.LayoutParams.TYPE_SYSTEM_DIALOG
import android.widget.FrameLayout
import android.widget.HorizontalScrollView
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import com.android.hyprtile.AltTabMode
import com.android.hyprtile.AltTabSession

/**
 * ALT + Tab (Phase E4, docs/hypr-desktop.md "ALT + Tab switcher"). InputManager turns ALT + Tab
 * into its recents gesture (START when Tab goes down, COMPLETE when ALT is released) and sends
 * it to HyprLauncher, the recents provider; a Tab pressed again while ALT is still held is not a
 * gesture and goes to the focused window. So for the whole ALT press this class holds a focused
 * window of its own: the switcher's overlay, or (modes `cycle` and `bind`) an invisible one that
 * only catches the Tabs.
 *
 * `shell.alt_tab`:
 * - `switcher` (default): the windows of the display in focus-history order (hyprtile's
 *   [AltTabSession]) in a strip of icons and titles in the bar's colours. The first Tab selects
 *   the window focused before, further Tabs go deeper, SHIFT + Tab back; releasing ALT focuses
 *   the selection (`focuswindow`), Esc or a click outside cancels, a click on a window takes it.
 *   The strip shows [SHOW_DELAY_MS] after the first Tab, so a quick ALT + Tab only switches to
 *   the previous window, exactly as `focuscurrentorlast` (E3's ALT + Tab).
 * - `cycle`: every Tab runs `cyclenext` (which also brings the window to front, the host's
 *   `alterzorder top`), SHIFT + Tab `cyclenext prev` (the host's `ALT + TAB` bind).
 * - `bind`: every Tab runs the configuration's bind on `ALT + Tab` (SHIFT: `ALT + SHIFT + Tab`
 *   when there is one); without one, the overview.
 * - `overview`: D4's overview on the next window, focused when ALT is released.
 * - `off`: nothing.
 */
class AltTabSwitcher(private val desktop: Desktop) {
    private val ctx: Context = desktop.context.createDisplayContext(desktop.display)
        .createWindowContext(TYPE_SYSTEM_DIALOG, null)
    private val wm = ctx.getSystemService(WindowManager::class.java)

    private val root = object : FrameLayout(ctx) {
        override fun dispatchKeyEvent(event: KeyEvent): Boolean =
            onKey(event) || super.dispatchKeyEvent(event)
    }
    private val panel = LinearLayout(ctx)
    private val scroll = HorizontalScrollView(ctx)
    private val row = LinearLayout(ctx)
    private val caption = TextView(ctx)

    /** The mode of the ALT press in progress, null when none is. */
    private var active: AltTabMode? = null
    private var session: AltTabSession? = null
    private var hadFocus = false
    private val reveal = Runnable { if (active == AltTabMode.SWITCHER) panel.visibility = View.VISIBLE }

    init {
        root.isFocusable = true
        root.isFocusableInTouchMode = true
        root.setOnTouchListener { _, ev ->
            if (ev.action == MotionEvent.ACTION_DOWN) cancel()
            true
        }
        root.viewTreeObserver.addOnWindowFocusChangeListener { focused ->
            if (focused) hadFocus = true else if (hadFocus && active == AltTabMode.SWITCHER) cancel()
        }
        // The shell's touch gestures (D6) leave the switcher's touches alone.
        root.addOnLayoutChangeListener { v, _, _, _, _, _, _, _, _ ->
            v.systemGestureExclusionRects = listOf(Rect(0, 0, v.width, v.height))
        }
        panel.orientation = LinearLayout.VERTICAL
        panel.gravity = Gravity.CENTER_HORIZONTAL
        val pad = desktop.dp(12f)
        panel.setPadding(pad, pad, pad, pad)
        panel.isClickable = true
        scroll.isHorizontalScrollBarEnabled = false
        scroll.isFocusable = false
        scroll.addView(row)
        panel.addView(scroll, LinearLayout.LayoutParams(
            ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT))
        caption.textSize = 13f
        caption.isSingleLine = true
        caption.ellipsize = TextUtils.TruncateAt.END
        caption.gravity = Gravity.CENTER
        panel.addView(caption, LinearLayout.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply {
            topMargin = desktop.dp(8f)
        })
        root.addView(panel, FrameLayout.LayoutParams(
            ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.CENTER))
    }

    val isShown get() = active != null

    /** `shell.alt_tab` from init.lua; the switcher before it loaded or when it is unreadable. */
    fun mode(): AltTabMode = desktop.lua.current?.wire?.values?.get("alt_tab")
        ?.let { runCatching { AltTabMode.parse(it) }.getOrNull() } ?: AltTabMode.SWITCHER

    /** InputManager's RECENT_APPS_SWITCHER gesture: [start] on the first Tab, else ALT released. */
    fun onGesture(start: Boolean, cancelled: Boolean) {
        if (!start) {
            when (active) {
                AltTabMode.SWITCHER -> if (cancelled) cancel() else commit()
                AltTabMode.CYCLE, AltTabMode.BIND -> close()
                AltTabMode.OVERVIEW -> desktop.overview.commitAltTab()
                AltTabMode.OFF, null -> Unit
            }
            active = null
            return
        }
        if (active != null) {
            // A START without the COMPLETE of the last press: begin again.
            close()
        }
        val mode = mode()
        if (mode == AltTabMode.BIND && !desktop.hasTabBind()) {
            active = AltTabMode.OVERVIEW
            desktop.overview.show(altTab = true)
            return
        }
        active = mode
        when (mode) {
            AltTabMode.SWITCHER -> open()
            AltTabMode.CYCLE, AltTabMode.BIND -> { open(); tab(shift = false) }
            AltTabMode.OVERVIEW -> desktop.overview.show(altTab = true)
            AltTabMode.OFF -> active = null
        }
    }

    /** The shell's state changed while the switcher is up. */
    fun update(s: ShellState) {
        val sess = session ?: return
        sess.update(s.windows.map { it.taskId })
        if (sess.order.isEmpty()) cancel() else rebuild()
    }

    private fun open() {
        desktop.launcher.hide()
        desktop.overview.hide()
        hadFocus = false
        val switcher = active == AltTabMode.SWITCHER
        val lp = if (switcher) {
            val s = desktop.state
            session = AltTabSession(s.focusHistory, s.windows.map { it.taskId }, s.focusedTask)
            panel.background = GradientDrawable().apply {
                cornerRadius = desktop.dp(12f).toFloat()
                setColor(desktop.bar.config.background)
                setStroke(desktop.dp(2f), desktop.bar.config.accent)
            }
            caption.setTextColor(desktop.bar.config.foreground)
            rebuild()
            panel.visibility = View.INVISIBLE
            root.postDelayed(reveal, SHOW_DELAY_MS)
            WindowManager.LayoutParams(
                WindowManager.LayoutParams.MATCH_PARENT, WindowManager.LayoutParams.MATCH_PARENT,
                TYPE_SYSTEM_DIALOG, FLAG_LAYOUT_IN_SCREEN, PixelFormat.TRANSLUCENT,
            )
        } else {
            session = null
            panel.visibility = View.GONE
            // One pixel that takes no touches: it is only there to hold the key focus.
            WindowManager.LayoutParams(1, 1, TYPE_SYSTEM_DIALOG, FLAG_NOT_TOUCHABLE, PixelFormat.TRANSLUCENT)
        }
        lp.title = "HyprAltTab"
        lp.setFitInsetsTypes(0)
        try {
            wm.addView(root, lp)
            root.requestFocus()
        } catch (e: Exception) {
            Log.w(Desktop.TAG, "alt-tab window: $e")
        }
    }

    private fun close() {
        root.removeCallbacks(reveal)
        session = null
        if (root.isAttachedToWindow) wm.removeViewImmediate(root)
        row.removeAllViews()
    }

    /** ALT released: focus the selected window. */
    private fun commit() {
        val task = session?.selectedTask
        close()
        active = null
        if (task != null) desktop.dispatch("focuswindow", "$task")
    }

    private fun cancel() {
        close()
        active = null
    }

    /** A Tab while ALT is held (the first one is the gesture's START). */
    private fun tab(shift: Boolean) {
        when (active) {
            AltTabMode.SWITCHER -> session?.let { it.step(!shift); highlight() }
            // The host's `cyclenext` + `alterzorder top`: here focusing a window already brings
            // it to front, and an `alterzorder top` sent right behind would run before the
            // focus moved and raise the window that had it.
            AltTabMode.CYCLE -> desktop.dispatch("cyclenext", if (shift) "prev" else "")
            AltTabMode.BIND -> desktop.runTabBind(shift)
            else -> Unit
        }
    }

    private fun onKey(e: KeyEvent): Boolean {
        val mode = active ?: return false
        // Only the keyboard's keys: a game pad's D-pad (Waydroid passes the host's pads in)
        // must neither step nor end the press.
        if (!e.isFromSource(InputDevice.SOURCE_KEYBOARD) || e.isFromSource(InputDevice.SOURCE_GAMEPAD) ||
            e.isFromSource(InputDevice.SOURCE_JOYSTICK)) return true
        val altKey = e.keyCode == KeyEvent.KEYCODE_ALT_LEFT || e.keyCode == KeyEvent.KEYCODE_ALT_RIGHT
        // InputManager eats ALT's release while its gesture runs; should it ever come here, or a
        // key come without ALT, ALT is no longer held.
        if ((altKey && e.action == KeyEvent.ACTION_UP) || (!altKey && !e.isAltPressed && e.action == KeyEvent.ACTION_DOWN &&
                e.keyCode != KeyEvent.KEYCODE_ESCAPE)) {
            onGesture(start = false, cancelled = false)
            return true
        }
        if (e.action != KeyEvent.ACTION_DOWN) return altKey || e.keyCode == KeyEvent.KEYCODE_TAB
        when (e.keyCode) {
            KeyEvent.KEYCODE_TAB -> tab(e.isShiftPressed)
            KeyEvent.KEYCODE_GRAVE -> tab(shift = true)
            KeyEvent.KEYCODE_ESCAPE -> cancel()
            KeyEvent.KEYCODE_DPAD_RIGHT -> if (mode == AltTabMode.SWITCHER) tab(shift = false) else return false
            KeyEvent.KEYCODE_DPAD_LEFT -> if (mode == AltTabMode.SWITCHER) tab(shift = true) else return false
            KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_NUMPAD_ENTER ->
                if (mode == AltTabMode.SWITCHER) commit() else return false
            else -> return altKey
        }
        return true
    }

    private fun rebuild() {
        val sess = session ?: return
        row.removeAllViews()
        val byId = desktop.state.windows.associateBy { it.taskId }
        val n = sess.order.size.coerceAtLeast(1)
        val maxRow = (desktop.display.mode.physicalWidth * 0.9f).toInt() - desktop.dp(24f)
        val itemW = (maxRow / n).coerceIn(desktop.dp(72f), desktop.dp(128f))
        for (task in sess.order) {
            val w = byId[task] ?: continue
            val item = LinearLayout(ctx)
            item.orientation = LinearLayout.VERTICAL
            item.gravity = Gravity.CENTER_HORIZONTAL
            item.tag = task
            val p = desktop.dp(8f)
            item.setPadding(p, p, p, p)
            item.setOnClickListener { if (sess.select(task)) commit() }
            item.alpha = if (w.minimized) 0.5f else 1f
            val icon = ImageView(ctx)
            icon.setImageDrawable(desktop.apps.iconOf(w.packageName))
            item.addView(icon, LinearLayout.LayoutParams(desktop.dp(48f), desktop.dp(48f)))
            val title = TextView(ctx)
            title.text = label(w)
            title.textSize = 12f
            title.maxLines = 2
            title.ellipsize = TextUtils.TruncateAt.END
            title.gravity = Gravity.CENTER
            title.setTextColor(desktop.bar.config.foreground)
            item.addView(title, LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply {
                topMargin = desktop.dp(6f)
            })
            val ws = TextView(ctx)
            ws.text = if (w.workspace > 0) "${w.workspace}" else ""
            ws.textSize = 10f
            ws.gravity = Gravity.CENTER
            ws.setTextColor(Bar.DIM)
            item.addView(ws, LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
            item.contentDescription = label(w)
            row.addView(item, LinearLayout.LayoutParams(itemW, ViewGroup.LayoutParams.WRAP_CONTENT))
        }
        highlight()
    }

    private fun label(w: Win) = w.title.ifEmpty { desktop.apps.label(w.packageName) }

    private fun highlight() {
        val sel = session?.selectedTask
        for (i in 0 until row.childCount) {
            val item = row.getChildAt(i)
            val on = item.tag == sel
            item.background = if (on) {
                GradientDrawable().apply {
                    cornerRadius = desktop.dp(8f).toFloat()
                    setColor(Bar.FOCUS_BG)
                    setStroke(desktop.dp(2f), desktop.bar.config.accent)
                }
            } else {
                null
            }
            if (on) scroll.post { scroll.smoothScrollTo((item.left - (scroll.width - item.width) / 2).coerceAtLeast(0), 0) }
        }
        caption.text = desktop.state.windows.firstOrNull { it.taskId == sel }?.let { label(it) } ?: ""
    }

    override fun toString() = "AltTabSwitcher(active=$active, order=${session?.order}, selected=${session?.selectedTask})"

    private companion object {
        /** A quicker ALT + Tab only switches, without the strip flashing up. */
        const val SHOW_DELAY_MS = 120L
    }
}
