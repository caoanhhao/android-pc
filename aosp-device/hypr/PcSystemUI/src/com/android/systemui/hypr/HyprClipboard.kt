package com.android.systemui.hypr

import android.content.BroadcastReceiver
import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.graphics.Color
import android.graphics.PixelFormat
import android.graphics.drawable.GradientDrawable
import android.os.Handler
import android.util.Log
import android.util.TypedValue
import android.view.Gravity
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.view.WindowManager
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import com.android.hyprtile.ClipHistory

/**
 * Clipboard history (Phase E9, the host's `SUPER + SHIFT + V`): SystemUI watches the primary
 * clip (it holds READ_CLIPBOARD_IN_BACKGROUND) and keeps the last text clips in a [ClipHistory],
 * in memory only. Sensitive clips (passwords) are never kept; switching users clears it; nothing
 * is written to storage, so a reboot or a SystemUI restart forgets it (docs/hypr-desktop.md,
 * "System actions (E9)").
 *
 * The picker is a bar-styled, focused system window at the top of the screen: type to filter,
 * Up/Down to choose, Enter (or a click) puts the entry back on the clipboard and pastes it into
 * the focused app (CTRL + V), SHIFT + Enter only copies it, Delete removes it, CTRL + Delete
 * clears the history, Escape (or a touch outside) closes. Main thread only.
 */
