package com.android.hyprlauncher

import android.app.role.RoleManager
import android.content.Context
import android.content.Intent
import android.util.Log

/**
 * Makes HyprLauncher the HOME role holder once per user (Phase D4). On a fresh userdata the role
 * controller already picks it (the only HOME activity besides Settings' FallbackHome, at a
 * higher priority); on a userdata from an earlier build the role still names Smart Dock or
 * Launcher3, which this build no longer ships. A holder the user chose later (another
 * launcher installed from a store) is left alone, and so is any change after the first run.
 */
object HomeRole {
    private const val PREFS = "hypr"
    private const val DONE = "home_role_checked"
    /** Home apps of earlier android-pc builds (H4 Smart Dock, Launcher3 until I2). */
    private val REPLACED = setOf("cu.axel.smartdock", "com.android.launcher3")

    fun ensure(context: Context) {
        // Device-protected storage: the service runs before the user is unlocked.
        val prefs = context.createDeviceProtectedStorageContext()
            .getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        if (prefs.getBoolean(DONE, false)) return
        val rm = context.getSystemService(RoleManager::class.java) ?: return
        val holder = try {
            rm.getRoleHoldersAsUser(RoleManager.ROLE_HOME, context.user).firstOrNull()
        } catch (e: Exception) {
            Log.w(Desktop.TAG, "home role: $e")
            return
        }
        if (holder == context.packageName ||
            (holder != null && holder !in REPLACED && installed(context, holder))) {
            Log.i(Desktop.TAG, "home role holder is $holder, kept")
            prefs.edit().putBoolean(DONE, true).apply()
            return
        }
        Log.i(Desktop.TAG, "home role holder is $holder, taking it over")
        rm.addRoleHolderAsUser(RoleManager.ROLE_HOME, context.packageName,
            RoleManager.MANAGE_HOLDERS_FLAG_DONT_KILL_APP, context.user, context.mainExecutor) { ok ->
            Log.i(Desktop.TAG, "home role set: $ok")
            if (!ok) return@addRoleHolderAsUser
            prefs.edit().putBoolean(DONE, true).apply()
            context.startActivity(Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_HOME)
                .setPackage(context.packageName).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        }
    }

    private fun installed(context: Context, pkg: String) = try {
        context.packageManager.getPackageInfo(pkg, 0)
        true
    } catch (e: Exception) {
        false
    }
}
