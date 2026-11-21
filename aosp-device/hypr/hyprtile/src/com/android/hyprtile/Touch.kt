package com.android.hyprtile

import kotlin.math.abs
import kotlin.math.hypot

/**
 * Touch gestures (Phase D6), HarmonyOS PC mode style. A gesture is bound like a key: its bind
 * has no modifiers and the key `GESTURE:<NAME>` (Lua `hl.bind("gesture:edge_bottom", ...)`).
 *
 * | Name | Gesture |
 * |---|---|
 * | `EDGE_BOTTOM`, `EDGE_TOP`, `EDGE_LEFT`, `EDGE_RIGHT` | one finger swiping in from that screen edge |
 * | `EDGE_TOP_RIGHT` | from the right third of the top edge (before `EDGE_TOP` there) |
 * | `SWIPE_<N>_<DIR>` | N = 3..5 fingers swiping `UP`, `DOWN`, `LEFT` or `RIGHT`, anywhere |
 * | `PINCH_IN`, `PINCH_OUT` | two fingers on the wallpaper or the bar (not on a window) |
 */
object TouchGesture {
    const val PREFIX = "GESTURE:"

    val EDGES = listOf("EDGE_BOTTOM", "EDGE_TOP", "EDGE_TOP_RIGHT", "EDGE_LEFT", "EDGE_RIGHT")
    val DIRS = listOf("UP", "DOWN", "LEFT", "RIGHT")
    val NAMES: Set<String> = (EDGES + listOf("PINCH_IN", "PINCH_OUT") +
        (3..5).flatMap { n -> DIRS.map { "SWIPE_${n}_$it" } }).toSet()

    fun isGestureKey(key: String) = key.uppercase().startsWith(PREFIX)

    /** The gesture name of a bind key (`GESTURE:EDGE_LEFT` → `EDGE_LEFT`); throws if unknown. */
    fun nameOf(key: String): String {
        val name = key.uppercase().removePrefix(PREFIX)
        require(name in NAMES) { "unknown gesture $name (${NAMES.sorted().joinToString()})" }
        return name
    }

    fun key(name: String) = PREFIX + name
}

/** The default gesture binds: launcher, workspaces, notifications, quick settings, overview. */
fun defaultTouchBinds(): List<Bind> = listOf(
    Bind(emptySet(), TouchGesture.key("EDGE_BOTTOM"), "launcher"),
    Bind(emptySet(), TouchGesture.key("EDGE_LEFT"), "workspace", "e-1"),
    Bind(emptySet(), TouchGesture.key("EDGE_RIGHT"), "workspace", "e+1"),
    Bind(emptySet(), TouchGesture.key("EDGE_TOP"), "notifications"),
    Bind(emptySet(), TouchGesture.key("EDGE_TOP_RIGHT"), "quicksettings"),
    Bind(emptySet(), TouchGesture.key("PINCH_IN"), "overview"),
    Bind(emptySet(), TouchGesture.key("SWIPE_3_UP"), "overview"),
)

/**
 * Turns Hyprland's `gesture` (`hl.gesture({ fingers = 3, direction = "horizontal",
 * action = "workspace" })`) into gesture binds. Swipes need 3..5 fingers (two-finger swipes
 * are app scrolling); pinches are two fingers. `workspace` moves to the next workspace on a
 * swipe left or up and the previous one on a swipe right or down, as Hyprland does; `close`,
 * `fullscreen` and `float` are the dispatchers of the same names; anything else is a
 * dispatcher line (`name arg`). Throws for what cannot be played.
 */
fun gestureBinds(g: Gesture): List<Bind> {
    val pinch = g.direction.startsWith("pinch")
    if (pinch) {
        require(g.fingers == 2) { "pinch gestures take 2 fingers" }
    } else {
        require(g.fingers in 3..5) { "swipe gestures take 3 to 5 fingers (2 fingers scroll apps)" }
    }
    val names = when (g.direction) {
        "pinch", "pinchin" -> listOf("PINCH_IN")
        "pinchout" -> listOf("PINCH_OUT")
        "horizontal" -> listOf("LEFT", "RIGHT")
        "vertical" -> listOf("UP", "DOWN")
        "swipe" -> TouchGesture.DIRS
        else -> listOf(g.direction.uppercase())
    }.map { if (pinch) it else "SWIPE_${g.fingers}_$it" }
    require(g.mods.isBlank()) { "gestures with modifiers are not supported" }
    return names.map { name ->
        val (dispatcher, arg) = when (val a = g.action.trim()) {
            "workspace" -> "workspace" to
                (if (name.endsWith("LEFT") || name.endsWith("UP") || name == "PINCH_IN") "e+1" else "e-1")
            "close" -> "killactive" to ""
            "fullscreen" -> "fullscreen" to "0"
            "float" -> "togglefloating" to ""
            else -> {
                val parts = a.split(Regex("\\s+"), limit = 2)
                require(parts[0] in DISPATCHERS) { "unsupported gesture action '$a'" }
                parts[0] to parts.getOrElse(1) { "" }
            }
        }
        Bind(emptySet(), TouchGesture.key(name), dispatcher, arg)
    }
}

