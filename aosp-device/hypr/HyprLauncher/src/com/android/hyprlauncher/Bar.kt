package com.android.hyprlauncher

import android.animation.Animator
import android.animation.AnimatorListenerAdapter
import android.animation.ValueAnimator
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
import android.util.Log
import android.view.Gravity
import android.view.InputDevice
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.view.ViewTreeObserver
import android.view.WindowInsets
import android.view.WindowManager
import android.view.WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN
import android.view.WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE
import android.view.WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE
import android.view.WindowManager.LayoutParams.FLAG_SPLIT_TOUCH
import android.view.WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
import android.view.WindowManager.LayoutParams.TYPE_STATUS_BAR_ADDITIONAL
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextClock
import android.widget.TextView
import android.view.InsetsFrameProvider
import com.android.hyprlua.BarConfig
import com.android.hyprtile.AnimationConfig
import com.android.hyprtile.BarVisibility

/**
 * The waybar-like bar along the top of the default display (Phase D4): an Apps button,
 * workspaces (click to switch, scroll to step), the focused window's title, running apps
 * (click to focus), the notification and quick settings entries, and the clock. Which modules
 * sit left, centre and right, the colours, the clock format and modules written in Lua come
 * from init.lua (`hl.bar`, `hl.bar_module`, Phase D5).
 *
 * It is a TYPE_STATUS_BAR_ADDITIONAL window: above apps and the launcher's panels, below the
 * notification shade and the keyguard (Launcher3's taskbar type, NAVIGATION_BAR_PANEL, is
 * above both). It provides statusBars and tappableElement insets of its height (the recents
 * provider may provide insets), so apps that take the whole screen lay out below it and
 * immersive apps hide it.
 * SystemUI's own status bar has zero height (hypr RRO), so this is the only top bar.
 * Shell's desktop bounds do not count such insets, so the bar also tells hyprtile its strip
 * through IHyprShell.setReserved (waybar's exclusive zone).
 *
 * Phase E7: the `bar` dispatcher hides, shows, toggles and reloads it, and `hl.bar({ autohide
 * = true })` keeps it hidden until the pointer reaches the top edge ([BarVisibility]). A hidden
 * bar gives its strip back (no insets, reserved 0, the tiles re-lay out); its window stays as
 * a thin strip along the edge that catches the pointer (and touches) when it auto-hides, and
 * a revealed bar floats over the windows without moving them. It slides in and out with the
 * `layersIn`/`layersOut` animations (style `fade` fades instead).
 */
class Bar(private val desktop: Desktop) {
    private val ctx: Context = desktop.context.createDisplayContext(desktop.display)
        .createWindowContext(TYPE_STATUS_BAR_ADDITIONAL, null)
    private val wm = ctx.getSystemService(WindowManager::class.java)
    private val insetsOwner = Binder()
    var config = BarConfig()
        private set
    /** The bar's height in pixels, whether it shows or not. */
    var heightPx = desktop.dp(config.heightDp.toFloat())
        private set
    private val vis = BarVisibility(config.autohide, config.autohideDelayMs)
    /** The strip the bar keeps out of the work area: its height while it reserves, else 0. */
    val reservedTop: Int get() = if (vis.reserves) heightPx else 0
    private var reportedTop = reservedTop
    var anims = AnimationConfig()
    var animationsEnabled = true
    private var names: Map<Int, String> = emptyMap()
    private var state = ShellState.EMPTY

    private val root = LinearLayout(ctx)
    private val workspaces = LinearLayout(ctx)
    private val title = TextView(ctx)
    private val running = LinearLayout(ctx)
    private val clock = TextClock(ctx)
    /** Lua modules' views by name; their text comes from [setModuleText]. */
    private val custom = HashMap<String, TextView>()
    private var shown = false
    /** The content is on screen or sliding in; false while it slides out or is gone. */
    private var onScreen = vis.visible
    private var slidingOut = false
    private var animator: ValueAnimator? = null
    private var timerGen = 0
    private val hideTimer = Runnable { if (vis.expire(timerGen)) sync() }

    /** The bar's window: [root] at the top, clipped to a thin strip while hidden. */
    private val frame = object : FrameLayout(ctx) {
        override fun dispatchHoverEvent(ev: MotionEvent): Boolean {
            when (ev.actionMasked) {
                MotionEvent.ACTION_HOVER_ENTER -> pointer(true)
                MotionEvent.ACTION_HOVER_MOVE -> if (!vis.pointerInside) pointer(true)
                MotionEvent.ACTION_HOVER_EXIT -> pointer(false)
            }
            return super.dispatchHoverEvent(ev)
        }

        override fun dispatchTouchEvent(ev: MotionEvent): Boolean {
            if (ev.actionMasked == MotionEvent.ACTION_DOWN &&
                !ev.isFromSource(InputDevice.SOURCE_MOUSE)) {
                val wasVisible = vis.visible
                schedule(vis.touched())
                if (!wasVisible) {
                    sync()
                    return true // the touch only reveals the bar
                }
            }
            return super.dispatchTouchEvent(ev)
        }
    }

