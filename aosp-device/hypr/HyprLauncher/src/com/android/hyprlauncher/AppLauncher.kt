package com.android.hyprlauncher

import android.content.Context
import android.graphics.Color
import android.graphics.PixelFormat
import android.graphics.Rect
import android.graphics.drawable.GradientDrawable
import android.text.Editable
import android.text.TextWatcher
import android.view.Gravity
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.WindowManager
import android.view.WindowManager.LayoutParams.FLAG_DIM_BEHIND
import android.view.WindowManager.LayoutParams.FLAG_WATCH_OUTSIDE_TOUCH
import android.view.WindowManager.LayoutParams.SOFT_INPUT_ADJUST_NOTHING
import android.view.WindowManager.LayoutParams.TYPE_SYSTEM_DIALOG
import android.view.inputmethod.EditorInfo
import android.widget.BaseAdapter
import android.widget.EditText
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ListView
import android.widget.TextView
import com.android.hyprtile.fuzzyFilter

/**
 * The rofi-like app launcher (Phase D4): a centred box with a search field and the matching
 * apps, fuzzy-matched (hyprtile's [fuzzyFilter]). Keyboard driven: type to filter, Up/Down
 * (or Tab) to move, Enter to start, Esc to close; a click on a row starts it, a click outside
 * or losing focus closes the box. Bound to MOD+D and MOD+Space (the `launcher` dispatcher)
 * and to the bar's Apps button. The app opens on the active workspace, where hyprtile tiles it.
 */
class AppLauncher(private val desktop: Desktop) {
    private val ctx: Context = desktop.context.createDisplayContext(desktop.display)
        .createWindowContext(TYPE_SYSTEM_DIALOG, null)
    private val wm = ctx.getSystemService(WindowManager::class.java)
    private val root = object : LinearLayout(ctx) {
        override fun dispatchKeyEvent(event: KeyEvent): Boolean =
            onKey(event) || super.dispatchKeyEvent(event)
    }
    private val search = EditText(ctx)
    private val list = ListView(ctx)
    private val adapter = Adapter()
    private var matches: List<App> = emptyList()
    private var selected = 0
    private var shown = false
    private var hadFocus = false

    val isShown get() = shown

    init {
        root.orientation = LinearLayout.VERTICAL
        root.background = GradientDrawable().apply {
            cornerRadius = desktop.dp(12f).toFloat()
            setColor(Color.parseColor("#F21E1E2E"))
            setStroke(desktop.dp(2f), Bar.ACCENT)
        }
        val pad = desktop.dp(10f)
        root.setPadding(pad, pad, pad, pad)
        // The shell's touch gestures (D6) leave the launcher's touches alone.
        root.addOnLayoutChangeListener { v, _, _, _, _, _, _, _, _ ->
            v.systemGestureExclusionRects = listOf(Rect(0, 0, v.width, v.height))
        }

        search.hint = "Run…"
        search.setHintTextColor(Bar.DIM)
        search.setTextColor(Bar.FG)
        search.textSize = 16f
        search.isSingleLine = true
        search.imeOptions = EditorInfo.IME_ACTION_GO
        search.background = GradientDrawable().apply {
            cornerRadius = desktop.dp(8f).toFloat()
            setColor(Color.parseColor("#313244"))
        }
        search.setPadding(pad, pad / 2, pad, pad / 2)
        search.addTextChangedListener(object : TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
            override fun onTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
            override fun afterTextChanged(s: Editable?) = filter()
        })
        search.setOnEditorActionListener { _, _, _ -> launchSelected(); true }
        root.addView(search, LinearLayout.LayoutParams(MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))

        list.adapter = adapter
        list.divider = null
        list.isFocusable = false
        list.setOnItemClickListener { _, _, pos, _ -> selected = pos; launchSelected() }
        root.addView(list, LinearLayout.LayoutParams(MATCH_PARENT, 0, 1f).apply {
            topMargin = desktop.dp(8f)
        })

