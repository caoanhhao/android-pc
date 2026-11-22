package com.android.hyprlauncher

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Color
import android.graphics.PixelFormat
import android.graphics.Rect
import android.graphics.drawable.GradientDrawable
import android.util.Log
import android.view.Gravity
import android.view.KeyEvent
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.view.WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN
import android.view.WindowManager.LayoutParams.TYPE_SYSTEM_DIALOG
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.TextView
import android.window.TaskSnapshotManager
import java.util.concurrent.Executors
import kotlin.math.ceil
import kotlin.math.min
import kotlin.math.sqrt

/**
 * The overview (Phase D4), after Hyprland's hyprexpo: every workspace as a scaled-down desktop
 * in a grid, its windows drawn from their task snapshots at their real places. A click on a
 * window focuses it (switching workspace), a click on a workspace switches to it; 1-9, 0 switch
 * too, arrows move the selection, Enter focuses it, Esc closes. Bound to MOD+` (the `overview`
 * dispatcher), and it is the recents screen SystemUI asks for: the recents key toggles it and
 * Alt+Tab opens it on the next window, Tab steps on, releasing Alt focuses the selection.
 */
class Overview(private val desktop: Desktop) {
    private val ctx: Context = desktop.context.createDisplayContext(desktop.display)
        .createWindowContext(TYPE_SYSTEM_DIALOG, null)
    private val wm = ctx.getSystemService(WindowManager::class.java)
    private val grid = object : ViewGroup(ctx) {
        override fun onLayout(changed: Boolean, l: Int, t: Int, r: Int, b: Int) {
            // The shell's touch gestures (D6) leave the overview's touches alone.
            systemGestureExclusionRects = listOf(Rect(0, 0, r - l, b - t))
            layoutCells(r - l, b - t)
        }
        override fun dispatchKeyEvent(event: KeyEvent): Boolean =
            onKey(event) || super.dispatchKeyEvent(event)
    }
    private val snapshots = HashMap<Int, Bitmap?>()
    private val loader = Executors.newSingleThreadExecutor()
    private var state = ShellState.EMPTY
    /** Windows in selection order: the active workspace first, front to back. */
    private var order: List<Win> = emptyList()
    private var selected = -1
    private var shown = false
    private var hadFocus = false

    val isShown get() = shown

    init {
        grid.setBackgroundColor(Color.parseColor("#D911111B"))
        grid.isFocusable = true
        grid.isFocusableInTouchMode = true
        grid.setOnClickListener { hide() }
        grid.viewTreeObserver.addOnWindowFocusChangeListener { focused ->
            if (focused) hadFocus = true else if (hadFocus) hide()
        }
    }

    fun toggle() = if (shown) hide() else show(altTab = false)

    /** Opens the overview; with [altTab] the selection starts on (or steps to) the next window. */
    fun show(altTab: Boolean) {
        if (shown) {
            if (altTab) step(1)
            return
        }
        desktop.launcher.hide()
        shown = true
        hadFocus = false
        state = desktop.state
        snapshots.clear()
        rebuild()
        selected = when {
            order.isEmpty() -> -1
            altTab -> min(1, order.size - 1)
            else -> order.indexOfFirst { it.taskId == state.focusedTask }.coerceAtLeast(0)
        }
        highlight()
        val lp = WindowManager.LayoutParams(
            WindowManager.LayoutParams.MATCH_PARENT, WindowManager.LayoutParams.MATCH_PARENT,
            TYPE_SYSTEM_DIALOG, FLAG_LAYOUT_IN_SCREEN, PixelFormat.TRANSLUCENT,
        )
        lp.title = "HyprOverview"
        lp.setFitInsetsTypes(0)
        wm.addView(grid, lp)
        grid.requestFocus()
        loadSnapshots()
    }

    fun hide() {
        if (!shown) return
        shown = false
        wm.removeViewImmediate(grid)
        grid.removeAllViews()
        snapshots.clear()
    }

    /** Alt released after Alt+Tab: focus the selected window. */
    fun commitAltTab() {
        if (!shown) return
        val w = order.getOrNull(selected)
        hide()
        if (w != null) desktop.dispatch("focuswindow", "${w.taskId}")
    }

