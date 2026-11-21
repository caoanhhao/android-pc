package com.android.hyprlauncher

import android.app.Service
import android.app.contextualsearch.ContextualSearchConfig
import android.content.Intent
import android.graphics.Region
import android.os.Bundle
import android.os.IBinder
import android.os.IRemoteCallback
import android.util.Log
import android.view.Display
import com.android.hyprshell.IHyprShell
import com.android.systemui.shared.recents.ILauncherProxy

/**
 * The recents provider's service (Phase D4), bound by SystemUI's LauncherProxyService through
 * `android.intent.action.QUICKSTEP_SERVICE` in the package of config_recentsComponentName.
 * It implements only what a desktop without a navigation bar or taskbar needs of
 * ILauncherProxy: onInitialize (Shell's interfaces, IHyprShell among them), the overview
 * toggles (recents key), the keyguard state (the bar hides under a see-through keyguard) and
 * onUnbind; the rest are navigation bar, taskbar and assistant callbacks, ignored.
 */
class LauncherService : Service() {
    private val desktop by lazy { Desktop.get(this) }

    private val proxy = object : ILauncherProxy.Stub() {
        override fun onInitialize(params: Bundle) {
            val shell = params.getBinder(IHyprShell.DESCRIPTOR)
            Log.i(Desktop.TAG, "onInitialize: ${params.keySet().sorted()}")
            desktop.handler.post { desktop.attach(shell) }
        }

        override fun onOverviewToggle() {
            desktop.handler.post { desktop.overview.toggle() }
        }

        override fun onOverviewShown(triggeredFromAltTab: Boolean) {
            desktop.handler.post { desktop.overview.show(altTab = triggeredFromAltTab) }
        }

        override fun onOverviewHidden(triggeredFromAltTab: Boolean, triggeredFromHomeKey: Boolean) {
            desktop.handler.post {
                if (triggeredFromAltTab) desktop.overview.commitAltTab() else desktop.overview.hide()
            }
        }

        override fun onUnbind(reply: IRemoteCallback) {
            reply.sendResult(null)
        }

        override fun onActiveNavBarRegionChanges(activeRegion: Region?) {}
        override fun onAssistantAvailable(available: Boolean, longPressHomeEnabled: Boolean) {}
        override fun onAssistantVisibilityChanged(visibility: Float) {}
        override fun onAssistantOverrideInvoked(invocationType: Int) {}
        override fun onSystemUiStateChanged(stateFlags: Long, displayId: Int) {
            if (displayId != Display.DEFAULT_DISPLAY) return
            // QuickStepContract's SYSUI_STATE_STATUS_BAR_KEYGUARD_SHOWING and ..._OCCLUDED.
            val keyguard = stateFlags and KEYGUARD_SHOWING != 0L &&
                stateFlags and KEYGUARD_OCCLUDED == 0L
            desktop.handler.post { desktop.bar.setHiddenByKeyguard(keyguard) }
        }
        override fun onRotationProposal(rotation: Int, isValid: Boolean) {}
        override fun disable(displayId: Int, state1: Int, state2: Int, animate: Boolean) {}
        override fun onSystemBarAttributesChanged(displayId: Int, behavior: Int) {}
        override fun onTransitionModeUpdated(barMode: Int, checkBarModes: Boolean) {}
        override fun onNavButtonsDarkIntensityChanged(darkIntensity: Float) {}
        override fun onNavigationBarLumaSamplingEnabled(displayId: Int, enable: Boolean) {}
        override fun enterStageSplitFromRunningApp(displayId: Int, leftOrTop: Boolean) {}
        override fun onTaskbarToggled() {}
        override fun updateWallpaperVisibility(displayId: Int, visible: Boolean) {}
        override fun checkNavBarModes(displayId: Int) {}
        override fun finishBarAnimations(displayId: Int) {}
        override fun touchAutoDim(displayid: Int, reset: Boolean) {}
        override fun transitionTo(displayId: Int, barMode: Int, animate: Boolean) {}
        override fun appTransitionPending(pending: Boolean) {}
        override fun onDisplayAddSystemDecorations(displayId: Int) {}
        override fun onDisplayRemoved(displayId: Int) {}
        override fun onDisplayRemoveSystemDecorations(displayId: Int) {}
        override fun onActionCornerActivated(action: Int, displayId: Int) {}
        override fun invokeContextualSearch(entryPoint: Int, config: ContextualSearchConfig?) {}
    }

    override fun onCreate() {
        super.onCreate()
        desktop.start()
        HomeRole.ensure(this)
    }

    override fun onBind(intent: Intent?): IBinder = proxy

    private companion object {
        const val KEYGUARD_SHOWING = 1L shl 6
        const val KEYGUARD_OCCLUDED = 1L shl 9
    }
}
