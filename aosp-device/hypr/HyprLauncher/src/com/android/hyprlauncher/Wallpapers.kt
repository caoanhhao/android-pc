package com.android.hyprlauncher

import android.app.WallpaperManager
import android.util.Log
import java.io.File
import java.util.concurrent.Executors

/**
 * The `wallpaper random current|all [folder]` dispatcher (Phase E11), the host's
 * random-wallpaper script: a random picture of [DEFAULT_DIR] (or the given folder in shared
 * storage, which the launcher reads with all-files access) becomes the system and lock
 * wallpaper. HOME shows the system wallpaper on every workspace, so `current` and `all` do the
 * same (a wallpaper per workspace would mean a wallpaper change at every switch).
 */
class Wallpapers(private val desktop: Desktop) {
    private val worker = Executors.newSingleThreadExecutor()
    private var last: String? = null

    fun dispatch(arg: String) {
        val words = arg.trim().split(Regex("\\s+"))
        if (words.firstOrNull() != "random") {
            Log.w(TAG, "wallpaper: unknown argument '$arg'")
            return
        }
        val dir = File(words.getOrNull(2) ?: DEFAULT_DIR)
        worker.execute { setRandom(dir) }
    }

    private fun setRandom(dir: File) {
        val pictures = dir.listFiles { f -> f.isFile && f.extension.lowercase() in EXTENSIONS }
            ?.sortedBy { it.name }.orEmpty()
        if (pictures.isEmpty()) {
            Log.w(TAG, "wallpaper: no pictures in $dir")
            desktop.handler.post {
                desktop.notifier.notify("No pictures in $dir for a wallpaper", NOTE_MS, null)
            }
            return
        }
        val choice = (pictures.filter { it.path != last }.ifEmpty { pictures }).random()
        last = choice.path
        try {
            choice.inputStream().use {
                WallpaperManager.getInstance(desktop.context).setStream(it, null, true,
                    WallpaperManager.FLAG_SYSTEM or WallpaperManager.FLAG_LOCK)
            }
            Log.i(TAG, "wallpaper: $choice")
        } catch (e: Exception) {
            Log.w(TAG, "wallpaper: cannot set $choice: $e")
        }
    }

    private companion object {
        const val TAG = "HyprWallpapers"
        const val DEFAULT_DIR = "/sdcard/Pictures/Wallpapers"
        const val NOTE_MS = 4000L
        val EXTENSIONS = setOf("jpg", "jpeg", "png", "webp")
    }
}
