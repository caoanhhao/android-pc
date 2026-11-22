package com.android.hyprtop

import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.RectF
import android.graphics.Typeface
import android.util.TypedValue
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.View
import kotlin.math.ceil
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt

/**
 * btop's default layout drawn on a canvas in character cells: the cpu box on top (a history
 * graph and one meter per core), mem (disks in their own column) and net on the lower left, the
 * process list on the lower right. Keyboard: arrows/PageUp/PageDown/Home/End select, c/m/p/n
 * sort, r reverses, f force-stops the selected app, q or Esc closes; the wheel scrolls and a
 * click selects a row or sorts by a column header.
 */
class TopView(context: Context) : View(context) {
    enum class Sort { CPU, MEM, PID, NAME }

    /** Called with the package name of the process the user asked to force-stop. */
    var onForceStop: ((String) -> Unit)? = null
    var onQuit: (() -> Unit)? = null

    private var snap: Snapshot? = null
    private val history = FloatArray(HISTORY)
    private val rxHistory = LongArray(HISTORY)
    private val txHistory = LongArray(HISTORY)
    private var samples = 0
    private var sort = Sort.CPU
    private var reverse = false
    private var rows: List<Proc> = emptyList()
    private var selected = 0
    private var scroll = 0
    private var procListTop = 0f
    private var procListRows = 0
    private var procHeaderY = 0f
    private var columnStarts = FloatArray(0)
    private var status: String? = null

