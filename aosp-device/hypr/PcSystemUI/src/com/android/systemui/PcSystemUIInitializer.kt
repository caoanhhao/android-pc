package com.android.systemui

import android.content.Context
import com.android.systemui.dagger.GlobalRootComponent

/**
 * [SystemUIInitializer] for aosp_pc_x86_64: AOSP SystemUI on [PcGlobalRootComponent], whose WM
 * Shell is composed by [com.android.systemui.wmshell.PcWMShellModule].
 */
class PcSystemUIInitializer(context: Context) : SystemUIInitializer(context) {
    override fun getGlobalRootComponentBuilder(): GlobalRootComponent.Builder {
        return DaggerPcGlobalRootComponent.builder()
    }
}
