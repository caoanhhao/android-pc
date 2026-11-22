package com.android.systemui.hypr

import android.app.ActivityManager
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.os.RemoteException
import android.os.SystemClock
import android.os.UserHandle
import android.util.Log
import android.view.Display
import android.view.InputDevice
import android.view.KeyCharacterMap
import android.view.KeyEvent
import android.view.WindowManager
import android.view.WindowManagerGlobal
import android.hardware.input.InputManager
import android.hardware.input.InputManagerGlobal
import com.android.hyprtile.ScreenshotMode
import com.android.internal.util.ScreenshotHelper
import com.android.internal.util.ScreenshotRequest
import com.android.systemui.CoreStartable
import com.android.systemui.dagger.SysUISingleton
import com.android.systemui.screencapture.domain.interactor.ScreenCaptureKeyboardShortcutInteractor
import com.android.systemui.user.domain.interactor.UserSwitcherInteractor
import javax.inject.Inject

/**
 * The system-action dispatchers of Phase E9, run inside SystemUI's own graph (they need its
 * interactors, which Shell's graph cannot reach): `clipboardhistory` (the [HyprClipboard]
 * picker), `translate` (the selection or clipboard to Google Translate), `screenshot area|full`
 * (SystemUI's large-screen pre-capture UI / a full-screen screenshot), `lock` (the keyguard,
 * as the power menu's Lock does) and `userswitcher` (the shade's user switcher dialog).
 * [HyprTileController] hands these names to [dispatch]; everything runs on the main thread.
 */
