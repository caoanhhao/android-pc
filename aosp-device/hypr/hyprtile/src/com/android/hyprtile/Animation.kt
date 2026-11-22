package com.android.hyprtile

import kotlin.math.abs
import kotlin.math.roundToInt
import kotlin.math.roundToLong

/**
 * A cubic bezier timing curve from (0,0) to (1,1) with control points ([x1],[y1]) and ([x2],[y2]),
 * Hyprland's `bezier = name, x1, y1, x2, y2` (CSS's cubic-bezier). y may leave 0..1 to overshoot.
 */
data class Bezier(val x1: Float, val y1: Float, val x2: Float, val y2: Float) {
    init {
        require(x1 in 0f..1f && x2 in 0f..1f) { "bezier x must be in 0..1" }
    }

    private fun coord(t: Float, p1: Float, p2: Float): Float {
        val u = 1 - t
        return 3 * u * u * t * p1 + 3 * u * t * t * p2 + t * t * t
    }

    private fun slopeX(t: Float): Float {
        val u = 1 - t
        return 3 * u * u * x1 + 6 * u * t * (x2 - x1) + 3 * t * t * (1 - x2)
    }

    /** The curve's value at time [x] (0..1): solves x(t) = [x] for t, returns y(t). */
    fun value(x: Float): Float {
        if (x <= 0f) return 0f
        if (x >= 1f) return 1f
        var t = x
        repeat(8) {
            val err = coord(t, x1, x2) - x
            if (abs(err) < 1e-5f) return coord(t, y1, y2)
            val d = slopeX(t)
            if (abs(d) < 1e-6f) return@repeat
            t = (t - err / d).coerceIn(0f, 1f)
        }
        // Newton did not settle (flat spots): bisection, x(t) is monotonic for x1, x2 in 0..1.
        var lo = 0f
        var hi = 1f
        t = x
        repeat(40) {
            val cx = coord(t, x1, x2)
            if (abs(cx - x) < 1e-6f) return coord(t, y1, y2)
            if (cx < x) lo = t else hi = t
            t = (lo + hi) / 2
        }
        return coord(t, y1, y2)
    }

    companion object {
        val LINEAR = Bezier(0f, 0f, 1f, 1f)
        /** Hyprland's built-in `default` curve. */
        val DEFAULT = Bezier(0f, 0.75f, 0.15f, 1f)
    }
}

/**
 * One node of Hyprland's animation tree, `animation = NAME, ONOFF, SPEED, CURVE[, STYLE]`.
 * [speed] is in deciseconds (Hyprland's unit): 4.79 plays for 479 ms.
 */
data class AnimSpec(
    val enabled: Boolean = true,
    val speed: Float = 10f,
    val curve: String = "default",
    val style: String = "",
) {
    val durationMs: Long get() = if (enabled) (speed * 100).roundToLong().coerceAtLeast(1) else 0
}

/** How a window appears and disappears (`windowsIn`/`windowsOut` style). */
sealed interface WindowStyle {
    /** Grows from [percent] % of its size (shrinks to it when closing), around its centre. */
    data class Popin(val percent: Int = 80) : WindowStyle

    /** Slides in from (out to) [edge] of the display, or from the nearest one when null. */
    data class Slide(val edge: Edge? = null) : WindowStyle

    /** Only fades. */
    data object Fade : WindowStyle

    enum class Edge { LEFT, RIGHT, TOP, BOTTOM }

    companion object {
        fun parse(text: String): WindowStyle {
            val parts = text.trim().lowercase().split(Regex("\\s+")).filter { it.isNotEmpty() }
            return when (parts.firstOrNull()) {
                null, "popin" -> Popin(parts.getOrNull(1)?.let { percent(it) } ?: 80)
                "slide" -> Slide(parts.getOrNull(1)?.let {
                    when (it) {
                        "left" -> Edge.LEFT
                        "right" -> Edge.RIGHT
                        "top" -> Edge.TOP
                        "bottom" -> Edge.BOTTOM
                        else -> throw IllegalArgumentException("bad slide edge $it")
                    }
                })
                "fade" -> Fade
                // gnomed (shrink to a line) is drawn as a popin from nothing.
                "gnomed" -> Popin(0)
                else -> throw IllegalArgumentException("bad window style $text")
            }
        }
    }
}