    init {
        root.orientation = LinearLayout.HORIZONTAL
        root.gravity = Gravity.CENTER_VERTICAL
        root.setPadding(desktop.dp(6f), 0, desktop.dp(6f), 0)

        workspaces.orientation = LinearLayout.HORIZONTAL
        workspaces.gravity = Gravity.CENTER_VERTICAL
        title.textSize = 13f
        title.isSingleLine = true
        title.ellipsize = TextUtils.TruncateAt.END
        title.gravity = Gravity.CENTER
        running.orientation = LinearLayout.HORIZONTAL
        running.gravity = Gravity.CENTER_VERTICAL
        clock.textSize = 13f
        clock.typeface = Typeface.DEFAULT_BOLD
        clock.setPadding(desktop.dp(10f), 0, desktop.dp(6f), 0)

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
        frame.addView(root, FrameLayout.LayoutParams(MATCH_PARENT, heightPx, Gravity.TOP))
        if (!onScreen) root.translationY = -heightPx.toFloat()
        layoutModules()
    }

    /**
     * Takes the bar's configuration from init.lua and [workspaceNames] (workspace rules'
     * names); a new height or auto-hide setting moves the reserved strip
     * ([Desktop.barReserved]).
     */
    fun configure(cfg: BarConfig, workspaceNames: Map<Int, String>) {
        names = workspaceNames
        vis.configure(cfg.autohide, cfg.autohideDelayMs)
        if (cfg == config) {
            update(state)
            sync()
            return
        }
        val heightChanged = cfg.heightDp != config.heightDp
        config = cfg
        heightPx = desktop.dp(cfg.heightDp.toFloat())
        if (heightChanged) {
            root.layoutParams = FrameLayout.LayoutParams(MATCH_PARENT, heightPx, Gravity.TOP)
            if (!onScreen) root.translationY = -heightPx.toFloat()
        }
        layoutModules()
        sync()
    }

    /**
     * The `bar` dispatcher: `show`, `hide`, `toggle`, `reveal` (an auto-hiding bar for a
     * while), `reload` (the bar re-created from the configuration in force, as a restarted
     * waybar: its views, its Lua modules and its configured visibility).
     */
    fun dispatch(action: String) {
        if (action == "reload") {
            reload()
            return
        }
        try {
            schedule(vis.dispatch(action))
        } catch (e: IllegalArgumentException) {
            Log.w(Desktop.TAG, "bar: ${e.message}")
            return
        }
        sync()
    }

    private fun reload() {
        animator?.cancel()
        desktop.handler.removeCallbacks(hideTimer)
        if (shown) wm.removeViewImmediate(frame)
        shown = false
        vis.reset()
        onScreen = vis.visible
        slidingOut = false
        root.alpha = 1f
        root.translationY = if (onScreen) 0f else -heightPx.toFloat()
        layoutModules()
        desktop.lua.restartModules()
        show()
        reportReserved()
        Log.i(Desktop.TAG, "bar reloaded")
    }

    private fun pointer(inside: Boolean) {
        val wasVisible = vis.visible
        schedule(if (inside) vis.pointerEntered() else vis.pointerLeft())
        if (vis.visible != wasVisible) sync()
    }

    /** Arms the hide timer for [delayMs], or leaves it when null. */
    private fun schedule(delayMs: Long?) {
        delayMs ?: return
        timerGen = vis.generation
        desktop.handler.removeCallbacks(hideTimer)
        desktop.handler.postDelayed(hideTimer, delayMs)
    }

    /** Brings the window, the slide and the reserved strip in line with [vis]. */
    private fun sync() {
        if (shown) {
            val want = vis.visible
            if (want != onScreen) {
                onScreen = want
                slide(want)
            } else {
                wm.updateViewLayout(frame, params())
            }
        }
        reportReserved()
    }

    /**
     * Tells hyprtile about a new reserved strip. A strip given back waits for the window's next
     * layout: WindowManager keeps freeform windows below the insets the bar still provides
     * (Task.computeFreeformBounds), so tiles laid out before the window dropped them would be
     * pushed down by the old strip and stay there.
     */
    private fun reportReserved() {
        val top = reservedTop
        if (top == reportedTop) return
        if (top > reportedTop || !shown) {
            publishReserved()
            return
        }
        val observer = frame.viewTreeObserver
        observer.addOnGlobalLayoutListener(object : ViewTreeObserver.OnGlobalLayoutListener {
            override fun onGlobalLayout() {
                frame.viewTreeObserver.removeOnGlobalLayoutListener(this)
                publishReserved()
            }
        })
        frame.requestLayout()
    }

