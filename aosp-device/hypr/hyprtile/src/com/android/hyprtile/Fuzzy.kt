package com.android.hyprtile

/**
 * Fuzzy matching for HyprLauncher's app launcher (Phase D4), in the spirit of rofi's and fzf's:
 * the query's characters must appear in order in the candidate, case-insensitively. Matches
 * score higher when they are consecutive, start a word, or start the candidate; gaps cost a
 * little. Returns null when the query does not match.
 */
fun fuzzyScore(query: String, candidate: String): Int? {
    val q = query.trim().lowercase()
    if (q.isEmpty()) return 0
    val c = candidate.lowercase()
    // Best score over every place the first character can match: greedy from each start.
    var best: Int? = null
    var start = c.indexOf(q[0])
    while (start >= 0) {
        val s = scoreFrom(q, c, candidate, start)
        if (s != null && (best == null || s > best)) best = s
        start = c.indexOf(q[0], start + 1)
    }
    return best
}

private fun scoreFrom(q: String, c: String, original: String, start: Int): Int? {
    var score = 0
    var ci = start
    var prev = -2
    for (ch in q) {
        while (ci < c.length && c[ci] != ch) ci++
        if (ci == c.length) return null
        score += 10
        if (ci == prev + 1) score += 15
        if (ci == 0) score += 20
        else if (isWordStart(original, ci)) score += 12
        else if (prev >= 0) score -= minOf(ci - prev - 1, 5)
        prev = ci
        ci++
    }
    // Shorter candidates first among equal matches.
    return score - c.length / 8
}

private fun isWordStart(s: String, i: Int): Boolean {
    val p = s[i - 1]
    return !p.isLetterOrDigit() || (p.isLowerCase() && s[i].isUpperCase())
}

/**
 * The [items] matching [query], best first; [label] gives the text to match. Ties keep the
 * input order (callers sort by name or by use first).
 */
fun <T> fuzzyFilter(query: String, items: List<T>, label: (T) -> String): List<T> {
    if (query.isBlank()) return items
    return items.mapIndexedNotNull { i, item -> fuzzyScore(query, label(item))?.let { Triple(it, i, item) } }
        .sortedWith(compareByDescending<Triple<Int, Int, T>> { it.first }.thenBy { it.second })
        .map { it.third }
}