@SysUISingleton
class HyprSystemActions
@Inject
constructor(
    private val context: Context,
    private val userSwitcherInteractor: UserSwitcherInteractor,
    private val screenCaptureShortcuts: ScreenCaptureKeyboardShortcutInteractor,
) : CoreStartable {
    private val main = Handler(Looper.getMainLooper())
    private val clipboard = HyprClipboard(context, main, ::injectCtrl)
    private val screenshotHelper by lazy { ScreenshotHelper(context) }

    override fun start() {
        clipboard.start()
        instance = this
    }

    private fun perform(name: String, arg: String): Boolean {
        val mode = if (name == "screenshot") ScreenshotMode.parse(arg) ?: return false else null
        main.post {
            Log.i(TAG, "$name $arg")
            when (name) {
                "clipboardhistory" -> clipboard.togglePicker()
                "translate" -> translate()
                "screenshot" -> screenshot(mode!!)
                "lock" -> lock()
                "userswitcher" -> userSwitcherInteractor.showUserSwitcher(null)
            }
        }
        return true
    }

    private fun screenshot(mode: ScreenshotMode) {
        when (mode) {
            // Falls back to a full-screen shot by itself where the pre-capture UI is off.
            ScreenshotMode.AREA -> screenCaptureShortcuts.attemptPartialRegionScreenshot()
            ScreenshotMode.FULL -> screenshotHelper.takeScreenshot(
                ScreenshotRequest.Builder(
                    WindowManager.TAKE_SCREENSHOT_FULLSCREEN,
                    WindowManager.ScreenshotSource.SCREENSHOT_KEY_OTHER,
                ).build(),
                main,
                null,
            )
        }
    }

    /** What the power menu's Lock and META + L do: the keyguard now, the screen stays on. */
    private fun lock() {
        try {
            WindowManagerGlobal.getWindowManagerService()?.lockNow(null)
        } catch (e: RemoteException) {
            Log.e(TAG, "lockNow failed", e)
        }
    }

    /**
     * Copies the focused app's selection (CTRL + C), waits a moment for the clip to change,
     * then sends the clipboard's text to Google Translate's text-selection popup, which floats.
     * With no new clip the clipboard's current text goes; with no text at all the app opens.
     */
    private fun translate() {
        val pm = context.packageManager
        val user = ActivityManager.getCurrentUser()
        val installed = try {
            pm.getPackageInfoAsUser(TRANSLATE_PACKAGE, 0, user)
            true
        } catch (e: Exception) {
            false
        }
        if (!installed) {
            notifyTranslateMissing(user)
            return
        }
        clipboard.afterCopy(TRANSLATE_COPY_WAIT_MS) { text -> launchTranslate(text, user) }
    }

    private fun launchTranslate(text: String?, user: Int) {
        val pm = context.packageManager
        val handle = UserHandle.of(user)
        val candidates = if (text.isNullOrBlank()) emptyList() else listOf(
            Intent(Intent.ACTION_PROCESS_TEXT).setType("text/plain")
                .putExtra(Intent.EXTRA_PROCESS_TEXT, text)
                .putExtra(Intent.EXTRA_PROCESS_TEXT_READONLY, true),
            Intent(Intent.ACTION_TRANSLATE).putExtra(Intent.EXTRA_TEXT, text),
            Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, text),
        )
        val intent = candidates.map { it.setPackage(TRANSLATE_PACKAGE) }.firstOrNull {
            pm.queryIntentActivitiesAsUser(it, 0, user).isNotEmpty()
        } ?: pm.getLaunchIntentForPackage(TRANSLATE_PACKAGE)
        if (intent == null) {
            notifyTranslateMissing(user)
            return
        }
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        try {
            context.startActivityAsUser(intent, handle)
        } catch (e: RuntimeException) {
            Log.e(TAG, "translate: $intent", e)
        }
    }

    private fun notifyTranslateMissing(user: Int) {
        val nm = context.getSystemService(NotificationManager::class.java) ?: return
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL, "Hypr shell", NotificationManager.IMPORTANCE_DEFAULT))
        val store = Intent(Intent.ACTION_VIEW, Uri.parse("market://details?id=$TRANSLATE_PACKAGE"))
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        val tap = if (context.packageManager.queryIntentActivitiesAsUser(store, 0, user).isEmpty()) {
            null
        } else {
            PendingIntent.getActivityAsUser(context, 0, store, PendingIntent.FLAG_IMMUTABLE, null,
                UserHandle.of(user))
        }
        val n = Notification.Builder(context, CHANNEL)
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setContentTitle("Google Translate is not installed")
            .setContentText("Quick translate needs it: install $TRANSLATE_PACKAGE")
            .setContentIntent(tap)
            .setAutoCancel(true)
            .build()
        nm.notifyAsUser(TAG, NOTIFY_TRANSLATE, n, UserHandle.of(user))
    }

    /**
     * Sends CTRL + [keyCode] to the focused window, as a keyboard would (paste and copy for the
     * clipboard picker and translate).
     */
    private fun injectCtrl(keyCode: Int) {
        val now = SystemClock.uptimeMillis()
        val meta = KeyEvent.META_CTRL_ON or KeyEvent.META_CTRL_LEFT_ON
        val im = InputManagerGlobal.getInstance()
        for (action in intArrayOf(KeyEvent.ACTION_DOWN, KeyEvent.ACTION_UP)) {
            val ev = KeyEvent(now, now, action, keyCode, 0, meta, KeyCharacterMap.VIRTUAL_KEYBOARD,
                0, KeyEvent.FLAG_FROM_SYSTEM, InputDevice.SOURCE_KEYBOARD)
            ev.displayId = Display.INVALID_DISPLAY
            im.injectInputEvent(ev, InputManager.INJECT_INPUT_EVENT_MODE_ASYNC)
        }
    }

    companion object {
        private const val TAG = "HyprSystemActions"
        private const val TRANSLATE_PACKAGE = "com.google.android.apps.translate"
        private const val TRANSLATE_COPY_WAIT_MS = 400L
        private const val CHANNEL = "hypr_shell"
        private const val NOTIFY_TRANSLATE = 1

        /** The dispatchers this class runs. */
        val NAMES = setOf("clipboardhistory", "translate", "screenshot", "lock", "userswitcher")

        @Volatile private var instance: HyprSystemActions? = null

        /**
         * Runs dispatcher [name] (one of [NAMES]) from any thread; false when SystemUI's graph
         * has not started this class yet or the argument is bad.
         */
        fun dispatch(name: String, arg: String): Boolean =
            instance?.perform(name, arg) ?: false.also { Log.w(TAG, "$name: not started yet") }
    }
}
