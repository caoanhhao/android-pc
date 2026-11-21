package com.android.hyprlauncher

import android.app.StatusBarManager
import android.content.Context
import android.content.res.ColorStateList
import android.graphics.Color
import android.graphics.Insets
import android.graphics.PixelFormat
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.os.Binder
import android.text.TextUtils
import android.view.Gravity
import android.view.InputDevice
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.view.WindowInsets
import android.view.WindowManager
import android.view.WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN
import android.view.WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE
import android.view.WindowManager.LayoutParams.FLAG_SPLIT_TOUCH
import android.view.WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
import android.view.WindowManager.LayoutParams.TYPE_STATUS_BAR_ADDITIONAL
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextClock
import android.widget.TextView
import android.view.InsetsFrameProvider

/**
 * The waybar-like bar along the top of the default display (Phase D4): an Apps button,
 * workspaces (click to switch, scroll to step), the focused window's title, running apps
 * (click to focus), the notification and quick settings entries, and the clock.
 *
 * It is a TYPE_STATUS_BAR_ADDITIONAL window: above apps and the launcher's panels, below the
 * notification shade and the keyguard (Launcher3's taskbar type, NAVIGATION_BAR_PANEL, is
 * above both). It provides statusBars and tappableElement insets of its height (the recents
 * provider may provide insets), so apps that take the whole screen lay out below it and
 * immersive apps hide it.
 * SystemUI's own status bar has zero height (hypr RRO), so this is the only top bar.
 * Shell's desktop bounds do not count such insets, so the bar also tells hyprtile its strip
 * through IHyprShell.setReserved (waybar's exclusive zone).
 */
class Bar(private val desktop: Desktop) {
    private val ctx: Context = desktop.context.createDisplayContext(desktop.display)
        .createWindowContext(TYPE_STATUS_BAR_ADDITIONAL, null)
    private val wm = ctx.getSystemService(WindowManager::class.java)
    private val insetsOwner = Binder()
    val reservedTop = desktop.dp(HEIGHT_DP)

    private val root = LinearLayout(ctx)
    private val workspaces = LinearLayout(ctx)
    private val title = TextView(ctx)
    private val running = LinearLayout(ctx)
    private var shown = false

    init {
        root.orientation = LinearLayout.HORIZONTAL
        root.gravity = Gravity.CENTER_VERTICAL
        root.setBackgroundColor(BG)
        root.setPadding(desktop.dp(6f), 0, desktop.dp(6f), 0)

        root.addView(button("▦  Apps") { desktop.launcher.toggle() })
        workspaces.orientation = LinearLayout.HORIZONTAL
        workspaces.gravity = Gravity.CENTER_VERTICAL
        root.addView(workspaces, LinearLayout.LayoutParams(WRAP_CONTENT, MATCH_PARENT))

        title.setTextColor(FG)
        title.textSize = 13f
        title.isSingleLine = true
        title.ellipsize = TextUtils.TruncateAt.END
        title.gravity = Gravity.CENTER
        root.addView(title, LinearLayout.LayoutParams(0, MATCH_PARENT, 1f))

        running.orientation = LinearLayout.HORIZONTAL
        running.gravity = Gravity.CENTER_VERTICAL
        root.addView(running, LinearLayout.LayoutParams(WRAP_CONTENT, MATCH_PARENT))

        root.addView(iconButton(android.R.drawable.ic_popup_reminder, "Notifications") {
            ctx.getSystemService(StatusBarManager::class.java)?.expandNotificationsPanel()
        })
        root.addView(iconButton(android.R.drawable.ic_menu_preferences, "Quick settings") {
            ctx.getSystemService(StatusBarManager::class.java)?.expandSettingsPanel()
        })
        val clock = TextClock(ctx)
        clock.format24Hour = "EEE d MMM  HH:mm"
        clock.format12Hour = "EEE d MMM  h:mm a"
        clock.setTextColor(FG)
        clock.textSize = 13f
        clock.typeface = Typeface.DEFAULT_BOLD
        clock.setPadding(desktop.dp(10f), 0, desktop.dp(6f), 0)
        root.addView(clock, LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT))