/** Sizes of the touch recognizer, in pixels and milliseconds. */
data class TouchConfig(
    val width: Int,
    val height: Int,
    /** How far from a screen edge a swipe must start. */
    val edge: Int = 24,
    /** How far a swipe must travel. */
    val distance: Int = 80,
    /** Movement that still counts as holding still. */
    val slop: Int = 16,
    /** A swipe must reach [distance] within this time. */
    val swipeTime: Long = 700,
)

/**
 * Recognizes edge swipes, multi-finger swipes and pinches from raw pointer positions (display
 * pixels), with no Android types so it can be unit-tested. Feed it every event of a gesture;
 * [move] returns the candidate names, most specific first, once a gesture is recognized (then
 * nothing more until all fingers are up). The caller pilfers the pointers and runs the first
 * candidate that has a bind.
 */
class TouchRecognizer(var config: TouchConfig) {
    private class Finger(val x0: Float, val y0: Float, var x: Float, var y: Float, val onWindow: Boolean)

    private val fingers = LinkedHashMap<Int, Finger>()
    private var maxFingers = 0
    private var edge: String? = null
    private var t0 = 0L
    private var done = false
    private var pinchD0 = 0f

    /** Whether a gesture was recognized in this touch. */
    val recognized get() = done

    /**
     * The first finger is down. [excluded]: the point is in the apps' system gesture exclusion
     * region (no edge swipe there). [onWindow]: it is on an app window (no pinch).
     */
    fun down(id: Int, x: Float, y: Float, time: Long, excluded: Boolean, onWindow: Boolean) {
        fingers.clear()
        fingers[id] = Finger(x, y, x, y, onWindow)
        maxFingers = 1
        done = false
        t0 = time
        val c = config
        edge = if (excluded) null else when {
            y >= c.height - c.edge -> "EDGE_BOTTOM"
            y < c.edge -> "EDGE_TOP"
            x < c.edge -> "EDGE_LEFT"
            x >= c.width - c.edge -> "EDGE_RIGHT"
            else -> null
        }
    }

    fun pointerDown(id: Int, x: Float, y: Float, time: Long, onWindow: Boolean) {
        if (done) return
        fingers[id] = Finger(x, y, x, y, onWindow)
        if (fingers.size > maxFingers) {
            maxFingers = fingers.size
            // A new finger restarts the count: they rarely land together.
            t0 = time
            for (k in fingers.keys.toList()) {
                val f = fingers.getValue(k)
                fingers[k] = Finger(f.x, f.y, f.x, f.y, f.onWindow)
            }
            if (fingers.size == 2) pinchD0 = spread()
        }
        edge = null
    }

    fun pointerUp(id: Int) {
        fingers.remove(id)
    }

    /** Pointers moved; [points] holds id to (x, y). Returns gesture candidates or null. */
    fun move(points: Map<Int, Pair<Float, Float>>, time: Long): List<String>? {
        if (done) return null
        for ((id, p) in points) fingers[id]?.let { it.x = p.first; it.y = p.second }
        val c = config
        val n = fingers.size
        if (time - t0 > c.swipeTime && edge != null) edge = null
        if (n == 1 && maxFingers == 1) {
            val e = edge ?: return null
            val f = fingers.values.first()
            val dx = f.x - f.x0
            val dy = f.y - f.y0
            val (inward, across) = when (e) {
                "EDGE_BOTTOM" -> -dy to dx
                "EDGE_TOP" -> dy to dx
                "EDGE_LEFT" -> dx to dy
                else -> -dx to dy
            }
            if (inward < 0 || abs(across) > maxOf(inward, c.slop.toFloat())) {
                // Went the wrong way or sideways: not an edge swipe.
                if (hypot(dx, dy) > c.slop) edge = null
                return null
            }
            if (inward < c.distance) return null
            done = true
            return if (e == "EDGE_TOP" && f.x0 >= c.width * 2f / 3) listOf("EDGE_TOP_RIGHT", "EDGE_TOP") else listOf(e)
        }
        if (n == 2 && maxFingers == 2) {
            if (fingers.values.any { it.onWindow }) return null
            val d = spread()
            if (pinchD0 <= 0f) return null
            if (d < pinchD0 * 0.6f && pinchD0 - d > c.slop) return finish("PINCH_IN")
            if (d > pinchD0 * 1.5f && d - pinchD0 > c.slop) return finish("PINCH_OUT")
            return null
        }
        if (n >= 3 && n == maxFingers && n <= 5) {
            if (time - t0 > c.swipeTime * 2) return null
            val dx = fingers.values.map { it.x - it.x0 }
            val dy = fingers.values.map { it.y - it.y0 }
            val mx = dx.average().toFloat()
            val my = dy.average().toFloat()
            val horizontal = abs(mx) > abs(my)
            val main = if (horizontal) mx else my
            if (abs(main) < c.distance) return null
            // Every finger must go the same way.
            val each = if (horizontal) dx else dy
            if (each.any { it * main <= 0 || abs(it) < c.distance / 2 }) return null
            val dir = when {
                horizontal && mx < 0 -> "LEFT"
                horizontal -> "RIGHT"
                my < 0 -> "UP"
                else -> "DOWN"
            }
            return finish("SWIPE_${n}_$dir")
        }
        return null
    }

    private fun finish(name: String): List<String> {
        done = true
        return listOf(name)
    }

    private fun spread(): Float {
        val f = fingers.values.toList()
        if (f.size < 2) return 0f
        return hypot(f[0].x - f[1].x, f[0].y - f[1].y)
    }
}
