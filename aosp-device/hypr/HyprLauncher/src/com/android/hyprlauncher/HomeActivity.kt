package com.android.hyprlauncher

import android.app.Activity
import android.content.Intent
import android.graphics.Color
import android.os.Bundle
import android.view.Gravity
import android.widget.FrameLayout
import android.widget.TextView
import android.window.OnBackInvokedDispatcher

/**
 * The HOME activity (Phase D4): the wallpaper, as an empty Hyprland workspace, with a hint of
 * the main keys. The bar, launcher and overview are windows of their own ([Desktop]), so they
 * stay over every workspace.
 */
class HomeActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Desktop.get(this).start()
        HomeRole.ensure(this)
        val hint = TextView(this)
        hint.text = "ALT+D apps    ALT+1..9 workspaces    ALT+` overview    ALT+C close"
        hint.setTextColor(Color.parseColor("#99CDD6F4"))
        hint.textSize = 12f
        hint.setShadowLayer(4f, 0f, 1f, Color.BLACK)
        val root = FrameLayout(this)
        root.addView(hint, FrameLayout.LayoutParams(
            FrameLayout.LayoutParams.WRAP_CONTENT, FrameLayout.LayoutParams.WRAP_CONTENT,
            Gravity.BOTTOM or Gravity.CENTER_HORIZONTAL).apply { bottomMargin = 24 })
        root.setOnClickListener { Desktop.get(this).closePanels() }
        setContentView(root)
        // Back on the desktop does nothing (a launcher's home never finishes).
        onBackInvokedDispatcher.registerOnBackInvokedCallback(
            OnBackInvokedDispatcher.PRIORITY_DEFAULT) {}
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        // HOME again: close the launcher and the overview, as Launcher3 closes its panels.
        Desktop.get(this).closePanels()
    }
}
