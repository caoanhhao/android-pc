package com.android.systemui.wmshell

import android.app.ActivityManager.RunningTaskInfo
import android.content.Context
import android.graphics.Color
import android.graphics.PixelFormat
import android.graphics.drawable.GradientDrawable
import android.util.Log
import android.util.TypedValue
import android.view.Gravity
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.view.WindowManager
import android.widget.LinearLayout
import android.widget.TextView

/**
 * The window menu a touch long-press on a window's frame opens (Phase D6): float or tile,
 * maximize (tiled windows), fullscreen, move to workspace 1..N (+1 for a new one), close.
 * Each entry runs a dispatcher on that window ([HyprTileController.dispatchOn]). A small
 * no-focus system window at the finger; a touch outside closes it. Shell's main thread only.
 */
class HyprWindowMenu(
    private val context: Context,
    private val controller: HyprTileController,
) {
    private val wm = context.getSystemService(WindowManager::class.java)
    private var view: View? = null
    private val frame = android.graphics.Rect()

    /** Whether ([x], [y]) is on the open menu. */
    fun contains(x: Float, y: Float) = view != null && frame.contains(x.toInt(), y.toInt())

    fun show(task: RunningTaskInfo, x: Float, y: Float) {
        dismiss()
        val (workspaces, current, tiled) = controller.menuState(task)
        val root = LinearLayout(context).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(6), dp(6), dp(6), dp(6))
            background = GradientDrawable().apply {
                cornerRadius = dp(12).toFloat()
                setColor(BACKGROUND)
                setStroke(dp(1), ACCENT)
            }
            elevation = dp(8).toFloat()
        }
        val pkg = (task.baseActivity ?: task.topActivity)?.packageName
        val name = task.taskDescription?.label?.takeIf { it.isNotEmpty() }
            ?: pkg?.let { p ->
                runCatching {
                    context.packageManager.getApplicationLabel(
                        context.packageManager.getApplicationInfo(p, 0)).toString()
                }.getOrNull() ?: p
            } ?: "#${task.taskId}"
        root.addView(TextView(context).apply {
            text = name
            setTextColor(DIM)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 13f)
            setPadding(dp(14), dp(4), dp(14), dp(6))
            maxLines = 1
        })
        fun item(label: String, dispatcher: String, arg: String = "") {
            root.addView(entry(label) { run(task, dispatcher, arg) },
                LinearLayout.LayoutParams(MATCH_PARENT, dp(44)))
        }
        item(if (tiled) "Float" else "Tile", "togglefloating")
        if (tiled) item("Maximize", "fullscreen", "1")
        item("Fullscreen", "fullscreen", "0")
        val row = LinearLayout(context).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(14), 0, dp(8), 0)
        }
        row.addView(TextView(context).apply {
            text = "Workspace"
            setTextColor(FOREGROUND)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 16f)
        }, LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT).apply { marginEnd = dp(8) })
        for (n in 1..minOf(MAX_WORKSPACES, workspaces + 1)) {
            row.addView(entry("$n", center = true, selected = n == current) {
                if (n != current) run(task, "movetoworkspacesilent", "$n") else dismiss()
            }, LinearLayout.LayoutParams(dp(40), dp(40)).apply { marginStart = dp(2) })
        }
        root.addView(row, LinearLayout.LayoutParams(MATCH_PARENT, dp(48)))
        item("Close", "killactive")

        root.setOnTouchListener { _, ev ->
            if (ev.actionMasked == MotionEvent.ACTION_OUTSIDE) dismiss()
            false
        }
        root.measure(View.MeasureSpec.UNSPECIFIED, View.MeasureSpec.UNSPECIFIED)
        val screen = wm.maximumWindowMetrics.bounds
        val w = root.measuredWidth
        val h = root.measuredHeight
        val lp = WindowManager.LayoutParams(
            WRAP_CONTENT, WRAP_CONTENT,
            WindowManager.LayoutParams.TYPE_SYSTEM_DIALOG,
            WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE or
                WindowManager.LayoutParams.FLAG_WATCH_OUTSIDE_TOUCH or
                WindowManager.LayoutParams.FLAG_NOT_TOUCH_MODAL,
            PixelFormat.TRANSLUCENT,
        ).apply {
            gravity = Gravity.TOP or Gravity.START
            this.x = (x.toInt() - w / 2).coerceIn(0, maxOf(0, screen.width() - w))
            this.y = y.toInt().coerceIn(0, maxOf(0, screen.height() - h))
            title = "HyprWindowMenu"
        }
        try {
            wm.addView(root, lp)
            view = root
            frame.set(lp.x, lp.y, lp.x + w, lp.y + h)
        } catch (e: RuntimeException) {
            Log.e(TAG, "window menu failed", e)
        }
    }

    fun dismiss() {
        val v = view ?: return
        view = null
        try {
            wm.removeViewImmediate(v)
        } catch (e: RuntimeException) {
            Log.w(TAG, "window menu: $e")
        }
    }

    private fun run(task: RunningTaskInfo, dispatcher: String, arg: String) {
        dismiss()
        Log.i(TAG, "window ${task.taskId}: $dispatcher $arg")
        controller.dispatchOn(task.taskId, dispatcher, arg)
    }

    private fun entry(
        label: String,
        center: Boolean = false,
        selected: Boolean = false,
        onClick: () -> Unit,
    ) = TextView(context).apply {
        text = label
        setTextColor(if (selected) BACKGROUND else FOREGROUND)
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 16f)
        gravity = if (center) Gravity.CENTER else Gravity.CENTER_VERTICAL
        if (!center) setPadding(dp(14), 0, dp(14), 0)
        minWidth = if (center) 0 else dp(200)
        background = GradientDrawable().apply {
            cornerRadius = dp(8).toFloat()
            setColor(if (selected) ACCENT else Color.TRANSPARENT)
        }
        isClickable = true
        setOnClickListener { onClick() }
    }

    private fun dp(v: Int) = (v * context.resources.displayMetrics.density + 0.5f).toInt()

    private companion object {
        const val TAG = "HyprWindowMenu"
        const val MAX_WORKSPACES = 9
        val BACKGROUND = Color.parseColor("#E61E1E2E")
        val FOREGROUND = Color.parseColor("#CDD6F4")
        val DIM = Color.parseColor("#7F849C")
        val ACCENT = Color.parseColor("#33CCFF")
    }
}
