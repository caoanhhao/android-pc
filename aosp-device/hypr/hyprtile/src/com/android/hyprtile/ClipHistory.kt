package com.android.hyprtile

/**
 * The clipboard history behind `clipboardhistory` (Phase E9): the last [capacity] text clips,
 * newest first, in memory only. A clip copied again moves to the front instead of showing twice;
 * sensitive clips (passwords, `ClipDescription.EXTRA_IS_SENSITIVE`), blank text and text over
 * [maxChars] are never kept. Not thread-safe: one thread (SystemUI's main thread) owns it.
 */
class ClipHistory(val capacity: Int = 25, val maxChars: Int = 64 * 1024) {
    private val entries = ArrayList<String>()

    /** The entries, newest first. */
    val items: List<String> get() = entries.toList()

    /**
     * Records a new primary clip. Returns whether it was kept (false for sensitive, blank or
     * oversized text).
     */
    fun add(text: String?, sensitive: Boolean = false): Boolean {
        if (sensitive || text.isNullOrBlank() || text.length > maxChars) return false
        entries.remove(text)
        entries.add(0, text)
        while (entries.size > capacity) entries.removeAt(entries.size - 1)
        return true
    }

    fun remove(text: String) = entries.remove(text)

    fun clear() = entries.clear()

    /**
     * The entries whose text contains every word of [query] (case-insensitive, any order),
     * newest first; all entries for a blank query.
     */
    fun filter(query: String): List<String> {
        val words = query.lowercase().split(Regex("\\s+")).filter { it.isNotEmpty() }
        if (words.isEmpty()) return items
        return entries.filter { e -> val l = e.lowercase(); words.all { it in l } }
    }

    companion object {
        /** One line for the picker: whitespace runs as single spaces, cut at [max] chars. */
        fun preview(text: String, max: Int = 120): String {
            val flat = text.trim().replace(Regex("\\s+"), " ")
            return if (flat.length <= max) flat else flat.take(max - 1) + "…"
        }
    }
}

/** The capture region of the `screenshot` dispatcher (E9): `area` (default) or `full`. */
enum class ScreenshotMode {
    AREA, FULL;

    companion object {
        /** `area`/`region`/`partial` (or empty), `full`/`fullscreen`/`screen`/`now`; null else. */
        fun parse(arg: String): ScreenshotMode? = when (arg.trim().lowercase()) {
            "", "area", "region", "partial" -> AREA
            "full", "fullscreen", "screen", "now", "output" -> FULL
            else -> null
        }
    }
}
