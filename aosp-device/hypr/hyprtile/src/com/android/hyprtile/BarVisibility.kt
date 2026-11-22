package com.android.hyprtile

/**
 * Whether the bar shows and whether it reserves its strip (Phase E7): waybar's
 * `SIGUSR1` toggle and an auto-hide in the style of waybar's `autohide` patches.
 *
 * The user's dispatcher (`bar show|hide|toggle`) sets [pinned]: a pinned bar shows and keeps
 * its strip out of the work area (the exclusive zone); an unpinned one gives the strip back to
 * the tiles. With [autohide] an unpinned bar still comes back, over the windows and without a
 * re-layout, when the pointer reaches its edge (or a touch lands there, or `bar reveal` runs),
 * and goes again [hideDelayMs] after the pointer left it. Without [autohide] an unpinned bar
 * stays away until it is shown again. A new configuration starts pinned unless it auto-hides.
 *
 * Plain state; the caller runs the timer: when a method returns a delay, it calls [expire]
 * that many ms later with the [generation] it read then.
 */
class BarVisibility(autohide: Boolean = false, var hideDelayMs: Long = DEFAULT_HIDE_DELAY_MS) {
    var autohide: Boolean = autohide
        private set
    var pinned: Boolean = !autohide
        private set
    /** Shown over the windows by the pointer, a touch or `bar reveal` (unpinned only). */
    var revealed: Boolean = false
        private set
    var pointerInside: Boolean = false
        private set
    /** Bumped by every change that makes a pending hide timer stale. */
    var generation: Int = 0
        private set

    /** The bar's content is on screen. */
    val visible: Boolean get() = pinned || revealed

    /** The bar keeps its strip out of the work area (insets and hyprtile's reserved strip). */
    val reserves: Boolean get() = pinned

    /** An unpinned auto-hiding bar keeps a thin edge strip that catches the pointer. */
    val edgeStrip: Boolean get() = autohide && !visible

    /** A new configuration: [autohide] changed resets the bar to its configured state. */
    fun configure(autohide: Boolean, hideDelayMs: Long) {
        this.hideDelayMs = hideDelayMs
        if (autohide == this.autohide) return
        this.autohide = autohide
        pinned = !autohide
        revealed = false
        generation++
    }

    /** Back to the configured state, as a restarted bar (`bar reload`). */
    fun reset() {
        pinned = !autohide
        revealed = false
        generation++
    }

    /**
     * Runs the `bar` dispatcher's [action] (`show`, `hide`, `toggle`, `reveal`); returns the
     * hide delay to schedule, or null. Unknown actions throw IllegalArgumentException.
     */
    fun dispatch(action: String): Long? {
        when (action) {
            "show" -> pin(true)
            "hide" -> pin(false)
            "toggle" -> pin(!pinned)
            "reveal" -> return reveal(TOUCH_HIDE_DELAY_MS)
            else -> throw IllegalArgumentException("bar takes show, hide, toggle, reveal or reload")
        }
        return null
    }

    private fun pin(on: Boolean) {
        pinned = on
        revealed = false
        generation++
    }

    /** The pointer entered the bar's window (its edge strip or the revealed bar). */
    fun pointerEntered(): Long? {
        pointerInside = true
        if (autohide && !pinned && !revealed) {
            revealed = true
            generation++
        }
        return null
    }

    /** The pointer left the bar's window: hide later unless it comes back. */
    fun pointerLeft(): Long? {
        pointerInside = false
        if (!revealed) return null
        generation++
        return hideDelayMs
    }

    /** A touch on the strip or the revealed bar: show for a while, as there is no pointer. */
    fun touched(): Long? = if (autohide && !pinned) reveal(TOUCH_HIDE_DELAY_MS) else null

    private fun reveal(delayMs: Long): Long? {
        if (pinned || !autohide) return null
        revealed = true
        generation++
        return if (pointerInside) null else maxOf(delayMs, hideDelayMs)
    }

    /** The hide timer of [gen] fired; returns true when the bar hid. */
    fun expire(gen: Int): Boolean {
        if (gen != generation || !revealed || pointerInside) return false
        revealed = false
        generation++
        return true
    }

    companion object {
        const val DEFAULT_HIDE_DELAY_MS = 500L
        /** How long a touch or `bar reveal` keeps the bar out at least. */
        const val TOUCH_HIDE_DELAY_MS = 3000L
        val ACTIONS = setOf("show", "hide", "toggle", "reveal", "reload")
    }
}