    private fun publishReserved() {
        val top = reservedTop
        if (top == reportedTop) return
        reportedTop = top
        desktop.barReserved()
    }

    /** Slides (or fades) the content in or out with the layersIn/layersOut animation. */
    private fun slide(inward: Boolean) {
        animator?.cancel()
        val spec = anims.resolve(if (inward) "layersIn" else "layersOut")
        val fade = spec.style.trim().startsWith("fade")
        val duration = if (animationsEnabled) spec.durationMs else 0L
        val curve = anims.curveNamed(spec.curve)
        val h = heightPx.toFloat()
        slidingOut = !inward
        // The window takes the bar's height before the content comes in, and keeps it until
        // the content is out.
        wm.updateViewLayout(frame, params())
        if (fade) {
            root.translationY = 0f
            if (inward && root.alpha >= 1f) root.alpha = 0f
        } else {
            root.alpha = 1f
        }
        val fromT = root.translationY
        val toT = if (inward) 0f else -h
        val fromA = root.alpha
        val toA = if (inward) 1f else 0f
        val a = ValueAnimator.ofFloat(0f, 1f)
        a.duration = duration
        a.interpolator = android.animation.TimeInterpolator { curve.value(it) }
        a.addUpdateListener {
            val f = it.animatedValue as Float
            if (fade) root.alpha = fromA + (toA - fromA) * f
            else root.translationY = fromT + (toT - fromT) * f
        }
        a.addListener(object : AnimatorListenerAdapter() {
            private var cancelled = false
            override fun onAnimationCancel(animation: Animator) {
                cancelled = true
            }

            override fun onAnimationEnd(animation: Animator) {
                if (animator === animation) animator = null
                if (cancelled || inward) return
                slidingOut = false
                root.translationY = -h
                root.alpha = 1f
                if (shown) wm.updateViewLayout(frame, params())
            }
        })
        animator = a
        a.start()
    }

    /** The text of the Lua module [name]. */
    fun setModuleText(name: String, text: String) {
        custom[name]?.text = text
    }

    /** Puts the modules of the three groups into the bar, the centre one taking the room left. */
    private fun layoutModules() {
        val c = config
        for (v in listOf(workspaces, title, running, clock)) (v.parent as? LinearLayout)?.removeView(v)
        root.removeAllViews()
        custom.clear()
        root.setBackgroundColor(c.background)
        title.setTextColor(c.foreground)
        clock.setTextColor(c.foreground)
        clock.format24Hour = c.clockFormat
        clock.format12Hour = c.clockFormat
        fun group(names: List<String>, weight: Float, gravity: Int): LinearLayout {
            val g = LinearLayout(ctx)
            g.orientation = LinearLayout.HORIZONTAL
            g.gravity = gravity or Gravity.CENTER_VERTICAL
            for (n in names) module(n)?.let { g.addView(it) }
            root.addView(g, LinearLayout.LayoutParams(if (weight > 0) 0 else WRAP_CONTENT, MATCH_PARENT, weight))
            return g
        }
        group(c.left, 0f, Gravity.START)
        group(c.center, 1f, Gravity.CENTER_HORIZONTAL)
        group(c.right, 0f, Gravity.END)
        update(state)
    }