        root.setOnTouchListener { _, ev ->
            if (ev.action == MotionEvent.ACTION_OUTSIDE) hide()
            false
        }
        root.viewTreeObserver.addOnWindowFocusChangeListener { focused ->
            if (focused) hadFocus = true else if (hadFocus) hide()
        }
    }

    fun toggle() = if (shown) hide() else show()

    fun show() {
        if (shown) return
        desktop.overview.hide()
        shown = true
        hadFocus = false
        search.setText("")
        filter()
        val lp = WindowManager.LayoutParams(
            desktop.dp(560f), desktop.dp(440f), TYPE_SYSTEM_DIALOG,
            FLAG_WATCH_OUTSIDE_TOUCH or FLAG_DIM_BEHIND, PixelFormat.TRANSLUCENT,
        )
        lp.gravity = Gravity.CENTER
        lp.dimAmount = 0.25f
        lp.title = "HyprLauncher"
        lp.softInputMode = SOFT_INPUT_ADJUST_NOTHING
        lp.setFitInsetsTypes(0)
        wm.addView(root, lp)
        search.requestFocus()
    }

    fun hide() {
        if (!shown) return
        shown = false
        wm.removeViewImmediate(root)
    }

    private fun filter() {
        matches = fuzzyFilter(search.text.toString(), desktop.apps.apps) { it.label }
        selected = 0
        adapter.notifyDataSetChanged()
        list.setSelection(0)
    }

    private fun onKey(e: KeyEvent): Boolean {
        if (e.action != KeyEvent.ACTION_DOWN) {
            return e.keyCode == KeyEvent.KEYCODE_ESCAPE
        }
        when (e.keyCode) {
            KeyEvent.KEYCODE_ESCAPE -> hide()
            KeyEvent.KEYCODE_DPAD_DOWN -> move(1)
            KeyEvent.KEYCODE_DPAD_UP -> move(-1)
            KeyEvent.KEYCODE_TAB -> move(if (e.isShiftPressed) -1 else 1)
            KeyEvent.KEYCODE_PAGE_DOWN -> move(8)
            KeyEvent.KEYCODE_PAGE_UP -> move(-8)
            KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_NUMPAD_ENTER -> launchSelected()
            else -> return false
        }
        return true
    }

    private fun move(by: Int) {
        if (matches.isEmpty()) return
        selected = (selected + by).coerceIn(0, matches.size - 1)
        adapter.notifyDataSetChanged()
        list.smoothScrollToPosition(selected)
    }

    private fun launchSelected() {
        val app = matches.getOrNull(selected) ?: return
        hide()
        desktop.apps.launch(app)
    }

    private inner class Adapter : BaseAdapter() {
        override fun getCount() = matches.size
        override fun getItem(pos: Int) = matches[pos]
        override fun getItemId(pos: Int) = pos.toLong()

        override fun getView(pos: Int, convert: View?, parent: ViewGroup): View {
            val row = (convert as? LinearLayout) ?: LinearLayout(ctx).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = Gravity.CENTER_VERTICAL
                val p = desktop.dp(6f)
                setPadding(p, p, p, p)
                addView(ImageView(ctx), LinearLayout.LayoutParams(desktop.dp(28f), desktop.dp(28f)))
                addView(TextView(ctx).apply {
                    textSize = 15f
                    setPadding(desktop.dp(12f), 0, 0, 0)
                })
            }
            val app = matches[pos]
            (row.getChildAt(0) as ImageView).setImageDrawable(app.icon)
            (row.getChildAt(1) as TextView).apply {
                text = app.label
                setTextColor(if (pos == selected) Bar.BG_SOLID else Bar.FG)
            }
            row.background = if (pos == selected) {
                GradientDrawable().apply {
                    cornerRadius = desktop.dp(8f).toFloat()
                    setColor(Bar.ACCENT)
                }
            } else {
                null
            }
            return row
        }
    }
}