    fun update(s: ShellState) {
        if (!shown) return
        val keep = order.getOrNull(selected)?.taskId
        state = s
        rebuild()
        selected = order.indexOfFirst { it.taskId == keep }.takeIf { it >= 0 }
            ?: selected.coerceAtMost(order.size - 1)
        highlight()
    }

    /**
     * The active and the occupied workspaces, as the bar shows them (E2), then the special
     * workspaces with windows or shown (E5; negative keys).
     */
    private fun shownWorkspaces() = state.shown().ifEmpty { listOf(1) } + state.occupiedSpecials().map { it.first }

    private fun rebuild() {
        grid.removeAllViews()
        val active = state.active.coerceAtLeast(1)
        val special = state.shownSpecialKey
        order = state.on(active) + (special?.let { state.on(it) } ?: emptyList()) +
            state.windows.filter { it.workspace != active && it.workspace != special }
        for (ws in shownWorkspaces()) {
            val current = ws == this@Overview.state.active || ws == special
            val cell = FrameLayout(ctx)
            cell.background = GradientDrawable().apply {
                cornerRadius = desktop.dp(10f).toFloat()
                setColor(Color.parseColor("#1E1E2E"))
                setStroke(desktop.dp(2f), if (current) Bar.ACCENT else Color.parseColor("#45475A"))
            }
            cell.tag = ws
            cell.setOnClickListener {
                hide()
                // A special workspace's cell toggles it over the active workspace.
                val name = state.specials[ws]
                if (name != null) desktop.dispatch("togglespecialworkspace", name) else desktop.dispatch("workspace", "$ws")
            }
            // Back to front, so the front window is drawn last.
            for (w in state.on(ws).asReversed()) {
                val v = ImageView(ctx)
                v.tag = w
                v.scaleType = ImageView.ScaleType.FIT_XY
                v.background = GradientDrawable().apply {
                    cornerRadius = desktop.dp(4f).toFloat()
                    setColor(Color.parseColor("#313244"))
                }
                v.clipToOutline = true
                val bmp = snapshots[w.taskId]
                if (bmp != null) {
                    v.setImageBitmap(bmp)
                } else {
                    v.scaleType = ImageView.ScaleType.CENTER_INSIDE
                    v.setImageDrawable(desktop.apps.iconOf(w.packageName))
                }
                if (w.minimized) v.alpha = 0.4f
                v.contentDescription = w.title.ifEmpty { desktop.apps.label(w.packageName) }
                v.tooltipText = v.contentDescription
                v.setOnClickListener { hide(); desktop.dispatch("focuswindow", "${w.taskId}") }
                cell.addView(v)
            }
            val label = TextView(ctx)
            label.text = state.label(ws)
            label.setTextColor(if (current) Bar.ACCENT else Bar.FG)
            label.textSize = 14f
            label.setPadding(desktop.dp(8f), desktop.dp(4f), 0, 0)
            cell.addView(label, FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.TOP or Gravity.START))
            grid.addView(cell)
        }
        grid.requestLayout()
    }

    /** Lays the workspaces out in a near-square grid of display-shaped cells. */
    private fun layoutCells(width: Int, height: Int) {
        val n = grid.childCount
        if (n == 0) return
        val cols = ceil(sqrt(n.toDouble())).toInt().coerceAtMost(4).coerceAtLeast(1)
        val rows = ceil(n / cols.toDouble()).toInt()
        val margin = desktop.dp(24f)
        val top = desktop.bar.reservedTop + margin
        val dw = desktop.display.mode.physicalWidth.toFloat().coerceAtLeast(1f)
        val dh = desktop.display.mode.physicalHeight.toFloat().coerceAtLeast(1f)
        val availW = (width - margin * (cols + 1)) / cols.toFloat()
        val availH = (height - top - margin * rows) / rows.toFloat()
        val scale = min(availW / dw, availH / dh)
        val cw = (dw * scale).toInt()
        val ch = (dh * scale).toInt()
        val x0 = (width - cols * cw - (cols - 1) * margin) / 2
        val y0 = top + ((height - top - margin) - rows * ch - (rows - 1) * margin) / 2
        for (i in 0 until n) {
            val cell = grid.getChildAt(i) as FrameLayout
            val x = x0 + (i % cols) * (cw + margin)
            val y = y0 + (i / cols) * (ch + margin)
            cell.measure(
                View.MeasureSpec.makeMeasureSpec(cw, View.MeasureSpec.EXACTLY),
                View.MeasureSpec.makeMeasureSpec(ch, View.MeasureSpec.EXACTLY))
            cell.layout(x, y, x + cw, y + ch)
            for (j in 0 until cell.childCount) {
                val v = cell.getChildAt(j)
                val w = v.tag as? Win ?: continue
                val r = Rect(
                    (w.bounds.left * scale).toInt(), (w.bounds.top * scale).toInt(),
                    (w.bounds.right * scale).toInt(), (w.bounds.bottom * scale).toInt())
                v.measure(
                    View.MeasureSpec.makeMeasureSpec(r.width().coerceAtLeast(1), View.MeasureSpec.EXACTLY),
                    View.MeasureSpec.makeMeasureSpec(r.height().coerceAtLeast(1), View.MeasureSpec.EXACTLY))
                v.layout(r.left, r.top, r.right, r.bottom)
            }
        }
    }

    override fun toString() = "Overview(shown=$shown, windows=${order.size})"

    private fun highlight() {
        val sel = order.getOrNull(selected)
        for (i in 0 until grid.childCount) {
            val cell = grid.getChildAt(i) as FrameLayout
            for (j in 0 until cell.childCount) {
                val v = cell.getChildAt(j)
                val w = v.tag as? Win ?: continue
                v.foreground = if (w.taskId == sel?.taskId) {
                    GradientDrawable().apply {
                        cornerRadius = desktop.dp(4f).toFloat()
                        setStroke(desktop.dp(3f), Bar.ACCENT)
                    }
                } else {
                    null
                }
            }
        }
    }

    private fun step(by: Int) {
        if (order.isEmpty()) return
        selected = Math.floorMod(selected + by, order.size)
        highlight()
    }

    private fun onKey(e: KeyEvent): Boolean {
        if (e.action != KeyEvent.ACTION_DOWN) return false
        when (val code = e.keyCode) {
            KeyEvent.KEYCODE_ESCAPE, KeyEvent.KEYCODE_GRAVE, KeyEvent.KEYCODE_BACK -> hide()
            KeyEvent.KEYCODE_DPAD_RIGHT, KeyEvent.KEYCODE_DPAD_DOWN, KeyEvent.KEYCODE_TAB ->
                step(if (e.isShiftPressed) -1 else 1)
            KeyEvent.KEYCODE_DPAD_LEFT, KeyEvent.KEYCODE_DPAD_UP -> step(-1)
            KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_NUMPAD_ENTER, KeyEvent.KEYCODE_SPACE -> {
                val w = order.getOrNull(selected)
                hide()
                if (w != null) desktop.dispatch("focuswindow", "${w.taskId}")
            }
            in KeyEvent.KEYCODE_0..KeyEvent.KEYCODE_9 -> {
                hide()
                desktop.dispatch("workspace", "${(code - KeyEvent.KEYCODE_0).let { if (it == 0) 10 else it }}")
            }
            else -> return false
        }
        return true
    }

    /** Task snapshots, fresh for visible windows, the last one taken for the others. */
    private fun loadSnapshots() {
        val windows = state.windows
        loader.execute {
            val mgr = TaskSnapshotManager.getInstance()
            for (w in windows) {
                val bmp = try {
                    val snap = (if (state.isVisible(w)) {
                        mgr.takeTaskSnapshot(w.taskId, false)
                    } else {
                        null
                    }) ?: mgr.getTaskSnapshot(w.taskId, TaskSnapshotManager.RESOLUTION_LOW)
                    snap?.wrapToBitmap()
                } catch (e: Exception) {
                    Log.d(Desktop.TAG, "snapshot of task ${w.taskId}: $e")
                    null
                }
                desktop.handler.post {
                    if (!shown) return@post
                    snapshots[w.taskId] = bmp
                }
            }
            desktop.handler.post { if (shown) { rebuild(); highlight() } }
        }
    }
}
