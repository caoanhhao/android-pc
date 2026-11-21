package com.android.hyprlauncher

import android.content.Context
import android.graphics.Color
import android.graphics.PixelFormat
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.view.Gravity
import android.view.View
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.view.WindowManager
import android.view.WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN
import android.view.WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE
import android.view.WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE
import android.view.WindowManager.LayoutParams.TYPE_STATUS_BAR_ADDITIONAL
import android.widget.LinearLayout
import android.widget.TextView

/**
 * Hyprland's on-screen messages (Phase D5), under the bar: the configuration error as a red
 * strip across the display that stays until init.lua loads again (Hyprland's config error
 * bar), and `hl.notify` messages stacked at the top left, each for its timeout (Hyprland's
 * notifications). A window that takes no input, so it never blocks the windows below it.
 */
class Notifier(private val desktop: Desktop) {
    private val ctx: Context = desktop.context.createDisplayContext(desktop.display)
        .createWindowContext(TYPE_STATUS_BAR_ADDITIONAL, null)
    private val wm = ctx.getSystemService(WindowManager::class.java)
    private val root = LinearLayout(ctx)
    private val error = TextView(ctx)
    private val list = LinearLayout(ctx)
    private var shown = false
    private var top = 0

    init {
        root.orientation = LinearLayout.VERTICAL
        error.setTextColor(Color.WHITE)
        error.textSize = 13f
        error.typeface = Typeface.MONOSPACE
        error.setBackgroundColor(ERROR_BG)
        val pad = desktop.dp(8f)
        error.setPadding(pad, pad / 2, pad, pad / 2)
        error.visibility = View.GONE
        root.addView(error, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT))
        list.orientation = LinearLayout.VERTICAL
        list.setPadding(pad, pad, pad, pad)
        root.addView(list, LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT))
    }

    /** Places the messages under a bar of height [barHeight] px. */
    fun setTop(barHeight: Int) {
        if (barHeight == top) return
        top = barHeight
        if (shown) wm.updateViewLayout(root, params())
    }

    /** Shows the configuration error, or hides it when [message] is null. */
    fun setError(message: String?) {
        error.text = message?.let { "init.lua: config error\n$it" }
        error.visibility = if (message == null) View.GONE else View.VISIBLE
        update()
    }

    /** A notification for [timeoutMs]; [color] is its accent (ARGB), the bar's accent if null. */
    fun notify(text: String, timeoutMs: Long, color: Int?) {
        val v = TextView(ctx)
        v.text = text
        v.setTextColor(Bar.FG)
        v.textSize = 13f
        v.maxWidth = desktop.dp(520f)
        val pad = desktop.dp(10f)
        v.setPadding(pad, pad / 2, pad, pad / 2)
        v.background = GradientDrawable().apply {
            cornerRadius = desktop.dp(8f).toFloat()
            setColor(Bar.BG_SOLID)
            setStroke(desktop.dp(2f), color ?: Bar.ACCENT)
        }
        while (list.childCount >= MAX_SHOWN) list.removeViewAt(0)
        list.addView(v, LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT).apply {
            bottomMargin = desktop.dp(6f)
        })
        update()
        desktop.handler.postDelayed({
            list.removeView(v)
            update()
        }, timeoutMs)
    }

    private fun update() {
        val needed = error.visibility == View.VISIBLE || list.childCount > 0
        if (needed && !shown) {
            wm.addView(root, params())
            shown = true
        } else if (!needed && shown) {
            wm.removeView(root)
            shown = false
        }
    }

    private fun params(): WindowManager.LayoutParams {
        val lp = WindowManager.LayoutParams(
            MATCH_PARENT, WRAP_CONTENT, TYPE_STATUS_BAR_ADDITIONAL,
            FLAG_NOT_FOCUSABLE or FLAG_NOT_TOUCHABLE or FLAG_LAYOUT_IN_SCREEN,
            PixelFormat.TRANSLUCENT,
        )
        lp.gravity = Gravity.TOP or Gravity.START
        lp.y = top
        lp.title = "HyprNotifications"
        lp.setFitInsetsTypes(0)
        lp.setTrustedOverlay()
        return lp
    }

    companion object {
        const val MAX_SHOWN = 6
        val ERROR_BG = Color.parseColor("#E6C0392B")
    }
}