/** How workspaces switch (`workspaces` style): slide, slidevert, fade, slidefade[vert] N%. */
data class WorkspaceStyle(
    val slide: Boolean = true,
    val vertical: Boolean = false,
    /** How far the desks move, in percent of the display. */
    val slidePercent: Int = 100,
    val fade: Boolean = false,
) {
    /** How far a workspace moves when it slides on a [width] x [height] display (x, y). */
    fun shift(width: Int, height: Int): Pair<Float, Float> {
        if (!slide) return 0f to 0f
        return if (vertical) 0f to height * slidePercent / 100f else width * slidePercent / 100f to 0f
    }

    companion object {
        fun parse(text: String): WorkspaceStyle {
            val parts = text.trim().lowercase().split(Regex("\\s+")).filter { it.isNotEmpty() }
            val pct = parts.getOrNull(1)?.let { percent(it) }
            return when (parts.firstOrNull()) {
                null, "slide" -> WorkspaceStyle()
                "slidevert" -> WorkspaceStyle(vertical = true)
                "fade" -> WorkspaceStyle(slide = false, fade = true)
                "slidefade" -> WorkspaceStyle(slidePercent = pct ?: 20, fade = true)
                "slidefadevert" -> WorkspaceStyle(vertical = true, slidePercent = pct ?: 20, fade = true)
                else -> throw IllegalArgumentException("bad workspace style $text")
            }
        }
    }
}

private fun percent(s: String): Int =
    s.removeSuffix("%").toFloat().roundToInt().also { require(it in 0..100) { "bad percent $s" } }

/**
 * All animation settings, Hyprland's `bezier` and `animation` keywords. Every animation name is
 * a node of Hyprland's tree; a node that is not set takes its parent's settings, a style left
 * empty takes the parent's style. [ANIMATIONS] lists the nodes used here.
 */