class HyprClipboard(
    private val context: Context,
    private val main: Handler,
    private val injectCtrl: (keyCode: Int) -> Unit,
) {
    private val cm = context.getSystemService(ClipboardManager::class.java)
    private val wm = context.getSystemService(WindowManager::class.java)
    private val history = ClipHistory(capacity = CAPACITY)
    private var picker: View? = null
    private var query = ""
    private var selected = 0
    private var shown: List<String> = emptyList()
    private var copyWaiters = ArrayList<(String?) -> Unit>()

    fun start() {
        cm.addPrimaryClipChangedListener { main.post(::onClipChanged) }
        context.registerReceiver(object : BroadcastReceiver() {
            override fun onReceive(c: Context, i: Intent) {
                history.clear()
                dismiss()
            }
        }, IntentFilter(Intent.ACTION_USER_SWITCHED), null, main, Context.RECEIVER_NOT_EXPORTED)
        onClipChanged()
    }

    private fun onClipChanged() {
        val clip = try {
            cm.primaryClip
        } catch (e: RuntimeException) {
            Log.w(TAG, "primary clip: $e")
            null
        }
        val text = clip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.text?.toString()
        val sensitive = isSensitive(clip?.description)
        history.add(text, sensitive)
        val waiters = copyWaiters
        if (waiters.isNotEmpty()) {
            copyWaiters = ArrayList()
            waiters.forEach { it(if (sensitive) null else text) }
        }
    }

    private fun isSensitive(d: ClipDescription?): Boolean =
        d?.extras?.getBoolean(ClipDescription.EXTRA_IS_SENSITIVE) == true

    /** The clipboard's text now, unless it is sensitive. */
    private fun currentText(): String? = try {
        cm.primaryClip?.takeIf { it.itemCount > 0 && !isSensitive(it.description) }
            ?.getItemAt(0)?.text?.toString()
    } catch (e: RuntimeException) {
        null
    }

    /**
     * Copies the focused app's selection (CTRL + C) and calls [then] with the new clip's text,
     * or with the clipboard's current text when no new clip comes within [waitMs].
     */
    fun afterCopy(waitMs: Long, then: (String?) -> Unit) {
        var done = false
        val waiter: (String?) -> Unit = { text ->
            if (!done) {
                done = true
                then(text)
            }
        }
        copyWaiters.add(waiter)
        injectCtrl(KeyEvent.KEYCODE_C)
        main.postDelayed({
            copyWaiters.remove(waiter)
            waiter(currentText())
        }, waitMs)
    }

    fun togglePicker() {
        if (picker != null) dismiss() else show()
    }

    private fun show() {
        query = ""
        selected = 0
        val root = object : LinearLayout(context) {
            override fun dispatchKeyEvent(event: KeyEvent): Boolean =
                onKey(event) || super.dispatchKeyEvent(event)
        }.apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(8), dp(8), dp(8), dp(8))
            background = GradientDrawable().apply {
                cornerRadius = dp(12).toFloat()
                setColor(BACKGROUND)
                setStroke(dp(1), ACCENT)
            }
            elevation = dp(8).toFloat()
            isFocusable = true
            isFocusableInTouchMode = true
        }
        root.setOnTouchListener { _, ev ->
            if (ev.actionMasked == MotionEvent.ACTION_OUTSIDE) dismiss()
            false
        }
        val lp = WindowManager.LayoutParams(
            dp(WIDTH_DP), WRAP_CONTENT,
            WindowManager.LayoutParams.TYPE_SYSTEM_DIALOG,
            WindowManager.LayoutParams.FLAG_WATCH_OUTSIDE_TOUCH or
                WindowManager.LayoutParams.FLAG_NOT_TOUCH_MODAL or
                WindowManager.LayoutParams.FLAG_ALT_FOCUSABLE_IM,
            PixelFormat.TRANSLUCENT,
        ).apply {
            gravity = Gravity.TOP or Gravity.CENTER_HORIZONTAL
            y = dp(64)
            title = "HyprClipboard"
        }
        try {
            wm.addView(root, lp)
            picker = root
            render()
            root.requestFocus()
        } catch (e: RuntimeException) {
            Log.e(TAG, "picker failed", e)
        }
    }

    fun dismiss() {
        val v = picker ?: return
        picker = null
        try {
            wm.removeViewImmediate(v)
        } catch (e: RuntimeException) {
            Log.w(TAG, "picker: $e")
        }
    }

    private fun onKey(e: KeyEvent): Boolean {
        if (e.action != KeyEvent.ACTION_DOWN) {
            return e.keyCode in HANDLED_KEYS
        }
        when (e.keyCode) {
            KeyEvent.KEYCODE_ESCAPE -> dismiss()
            KeyEvent.KEYCODE_DPAD_DOWN, KeyEvent.KEYCODE_TAB -> move(1)
            KeyEvent.KEYCODE_DPAD_UP -> move(-1)
            KeyEvent.KEYCODE_PAGE_DOWN -> move(VISIBLE_ROWS)
            KeyEvent.KEYCODE_PAGE_UP -> move(-VISIBLE_ROWS)
            KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_NUMPAD_ENTER ->
                shown.getOrNull(selected)?.let { pick(it, paste = !e.isShiftPressed) }
            KeyEvent.KEYCODE_FORWARD_DEL -> {
                if (e.isCtrlPressed) history.clear()
                else shown.getOrNull(selected)?.let { history.remove(it) }
                render()
            }
            KeyEvent.KEYCODE_DEL -> {
                if (query.isNotEmpty()) {
                    query = query.dropLast(1)
                    selected = 0
                    render()
                }
            }
            else -> {
                val c = e.unicodeChar
                if (c == 0 || e.isCtrlPressed || e.isAltPressed || e.isMetaPressed) return false
                query += c.toChar()
                selected = 0
                render()
            }
        }
        return true
    }

    private fun move(by: Int) {
        if (shown.isEmpty()) return
        selected = (selected + by).coerceIn(0, shown.size - 1)
        render()
    }

    /** Puts [text] back on the clipboard; with [paste], closes and pastes it into the focused app. */
    private fun pick(text: String, paste: Boolean) {
        cm.setPrimaryClip(ClipData.newPlainText("clipboard history", text))
        dismiss()
        if (paste) main.postDelayed({ injectCtrl(KeyEvent.KEYCODE_V) }, PASTE_DELAY_MS)
    }

    private fun render() {
        val root = picker as? LinearLayout ?: return
        root.removeAllViews()
        shown = history.filter(query)
        selected = selected.coerceIn(0, maxOf(0, shown.size - 1))
        root.addView(TextView(context).apply {
            text = if (query.isEmpty()) "Clipboard" else "Clipboard  ›  $query"
            setTextColor(FOREGROUND)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
            setPadding(dp(12), dp(4), dp(12), dp(6))
        })
        val list = LinearLayout(context).apply { orientation = LinearLayout.VERTICAL }
        if (shown.isEmpty()) {
            list.addView(row(if (history.items.isEmpty()) "Nothing copied yet" else "No match",
                dim = true, selected = false) {})
        }
        shown.forEachIndexed { i, text ->
            list.addView(row(ClipHistory.preview(text), dim = false, selected = i == selected) {
                pick(text, paste = true)
            })
        }
        val scroll = ScrollView(context).apply { addView(list) }
        val rows = minOf(VISIBLE_ROWS, maxOf(1, shown.size))
        root.addView(scroll, LinearLayout.LayoutParams(MATCH_PARENT, dp(ROW_DP) * rows))
        scroll.post { scroll.scrollTo(0, maxOf(0, (selected - rows + 1) * dp(ROW_DP))) }
        root.addView(TextView(context).apply {
            text = "Enter paste · Shift+Enter copy · Del remove · Ctrl+Del clear · Esc close"
            setTextColor(DIM)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
            setPadding(dp(12), dp(6), dp(12), dp(2))
        })
    }

    private fun row(label: String, dim: Boolean, selected: Boolean, onClick: () -> Unit) =
        TextView(context).apply {
            text = label
            maxLines = 1
            setTextColor(if (selected) BACKGROUND else if (dim) DIM else FOREGROUND)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(12), 0, dp(12), 0)
            layoutParams = LinearLayout.LayoutParams(MATCH_PARENT, dp(ROW_DP))
            background = GradientDrawable().apply {
                cornerRadius = dp(8).toFloat()
                setColor(if (selected) ACCENT else Color.TRANSPARENT)
            }
            isClickable = !dim
            setOnClickListener { onClick() }
        }

    private fun dp(v: Int) = (v * context.resources.displayMetrics.density + 0.5f).toInt()

    private companion object {
        const val TAG = "HyprClipboard"
        const val CAPACITY = 25
        const val WIDTH_DP = 640
        const val ROW_DP = 36
        const val VISIBLE_ROWS = 10
        const val PASTE_DELAY_MS = 150L
        val HANDLED_KEYS = setOf(KeyEvent.KEYCODE_ESCAPE, KeyEvent.KEYCODE_ENTER,
            KeyEvent.KEYCODE_NUMPAD_ENTER, KeyEvent.KEYCODE_DPAD_UP, KeyEvent.KEYCODE_DPAD_DOWN)
        // The bar's colours (HyprLauncher's Bar, HyprWindowMenu).
        val BACKGROUND = Color.parseColor("#E61E1E2E")
        val FOREGROUND = Color.parseColor("#CDD6F4")
        val DIM = Color.parseColor("#7F849C")
        val ACCENT = Color.parseColor("#33CCFF")
    }
}