    private fun module(name: String): View? = when (name) {
        "apps" -> button("▦  Apps") { desktop.launcher.toggle() }
        "workspaces" -> workspaces.also { it.layoutParams = LinearLayout.LayoutParams(WRAP_CONTENT, MATCH_PARENT) }
        "title" -> title.also { it.layoutParams = LinearLayout.LayoutParams(WRAP_CONTENT, MATCH_PARENT) }
        "windows" -> running.also { it.layoutParams = LinearLayout.LayoutParams(WRAP_CONTENT, MATCH_PARENT) }
        "notifications" -> notificationsButton()
        "quicksettings" -> quickSettingsButton()
        "tray" -> LinearLayout(ctx).apply {
            gravity = Gravity.CENTER_VERTICAL
            addView(notificationsButton())
            addView(quickSettingsButton())
        }
        "clock" -> clock.also { it.layoutParams = LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT) }
        else -> config.modules[name]?.let { m ->
            button(m.text ?: "") { desktop.lua.click(m) }.also {
                it.tooltipText = m.tooltip
                custom[name] = it
            }
        }
    }

    private fun notificationsButton() = iconButton(android.R.drawable.ic_popup_reminder, "Notifications") {
        ctx.getSystemService(StatusBarManager::class.java)?.expandNotificationsPanel()
    }

    private fun quickSettingsButton() = iconButton(android.R.drawable.ic_menu_preferences, "Quick settings") {
        ctx.getSystemService(StatusBarManager::class.java)?.expandSettingsPanel()
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
        wm.addView(frame, params())
    }

    private fun params(): WindowManager.LayoutParams {
        val full = onScreen || slidingOut
        val touchable = full || vis.edgeStrip
        val lp = WindowManager.LayoutParams(
            MATCH_PARENT, if (full) heightPx else maxOf(2, desktop.dp(EDGE_DP)),
            TYPE_STATUS_BAR_ADDITIONAL,
            FLAG_NOT_FOCUSABLE or FLAG_LAYOUT_IN_SCREEN or FLAG_SPLIT_TOUCH or
                (if (touchable) 0 else FLAG_NOT_TOUCHABLE),
            PixelFormat.TRANSLUCENT,
        )
        lp.gravity = Gravity.TOP
        lp.title = "HyprBar"
        lp.setFitInsetsTypes(0)
        lp.layoutInDisplayCutoutMode = LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
        lp.setTrustedOverlay()
        // WindowManager refuses a different number of inset providers after the window is
        // added (relayoutWindow), so a bar that gives its strip back keeps them at size 0.
        val size = if (vis.reserves) Insets.of(0, heightPx, 0, 0) else Insets.NONE
        lp.providedInsets = arrayOf(
            InsetsFrameProvider(insetsOwner, 0, WindowInsets.Type.statusBars()).setInsetsSize(size),
            InsetsFrameProvider(insetsOwner, 0, WindowInsets.Type.tappableElement())
                .setInsetsSize(size),
        )
        return lp
    }

    /** Redraws workspaces, title and running apps from the shell's state. */
    fun update(s: ShellState) {
        state = s
        workspaces.removeAllViews()
        // Only the active and the occupied workspaces, as waybar's hyprland/workspaces (E2).
        for (n in s.shown()) {
            val occupied = s.windows.any { it.workspace == n }
            val b = button(names[n] ?: "$n") { desktop.dispatch("workspace", "$n") }
            b.minWidth = desktop.dp(26f)
            b.gravity = Gravity.CENTER
            when {
                n == s.active -> {
                    b.background = pill(config.accent)
                    b.setTextColor(config.background or 0xff000000.toInt())
                }
                occupied -> b.setTextColor(config.foreground)
                else -> b.setTextColor(DIM)
            }
            workspaces.addView(b, LinearLayout.LayoutParams(WRAP_CONTENT, desktop.dp(22f)).apply {
                marginStart = desktop.dp(2f)
            })
        }
        // Special workspaces with windows or shown (E5, waybar's show-special): click toggles.
        for ((_, name) in s.occupiedSpecials()) {
            val b = button(if (name == "special") "S" else "S:$name") {
                desktop.dispatch("togglespecialworkspace", name)
            }
            b.minWidth = desktop.dp(26f)
            b.gravity = Gravity.CENTER
            if (name == s.specialShown) {
                b.background = pill(config.accent)
                b.setTextColor(config.background or 0xff000000.toInt())
            } else {
                b.setTextColor(config.foreground)
            }
            workspaces.addView(b, LinearLayout.LayoutParams(WRAP_CONTENT, desktop.dp(22f)).apply {
                marginStart = desktop.dp(2f)
            })
        }

        val focused = s.windows.find { it.taskId == s.focusedTask }
        title.text = when {
            focused == null && s.specialShown != null -> "special:${s.specialShown}"
            focused == null -> if (s.active == 0) "" else "workspace ${names[s.active] ?: s.active}"
            focused.title.isNotEmpty() -> focused.title
            else -> desktop.apps.label(focused.packageName)
        }

        running.removeAllViews()
        for (w in s.windows) {
            val icon = ImageView(ctx)
            icon.setImageDrawable(desktop.apps.iconOf(w.packageName))
            val pad = desktop.dp(3f)
            icon.setPadding(pad, pad, pad, pad)
            icon.alpha = if (s.isVisible(w)) 1f else 0.45f
            if (w.taskId == s.focusedTask) icon.background = pill(FOCUS_BG)
            icon.tooltipText = "${w.title.ifEmpty { desktop.apps.label(w.packageName) }} " +
                "(workspace ${s.label(w.workspace, names)})"
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
        b.setTextColor(config.foreground)
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
        v.imageTintList = ColorStateList.valueOf(config.foreground)
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
        /** The strip along the top edge an auto-hiding bar keeps for the pointer. */
        const val EDGE_DP = 2f
        val BG = Color.parseColor("#E61E1E2E")
        val BG_SOLID = Color.parseColor("#1E1E2E")
        val FG = Color.parseColor("#CDD6F4")
        val DIM = Color.parseColor("#6C7086")
        val ACCENT = Color.parseColor("#33CCFF")
        val FOCUS_BG = Color.parseColor("#4433CCFF")
    }
}