        // waybar: scrolling over the workspaces steps through them.
        root.setOnGenericMotionListener { _, ev ->
            if (ev.action == MotionEvent.ACTION_SCROLL &&
                ev.isFromSource(InputDevice.SOURCE_CLASS_POINTER)) {
                val v = ev.getAxisValue(MotionEvent.AXIS_VSCROLL)
                if (v != 0f) desktop.dispatch("workspace", if (v < 0) "e+1" else "e-1")
                true
            } else {
                false
            }
        }
        update(ShellState.EMPTY)
    }

    /**
     * The keyguard is a see-through window above the bar; hide the bar while it shows (its
     * insets stay, so nothing re-lays out on unlock).
     */
    fun setHiddenByKeyguard(hidden: Boolean) {
        root.visibility = if (hidden) View.INVISIBLE else View.VISIBLE
    }

    fun show() {
        if (shown) return
        shown = true
        wm.addView(root, params())
    }

    private fun params(): WindowManager.LayoutParams {
        val lp = WindowManager.LayoutParams(
            MATCH_PARENT, reservedTop, TYPE_STATUS_BAR_ADDITIONAL,
            FLAG_NOT_FOCUSABLE or FLAG_LAYOUT_IN_SCREEN or FLAG_SPLIT_TOUCH,
            PixelFormat.TRANSLUCENT,
        )
        lp.gravity = Gravity.TOP
        lp.title = "HyprBar"
        lp.setFitInsetsTypes(0)
        lp.layoutInDisplayCutoutMode = LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
        lp.setTrustedOverlay()
        val size = Insets.of(0, reservedTop, 0, 0)
        lp.providedInsets = arrayOf(
            InsetsFrameProvider(insetsOwner, 0, WindowInsets.Type.statusBars()).setInsetsSize(size),
            InsetsFrameProvider(insetsOwner, 0, WindowInsets.Type.tappableElement())
                .setInsetsSize(size),
        )
        return lp
    }

    /** Redraws workspaces, title and running apps from the shell's state. */
    fun update(s: ShellState) {
        workspaces.removeAllViews()
        val count = maxOf(s.workspaces, s.active, 1)
        for (n in 1..count) {
            val occupied = s.windows.any { it.workspace == n }
            val b = button("$n") { desktop.dispatch("workspace", "$n") }
            b.minWidth = desktop.dp(26f)
            b.gravity = Gravity.CENTER
            when {
                n == s.active -> {
                    b.background = pill(ACCENT)
                    b.setTextColor(BG_SOLID)
                }
                occupied -> b.setTextColor(FG)
                else -> b.setTextColor(DIM)
            }
            workspaces.addView(b, LinearLayout.LayoutParams(WRAP_CONTENT, desktop.dp(22f)).apply {
                marginStart = desktop.dp(2f)
            })
        }

        val focused = s.windows.find { it.taskId == s.focusedTask }
        title.text = when {
            focused == null -> if (s.active == 0) "" else "workspace ${s.active}"
            focused.title.isNotEmpty() -> focused.title
            else -> desktop.apps.label(focused.packageName)
        }

        running.removeAllViews()
        for (w in s.windows) {
            val icon = ImageView(ctx)
            icon.setImageDrawable(desktop.apps.iconOf(w.packageName))
            val pad = desktop.dp(3f)
            icon.setPadding(pad, pad, pad, pad)
            icon.alpha = if (w.workspace == s.active && !w.minimized) 1f else 0.45f
            if (w.taskId == s.focusedTask) icon.background = pill(FOCUS_BG)
            icon.tooltipText = "${w.title.ifEmpty { desktop.apps.label(w.packageName) }} " +
                "(workspace ${w.workspace})"
            icon.setOnClickListener { desktop.dispatch("focuswindow", "${w.taskId}") }
            val size = desktop.dp(24f)
            running.addView(icon, LinearLayout.LayoutParams(size, size).apply {
                marginStart = desktop.dp(2f)
            })
        }
    }

    private fun button(text: String, onClick: () -> Unit): TextView {
        val b = TextView(ctx)
        b.text = text
        b.setTextColor(FG)
        b.textSize = 13f
        b.typeface = Typeface.DEFAULT_BOLD
        b.gravity = Gravity.CENTER_VERTICAL
        b.setPadding(desktop.dp(8f), 0, desktop.dp(8f), 0)
        b.setOnClickListener { onClick() }
        return b
    }

    private fun iconButton(res: Int, label: String, onClick: () -> Unit): View {
        val v = ImageView(ctx)
        v.setImageResource(res)
        v.imageTintList = ColorStateList.valueOf(FG)
        val pad = desktop.dp(5f)
        v.setPadding(pad, pad, pad, pad)
        v.contentDescription = label
        v.tooltipText = label
        v.setOnClickListener { onClick() }
        val size = desktop.dp(26f)
        v.layoutParams = LinearLayout.LayoutParams(size, size).apply { marginStart = desktop.dp(4f) }
        return v
    }

    private fun pill(color: Int) = GradientDrawable().apply {
        cornerRadius = desktop.dp(8f).toFloat()
        setColor(color)
    }

    companion object {
        const val HEIGHT_DP = 30f
        val BG = Color.parseColor("#E61E1E2E")
        val BG_SOLID = Color.parseColor("#1E1E2E")
        val FG = Color.parseColor("#CDD6F4")
        val DIM = Color.parseColor("#6C7086")
        val ACCENT = Color.parseColor("#33CCFF")
        val FOCUS_BG = Color.parseColor("#4433CCFF")
    }
}
