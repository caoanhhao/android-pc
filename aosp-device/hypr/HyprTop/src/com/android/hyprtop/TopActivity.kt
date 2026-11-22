package com.android.hyprtop

import android.app.Activity
import android.app.ActivityManager
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper

/** The monitor's only window: samples once a second on a background thread while visible. */
class TopActivity : Activity() {
    private lateinit var view: TopView
    private val sampler = Sampler()
    private val main = Handler(Looper.getMainLooper())
    private lateinit var worker: HandlerThread
    private lateinit var background: Handler
    private var running = false

    private val tick = object : Runnable {
        override fun run() {
            val s = sampler.sample()
            main.post { if (running) view.update(s) }
            if (running) background.postDelayed(this, INTERVAL_MS)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        worker = HandlerThread("HyprTop.sampler").apply { start() }
        background = Handler(worker.looper)
        view = TopView(this)
        view.onQuit = { finish() }
        view.onForceStop = { pkg ->
            background.post {
                val ok = runCatching { getSystemService(ActivityManager::class.java).forceStopPackage(pkg) }
                main.post { view.showStatus(if (ok.isSuccess) "force-stopped $pkg" else "cannot stop $pkg") }
            }
        }
        setContentView(view)
        view.requestFocus()
    }

    override fun onStart() {
        super.onStart()
        running = true
        background.removeCallbacks(tick)
        background.post(tick)
    }

    override fun onStop() {
        running = false
        background.removeCallbacks(tick)
        super.onStop()
    }

    override fun onDestroy() {
        worker.quitSafely()
        super.onDestroy()
    }

    companion object {
        private const val INTERVAL_MS = 1000L
    }
}
