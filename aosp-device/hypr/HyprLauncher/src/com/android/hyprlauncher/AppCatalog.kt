package com.android.hyprlauncher

import android.app.ActivityOptions
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.graphics.drawable.Drawable
import android.util.Log
import android.view.Display

/** An app with a launcher activity. */
class App(val component: ComponentName, val label: String, private val catalog: AppCatalog) {
    val icon: Drawable by lazy { catalog.iconOf(component) }
}

/** The apps the launcher offers, kept up to date on package changes. */
class AppCatalog(private val context: Context) {
    var apps: List<App> = emptyList()
        private set
    private val labels = HashMap<String, String>()
    private var registered = false

    fun load() {
        val pm = context.packageManager
        val intent = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
        apps = pm.queryIntentActivities(intent, 0)
            .filter { it.activityInfo.packageName != context.packageName }
            .map {
                val cn = ComponentName(it.activityInfo.packageName, it.activityInfo.name)
                App(cn, it.loadLabel(pm).toString(), this)
            }
            .sortedBy { it.label.lowercase() }
        labels.clear()
        if (!registered) {
            registered = true
            val filter = IntentFilter().apply {
                addAction(Intent.ACTION_PACKAGE_ADDED)
                addAction(Intent.ACTION_PACKAGE_REMOVED)
                addAction(Intent.ACTION_PACKAGE_CHANGED)
                addDataScheme("package")
            }
            val reload = object : BroadcastReceiver() {
                override fun onReceive(c: Context, i: Intent) = load()
            }
            context.registerReceiver(reload, filter, Context.RECEIVER_EXPORTED)
            // Before the user is unlocked only direct boot aware apps resolve.
            context.registerReceiver(reload, IntentFilter(Intent.ACTION_USER_UNLOCKED),
                Context.RECEIVER_EXPORTED)
        }
    }

    /** The app's label, for a window whose task gives no title. */
    fun label(packageName: String): String = labels.getOrPut(packageName) {
        try {
            val pm = context.packageManager
            pm.getApplicationInfo(packageName, 0).loadLabel(pm).toString()
        } catch (e: Exception) {
            packageName
        }
    }

    fun iconOf(component: ComponentName): Drawable = try {
        context.packageManager.getActivityIcon(component)
    } catch (e: Exception) {
        context.packageManager.defaultActivityIcon
    }

    fun iconOf(packageName: String): Drawable = try {
        context.packageManager.getApplicationIcon(packageName)
    } catch (e: Exception) {
        context.packageManager.defaultActivityIcon
    }

    /** Starts [app] on the default display; desktop mode opens it on the active workspace. */
    fun launch(app: App) {
        val intent = Intent(Intent.ACTION_MAIN)
            .addCategory(Intent.CATEGORY_LAUNCHER)
            .setComponent(app.component)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_RESET_TASK_IF_NEEDED)
        val options = ActivityOptions.makeBasic().setLaunchDisplayId(Display.DEFAULT_DISPLAY)
        try {
            context.startActivity(intent, options.toBundle())
        } catch (e: Exception) {
            Log.w(Desktop.TAG, "launch ${app.component}: $e")
        }
    }
}