    private val text = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        typeface = Typeface.MONOSPACE
        textSize = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_SP, 13f, resources.displayMetrics)
    }
    private val stroke = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeWidth = resources.displayMetrics.density
    }
    private val fill = Paint(Paint.ANTI_ALIAS_FLAG)
    private val cw get() = text.measureText("M")
    private val ch get() = text.fontSpacing

    init {
        isFocusable = true
        isFocusableInTouchMode = true
        setBackgroundColor(BG)
    }

    fun update(s: Snapshot) {
        snap = s
        System.arraycopy(history, 1, history, 0, HISTORY - 1)
        history[HISTORY - 1] = s.total
        System.arraycopy(rxHistory, 1, rxHistory, 0, HISTORY - 1)
        rxHistory[HISTORY - 1] = s.rxBytesPerSec
        System.arraycopy(txHistory, 1, txHistory, 0, HISTORY - 1)
        txHistory[HISTORY - 1] = s.txBytesPerSec
        samples++
        resort()
        invalidate()
    }

    fun showStatus(message: String) {
        status = message
        invalidate()
    }

    private fun resort() {
        val selectedPid = rows.getOrNull(selected)?.pid
        val procs = snap?.procs ?: emptyList()
        val sorted = when (sort) {
            Sort.CPU -> procs.sortedWith(compareByDescending<Proc> { it.cpu }.thenByDescending { it.rssKb })
            Sort.MEM -> procs.sortedByDescending { it.rssKb }
            Sort.PID -> procs.sortedBy { it.pid }
            Sort.NAME -> procs.sortedBy { it.name.lowercase() }
        }
        rows = if (reverse) sorted.reversed() else sorted
        if (selectedPid != null) rows.indexOfFirst { it.pid == selectedPid }.takeIf { it >= 0 }?.let { selected = it }
        clampSelection()
    }

    private fun clampSelection() {
        selected = selected.coerceIn(0, max(0, rows.size - 1))
        if (procListRows > 0) {
            if (selected < scroll) scroll = selected
            if (selected >= scroll + procListRows) scroll = selected - procListRows + 1
            scroll = scroll.coerceIn(0, max(0, rows.size - procListRows))
        }
    }

    override fun onDraw(canvas: Canvas) {
        val s = snap
        val w = width.toFloat()
        val h = height.toFloat()
        val pad = cw / 2
        if (s == null) {
            text.color = FG
            canvas.drawText("Reading /proc ...", pad, ch, text)
            return
        }
        val wide = w / cw >= 100
        val cpuRows = cpuBoxRows(s, h)
        val cpuBox = RectF(pad, pad, w - pad, pad + cpuRows * ch)
        drawCpu(canvas, cpuBox, s)
        val top = cpuBox.bottom + pad
        if (wide) {
            val left = RectF(pad, top, pad + (w - 2 * pad) * 0.42f, h - pad)
            val memBox = RectF(left.left, left.top, left.right, left.top + (left.height() * 0.58f))
            drawMem(canvas, memBox, s)
            drawNet(canvas, RectF(left.left, memBox.bottom + pad, left.right, left.bottom), s)
            drawProcs(canvas, RectF(left.right + pad, top, w - pad, h - pad))
        } else {
            // Disks beside the meters from 60 characters inside the box, else below them.
            val memRows = if ((w - 2 * pad) / cw - 2 >= 60) 9f else 11f + s.disks.size
            val memBox = RectF(pad, top, w - pad, top + min(memRows, (h - top) / ch / 2) * ch)
            drawMem(canvas, memBox, s)
            drawProcs(canvas, RectF(pad, memBox.bottom + pad, w - pad, h - pad))
        }
    }

    private fun cpuBoxRows(s: Snapshot, h: Float): Int {
        val maxRows = max(6, (h / ch * 0.36f).toInt())
        return min(maxRows, s.cores.size + 3).coerceAtLeast(6)
    }

    // ---- boxes -----------------------------------------------------------------------------

    private fun box(canvas: Canvas, r: RectF, title: String, color: Int, footer: String? = null) {
        stroke.color = color
        val inset = RectF(r.left, r.top + ch / 2, r.right, r.bottom - ch / 2)
        canvas.drawRoundRect(inset, cw * 0.6f, cw * 0.6f, stroke)
        label(canvas, r.left + cw * 2, r.top, title, color, TITLE)
        footer?.let { label(canvas, r.left + cw * 2, r.bottom - ch, it, color, GRAPH_TEXT) }
    }

    /** A ┤title├-style label cut into a box border at row [y]. */
    private fun label(canvas: Canvas, x: Float, y: Float, s: String, border: Int, color: Int) {
        val tw = text.measureText(s)
        fill.color = BG
        canvas.drawRect(x - cw / 2, y, x + tw + cw / 2, y + ch, fill)
        stroke.color = border
        text.color = color
        canvas.drawText(s, x, baseline(y), text)
    }

    private fun baseline(rowTop: Float) = rowTop + ch - text.descent()

    private fun drawCpu(canvas: Canvas, r: RectF, s: Snapshot) {
        val uptime = "%dd %02d:%02d".format(s.uptimeSec / 86400, s.uptimeSec / 3600 % 24, s.uptimeSec / 60 % 60)
        box(canvas, r, "cpu", CPU_BOX, "up $uptime   load ${s.loadAvg}")
        val inner = RectF(r.left + cw, r.top + ch, r.right - cw, r.bottom - ch)
        val usableRows = (inner.height() / ch).toInt()
        val coreCols = max(1, ceil(s.cores.size / usableRows.coerceAtLeast(1).toFloat()).toInt())
        val colChars = 25
        val coresWidth = min(inner.width() * 0.6f, coreCols * colChars * cw + cw)
        val graph = RectF(inner.left, inner.top, inner.right - coresWidth - cw, inner.bottom)
        drawGraph(canvas, graph, history, 1f)
        text.color = TITLE
        canvas.drawText("Total %3d%%".format((s.total * 100).roundToInt()), graph.left + cw, baseline(graph.top), text)

        // Per-core meters, in columns when the box is shorter than the core count.
        val coresBox = RectF(inner.right - coresWidth, inner.top, inner.right, inner.bottom)
        stroke.color = DIV
        canvas.drawRoundRect(coresBox, cw * 0.6f, cw * 0.6f, stroke)
        val perCol = ceil(s.cores.size / coreCols.toFloat()).toInt().coerceAtLeast(1)
        s.cores.forEachIndexed { i, v ->
            val x = coresBox.left + cw + (i / perCol) * colChars * cw
            val y = coresBox.top + (i % perCol) * ch + (coresBox.height() - perCol * ch) / 2
            text.color = FG
            canvas.drawText("C%-2d".format(i), x, baseline(y), text)
            meter(canvas, x + 4 * cw, y, 10 * cw, v)
            text.color = gradient(v)
            canvas.drawText("%3d%%".format((v * 100).roundToInt()), x + 15 * cw, baseline(y), text)
            if (s.freqMhz[i] > 0 && colChars >= 25) {
                text.color = GRAPH_TEXT
                canvas.drawText("%4.1f".format(s.freqMhz[i] / 1000f), x + 20 * cw, baseline(y), text)
            }
        }
        s.tempC?.let {
            label(canvas, coresBox.right - 8 * cw, coresBox.top - ch / 2, "%d°C".format(it), DIV,
                gradient((it - 30) / 70f))
        }
    }

    /**
     * The mem box: memory meters on the left and, like btop, the disks in their own bordered
     * column on the right; below the meters when the box is too narrow for two columns.
     */
    private fun drawMem(canvas: Canvas, r: RectF, s: Snapshot) {
        box(canvas, r, "mem", MEM_BOX)
        val inner = RectF(r.left + cw, r.top + ch, r.right - cw, r.bottom - ch)
        val memRows = if (s.swapTotalKb > 0) 6 else 5
        val side = inner.width() / cw >= 60
        val disks = if (side) {
            RectF(inner.left + inner.width() * 0.56f, inner.top + ch / 2, inner.right, inner.bottom)
        } else {
            val top = inner.top + memRows * ch + ch / 2
            if (inner.bottom - top >= 2 * ch) RectF(inner.left, top, inner.right, inner.bottom) else null
        }
        val meters = if (side) RectF(inner.left, inner.top, disks!!.left - cw, inner.bottom) else inner
        drawMemMeters(canvas, meters, s)
        disks?.let { drawDisks(canvas, it, s.disks) }
    }

    private fun drawMemMeters(canvas: Canvas, r: RectF, s: Snapshot) {
        var y = r.top
        val x = r.left + cw
        val chars = (r.width() / cw).toInt() - 1
        // "Available:   22.8G" when there is room, "Avail: 22.8G" otherwise.
        val long = chars - 19 - 5 >= 8
        val labelChars = if (long) 19 else 13
        val barW = max(3 * cw, (chars - labelChars - 6) * cw)
        fun row(label: String, short: String, kb: Long, of: Long, color: Int) {
            if (y + ch > r.bottom) return
            val share = if (of > 0) kb.toFloat() / of else 0f
            text.color = FG
            val l = if (long) "%-10s%9s".format(label, bytes(kb * 1024)) else "%-6s%7s".format(short, bytes(kb * 1024))
            canvas.drawText(l, x, baseline(y), text)
            meter(canvas, x + (labelChars + 1) * cw, y, barW, share, color)
            canvas.drawText("%3d%%".format((share * 100).roundToInt()), x + (labelChars + 2) * cw + barW, baseline(y), text)
            y += ch
        }
        text.color = TITLE
        canvas.drawText((if (long) "Total:%13s" else "Total:%7s").format(bytes(s.memTotalKb * 1024)), x, baseline(y), text)
        y += ch
        row("Used:", "Used:", s.memTotalKb - s.memAvailKb, s.memTotalKb, USED)
        row("Available:", "Avail:", s.memAvailKb, s.memTotalKb, AVAILABLE)
        row("Cached:", "Cache:", s.cachedKb, s.memTotalKb, CACHED)
        row("Free:", "Free:", s.memFreeKb, s.memTotalKb, FREE)
        if (s.swapTotalKb > 0) row("Swap:", "Swap:", s.swapTotalKb - s.swapFreeKb, s.swapTotalKb, USED)
    }

    /** The disks column: a bordered sub-box, two rows per disk (name and sizes, then a meter). */
    private fun drawDisks(canvas: Canvas, r: RectF, disks: List<Disk>) {
        stroke.color = DIV
        canvas.drawRoundRect(r, cw * 0.6f, cw * 0.6f, stroke)
        label(canvas, r.left + cw * 2, r.top - ch / 2, "disks", DIV, TITLE)
        val x = r.left + cw
        val right = r.right - cw
        val rows = ((r.height() - ch / 2) / ch).toInt()
        val twoRows = rows >= disks.size * 2
        var y = r.top + ch / 2
        for (d in disks) {
            if (y + ch > r.bottom) break
            val used = d.totalBytes - d.freeBytes
            val share = if (d.totalBytes > 0) used.toFloat() / d.totalBytes else 0f
            val pct = "%3d%%".format((share * 100).roundToInt())
            text.color = TITLE
            canvas.drawText(d.name, x, baseline(y), text)
            if (twoRows) {
                val sizes = "${bytes(used)}/${bytes(d.totalBytes)}"
                text.color = GRAPH_TEXT
                canvas.drawText(sizes, right - text.measureText(sizes), baseline(y), text)
                y += ch
                if (y + ch > r.bottom) break
            }
            val meterX = if (twoRows) x else x + 7 * cw
            val meterW = right - meterX - 5 * cw
            if (meterW > 2 * cw) meter(canvas, meterX, y, meterW, share, USED)
            text.color = FG
            canvas.drawText(pct, right - text.measureText(pct), baseline(y), text)
            y += ch
        }
    }

    private fun drawNet(canvas: Canvas, r: RectF, s: Snapshot) {
        if (r.height() < 4 * ch) return
        box(canvas, r, "net", NET_BOX)
        val inner = RectF(r.left + cw, r.top + ch, r.right - cw, r.bottom - ch)
        val half = inner.height() / 2
        // Auto-scaled like btop, but a few bytes a second do not fill the box.
        val peak = max(NET_SCALE_FLOOR, max(rxHistory.max(), txHistory.max())).toFloat()
        drawGraph(canvas, RectF(inner.left, inner.top, inner.right, inner.top + half),
            FloatArray(HISTORY) { rxHistory[it] / peak }, 1f, DOWNLOAD)
        drawGraph(canvas, RectF(inner.left, inner.top + half, inner.right, inner.bottom),
            FloatArray(HISTORY) { txHistory[it] / peak }, 1f, UPLOAD, flip = true)
        text.color = TITLE
        canvas.drawText("▼ ${bytes(s.rxBytesPerSec)}/s  total ${bytes(s.rxTotal)}", inner.left + cw, baseline(inner.top), text)
        canvas.drawText("▲ ${bytes(s.txBytesPerSec)}/s  total ${bytes(s.txTotal)}", inner.left + cw,
            baseline(inner.bottom - ch), text)
    }

    private fun drawProcs(canvas: Canvas, r: RectF) {
        val arrow = if (reverse) "↑" else "↓"
        val footer = status ?: "${selected + 1}/${rows.size}  c m p n sort  r reverse  f force stop  q quit"
        box(canvas, r, "proc  sort:${sort.name.lowercase()}$arrow", PROC_BOX, footer)
        val x = r.left + cw * 2
        val chars = ((r.width() - 4 * cw) / cw).toInt()
        val nameChars = (chars - 45).coerceIn(8, 60)
        // Column starts in characters: Pid, Program, User, Thr, Mem, Cpu%, meter.
        val cols = intArrayOf(0, 8, 9 + nameChars, 19 + nameChars, 24 + nameChars, 33 + nameChars, 40 + nameChars)
        columnStarts = FloatArray(cols.size) { x + cols[it] * cw }
        procHeaderY = r.top + ch
        text.color = TITLE
        val headers = arrayOf("Pid:", "Program:", "User:", "Thr:", "MemB", "Cpu%")
        headers.forEachIndexed { i, hdr ->
            val active = (i == 0 && sort == Sort.PID) || (i == 1 && sort == Sort.NAME) ||
                (i == 4 && sort == Sort.MEM) || (i == 5 && sort == Sort.CPU)
            text.color = if (active) HI else TITLE
            canvas.drawText(hdr, columnStarts[i], baseline(procHeaderY), text)
        }
        procListTop = procHeaderY + ch
        procListRows = max(0, ((r.bottom - ch * 1.5f - procListTop) / ch).toInt())
        clampSelection()
        for (i in 0 until procListRows) {
            val p = rows.getOrNull(scroll + i) ?: break
            val y = procListTop + i * ch
            if (scroll + i == selected) {
                fill.color = SELECTED_BG
                canvas.drawRect(r.left + cw, y, r.right - cw, y + ch, fill)
            }
            text.color = if (scroll + i == selected) TITLE else FG
            canvas.drawText("%7d".format(p.pid), columnStarts[0], baseline(y), text)
            canvas.drawText(p.name.take(nameChars), columnStarts[1], baseline(y), text)
            text.color = if (scroll + i == selected) TITLE else GRAPH_TEXT
            canvas.drawText(userName(p.uid).take(9), columnStarts[2], baseline(y), text)
            text.color = if (scroll + i == selected) TITLE else FG
            canvas.drawText("%4d".format(p.threads), columnStarts[3], baseline(y), text)
            canvas.drawText("%8s".format(bytes(p.rssKb * 1024)), columnStarts[4], baseline(y), text)
            text.color = gradient(p.cpu / 100f)
            canvas.drawText("%5.1f".format(p.cpu), columnStarts[5], baseline(y), text)
            val meterW = r.right - 2 * cw - columnStarts[6]
            if (meterW > 3 * cw) meter(canvas, columnStarts[6], y, min(meterW, 10 * cw), p.cpu / 100f)
        }
    }

    // ---- primitives ------------------------------------------------------------------------

    /** A meter of [w] pixels filled by [v] (0..1), green to red unless [color] is given. */
    private fun meter(canvas: Canvas, x: Float, rowTop: Float, w: Float, v: Float, color: Int? = null) {
        val top = rowTop + ch * 0.25f
        val bottom = rowTop + ch * 0.8f
        fill.color = METER_BG
        canvas.drawRect(x, top, x + w, bottom, fill)
        val cells = (w / (cw / 2)).toInt()
        val lit = (v.coerceIn(0f, 1f) * cells).roundToInt()
        for (i in 0 until lit) {
            fill.color = color ?: gradient(i / cells.toFloat())
            val cx = x + i * cw / 2
            canvas.drawRect(cx, top, cx + cw / 2 - stroke.strokeWidth, bottom, fill)
        }
    }

    /** Vertical bars, newest on the right, one per half cell; [color] or a gradient by height. */
    private fun drawGraph(canvas: Canvas, r: RectF, values: FloatArray, max: Float, color: Int? = null,
            flip: Boolean = false) {
        val barW = cw / 2
        val bars = min((r.width() / barW).toInt(), min(samples, HISTORY))
        for (i in 0 until bars) {
            val v = (values[HISTORY - 1 - i] / max).coerceIn(0f, 1f)
            if (v <= 0f) continue
            val x = r.right - (i + 1) * barW
            val steps = (v * r.height() / (ch / 4)).roundToInt().coerceAtLeast(1)
            for (k in 0 until steps) {
                fill.color = color ?: gradient(k * (ch / 4) / r.height())
                val y0 = if (flip) r.top + k * ch / 4 else r.bottom - (k + 1) * ch / 4
                canvas.drawRect(x, y0, x + barW - stroke.strokeWidth, y0 + ch / 4 - stroke.strokeWidth, fill)
            }
        }
    }

    // ---- input -----------------------------------------------------------------------------

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        val page = max(1, procListRows - 1)
        when (keyCode) {
            KeyEvent.KEYCODE_DPAD_UP, KeyEvent.KEYCODE_K -> selected--
            KeyEvent.KEYCODE_DPAD_DOWN, KeyEvent.KEYCODE_J -> selected++
            KeyEvent.KEYCODE_PAGE_UP -> selected -= page
            KeyEvent.KEYCODE_PAGE_DOWN -> selected += page
            KeyEvent.KEYCODE_MOVE_HOME -> selected = 0
            KeyEvent.KEYCODE_MOVE_END -> selected = rows.size - 1
            KeyEvent.KEYCODE_C -> setSort(Sort.CPU)
            KeyEvent.KEYCODE_M -> setSort(Sort.MEM)
            KeyEvent.KEYCODE_P -> setSort(Sort.PID)
            KeyEvent.KEYCODE_N -> setSort(Sort.NAME)
            KeyEvent.KEYCODE_R -> { reverse = !reverse; resort() }
            KeyEvent.KEYCODE_F -> forceStopSelected()
            KeyEvent.KEYCODE_Q, KeyEvent.KEYCODE_ESCAPE -> onQuit?.invoke()
            else -> return super.onKeyDown(keyCode, event)
        }
        status = null
        clampSelection()
        invalidate()
        return true
    }

    private fun setSort(s: Sort) {
        if (sort == s) reverse = !reverse else { sort = s; reverse = false }
        resort()
    }

    private fun forceStopSelected() {
        val p = rows.getOrNull(selected) ?: return
        if (p.uid % 100000 < 10000) {
            status = "${p.name} is not an app"
            return
        }
        onForceStop?.invoke(p.name.substringBefore(':'))
    }

    override fun onGenericMotionEvent(event: MotionEvent): Boolean {
        if (event.action == MotionEvent.ACTION_SCROLL) {
            val v = event.getAxisValue(MotionEvent.AXIS_VSCROLL)
            selected -= (v * 3).roundToInt()
            clampSelection()
            invalidate()
            return true
        }
        return super.onGenericMotionEvent(event)
    }

    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (event.action != MotionEvent.ACTION_UP) return true
        requestFocus()
        if (columnStarts.isEmpty() || event.x < columnStarts[0] - cw) return true
        if (event.y >= procHeaderY && event.y < procHeaderY + ch) {
            val col = columnStarts.indexOfLast { event.x >= it - cw / 2 }
            when (col) {
                0 -> setSort(Sort.PID)
                1 -> setSort(Sort.NAME)
                4 -> setSort(Sort.MEM)
                5, 6 -> setSort(Sort.CPU)
            }
        } else if (event.y >= procListTop) {
            val row = ((event.y - procListTop) / ch).toInt()
            if (row < procListRows) selected = scroll + row
        }
        status = null
        clampSelection()
        invalidate()
        return true
    }

    companion object {
        private const val HISTORY = 600
        private const val NET_SCALE_FLOOR = 10L * 1024

        // btop's "Default" theme.
        private val BG = Color.rgb(0x0e, 0x0f, 0x12)
        private val FG = Color.rgb(0xcc, 0xcc, 0xcc)
        private val TITLE = Color.rgb(0xee, 0xee, 0xee)
        private val HI = Color.rgb(0xb5, 0x40, 0x40)
        private val SELECTED_BG = Color.rgb(0x6a, 0x2f, 0x2f)
        private val GRAPH_TEXT = Color.rgb(0x80, 0x80, 0x80)
        private val DIV = Color.rgb(0x30, 0x30, 0x30)
        private val METER_BG = Color.rgb(0x1c, 0x1c, 0x1c)
        private val CPU_BOX = Color.rgb(0x3d, 0x7b, 0x46)
        private val MEM_BOX = Color.rgb(0x8a, 0x88, 0x2e)
        private val NET_BOX = Color.rgb(0x42, 0x3b, 0xa5)
        private val PROC_BOX = Color.rgb(0x92, 0x35, 0x35)
        private val USED = Color.rgb(0xdf, 0x5d, 0x5d)
        private val AVAILABLE = Color.rgb(0xd6, 0xb9, 0x4a)
        private val CACHED = Color.rgb(0x4a, 0x9a, 0xd6)
        private val FREE = Color.rgb(0x77, 0xca, 0x9b)
        private val DOWNLOAD = Color.rgb(0x8a, 0x6e, 0xd6)
        private val UPLOAD = Color.rgb(0xd6, 0x5e, 0x9a)
        private val START = Color.rgb(0x77, 0xca, 0x9b)
        private val MID = Color.rgb(0xcb, 0xc0, 0x6c)
        private val END = Color.rgb(0xdc, 0x4c, 0x4c)

        private fun mix(a: Int, b: Int, t: Float): Int = Color.rgb(
            (Color.red(a) + (Color.red(b) - Color.red(a)) * t).roundToInt(),
            (Color.green(a) + (Color.green(b) - Color.green(a)) * t).roundToInt(),
            (Color.blue(a) + (Color.blue(b) - Color.blue(a)) * t).roundToInt())

        /** btop's cpu gradient: green, yellow, red. */
        fun gradient(v: Float): Int {
            val t = v.coerceIn(0f, 1f)
            return if (t < 0.5f) mix(START, MID, t * 2) else mix(MID, END, (t - 0.5f) * 2)
        }

        fun bytes(b: Long): String {
            val units = arrayOf("B", "K", "M", "G", "T")
            var v = b.toDouble()
            var u = 0
            while (v >= 1024 && u < units.size - 1) { v /= 1024; u++ }
            return if (u == 0) "${b}B" else if (v >= 100) "%.0f%s".format(v, units[u]) else "%.1f%s".format(v, units[u])
        }

        private val AIDS = mapOf(0 to "root", 1000 to "system", 1001 to "radio", 1002 to "bluetooth",
            1003 to "graphics", 1004 to "input", 1005 to "audio", 1010 to "wifi", 1013 to "media",
            1017 to "keystore", 1019 to "drm", 1021 to "gps", 1036 to "logd", 1041 to "audioserver",
            1046 to "mediacodec", 1047 to "cameraserver", 1066 to "statsd", 1068 to "secure_elem",
            1069 to "lmkd", 1072 to "credstore", 1073 to "ext_data_rw", 2000 to "shell", 9999 to "nobody")

        fun userName(uid: Int): String {
            if (uid < 0) return "?"
            val user = uid / 100000
            val app = uid % 100000
            return when {
                app >= 90000 -> "u${user}_i${app - 90000}"
                app >= 10000 -> "u${user}_a${app - 10000}"
                else -> AIDS[app]?.let { if (user == 0) it else "u${user}_$it" } ?: uid.toString()
            }
        }
    }
}