data class AnimationConfig(
    val beziers: Map<String, Bezier> = DEFAULT_BEZIERS,
    val animations: Map<String, AnimSpec> = DEFAULT_ANIMATIONS,
) {
    /** The effective settings of [name] after inheritance. */
    fun resolve(name: String): AnimSpec {
        require(name in PARENT || name == "global") { "unknown animation $name" }
        var spec: AnimSpec? = null
        var node: String? = name
        var style = ""
        while (node != null) {
            val s = animations[node]
            if (s != null) {
                if (spec == null) spec = s
                if (style.isEmpty()) style = s.style
                if (style.isNotEmpty()) break
            }
            node = PARENT[node]
        }
        return (spec ?: AnimSpec()).copy(style = style)
    }

    /** The curve [name] uses; unknown curve names fall back to Hyprland's `default`. */
    fun curve(name: String): Bezier = curveNamed(resolve(name).curve)

    fun curveNamed(curve: String): Bezier = beziers[curve] ?: when (curve) {
        "linear" -> Bezier.LINEAR
        else -> Bezier.DEFAULT
    }

    /** Adds `bezier = name, x1, y1, x2, y2`. */
    fun withBezier(line: String): AnimationConfig {
        val p = line.split(',').map { it.trim() }
        require(p.size == 5) { "bezier needs a name and four numbers: $line" }
        return copy(beziers = beziers + (p[0] to Bezier(p[1].toFloat(), p[2].toFloat(),
            p[3].toFloat(), p[4].toFloat())))
    }

    /** Adds `animation = NAME, ONOFF, SPEED, CURVE[, STYLE]` (speed and curve optional when off). */
    fun withAnimation(line: String): AnimationConfig {
        val p = line.split(',').map { it.trim() }
        require(p.size >= 2) { "animation needs a name and on/off: $line" }
        val name = p[0]
        require(name in PARENT || name == "global") { "unknown animation $name" }
        val on = when (p[1]) {
            "1", "on", "true", "yes" -> true
            "0", "off", "false", "no" -> false
            else -> throw IllegalArgumentException("bad on/off ${p[1]}")
        }
        val spec = if (on) {
            require(p.size >= 4) { "animation needs speed and curve: $line" }
            val speed = p[2].toFloat().also { require(it > 0f) { "speed must be positive" } }
            AnimSpec(true, speed, p[3], p.drop(4).joinToString(",").trim())
        } else {
            AnimSpec(enabled = false, speed = p.getOrNull(2)?.toFloatOrNull() ?: 1f,
                curve = p.getOrNull(3) ?: "default", style = p.drop(4).joinToString(",").trim())
        }
        if (spec.style.isNotEmpty()) checkStyle(name, spec.style)
        return copy(animations = animations + (name to spec))
    }

    fun windowStyle(name: String): WindowStyle = WindowStyle.parse(resolve(name).style)

    fun workspaceStyle(name: String): WorkspaceStyle = WorkspaceStyle.parse(resolve(name).style)

    companion object {
        /** Hyprland's animation tree: node to parent (the nodes this shell plays). */
        val PARENT: Map<String, String> = mapOf(
            "windows" to "global",
            "windowsIn" to "windows",
            "windowsOut" to "windows",
            "windowsMove" to "windows",
            "fade" to "global",
            "fadeIn" to "fade",
            "fadeOut" to "fade",
            "fadeSwitch" to "fade",
            "border" to "global",
            "workspaces" to "global",
            "workspacesIn" to "workspaces",
            "workspacesOut" to "workspaces",
            // Special workspaces (E5) show and hide with these.
            "specialWorkspace" to "workspaces",
            "specialWorkspaceIn" to "specialWorkspace",
            "specialWorkspaceOut" to "specialWorkspace",
            // The bar (Phase E7): Hyprland animates layer surfaces (waybar) with these.
            "layers" to "global",
            "layersIn" to "layers",
            "layersOut" to "layers",
        )
        val ANIMATIONS: Set<String> = PARENT.keys + "global"

        private val WORKSPACE_NODES = setOf("workspaces", "workspacesIn", "workspacesOut",
            "specialWorkspace", "specialWorkspaceIn", "specialWorkspaceOut")
        private val WINDOW_NODES = setOf("windows", "windowsIn", "windowsOut", "windowsMove")

        private fun checkStyle(name: String, style: String) {
            when (name) {
                in WORKSPACE_NODES -> WorkspaceStyle.parse(style)
                in WINDOW_NODES -> WindowStyle.parse(style)
            }
        }

        /** The curves of Hyprland's example configuration. */
        val DEFAULT_BEZIERS: Map<String, Bezier> = mapOf(
            "easeOutQuint" to Bezier(0.23f, 1f, 0.32f, 1f),
            "easeInOutCubic" to Bezier(0.65f, 0.05f, 0.36f, 1f),
            "linear" to Bezier.LINEAR,
            "almostLinear" to Bezier(0.5f, 0.5f, 0.75f, 1f),
            "quick" to Bezier(0.15f, 0f, 0.1f, 1f),
        )

        /**
         * Hyprland's example configuration, except workspaces: a 250 ms easeOutQuint slide
         * (Hyprland's own default style) instead of its almostLinear fade, and layers (the bar):
         * a slide instead of a fade, and special workspaces: they slide down from the top edge
         * (`slidevert`), so they read apart from a workspace switch.
         */
        val DEFAULT_ANIMATIONS: Map<String, AnimSpec> = mapOf(
            "global" to AnimSpec(true, 10f, "default"),
            "border" to AnimSpec(true, 5.39f, "easeOutQuint"),
            "windows" to AnimSpec(true, 4.79f, "easeOutQuint"),
            "windowsIn" to AnimSpec(true, 4.1f, "easeOutQuint", "popin 87%"),
            "windowsOut" to AnimSpec(true, 1.49f, "linear", "popin 87%"),
            "fadeIn" to AnimSpec(true, 1.73f, "almostLinear"),
            "fadeOut" to AnimSpec(true, 1.46f, "almostLinear"),
            "fade" to AnimSpec(true, 3.03f, "quick"),
            "workspaces" to AnimSpec(true, 2.5f, "easeOutQuint", "slide"),
            "specialWorkspace" to AnimSpec(true, 3f, "easeOutQuint", "slidevert"),
            "layers" to AnimSpec(true, 3.81f, "easeOutQuint", "slide"),
            "layersIn" to AnimSpec(true, 4f, "easeOutQuint"),
            "layersOut" to AnimSpec(true, 1.5f, "linear"),
        )
    }
}
