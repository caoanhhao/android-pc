package com.android.systemui

import android.content.Context

/** Starts SystemUI with [PcSystemUIInitializer] (the manifest's appComponentFactory). */
class PcSystemUIAppComponentFactory : SystemUIAppComponentFactoryBase() {
    override fun createSystemUIInitializer(context: Context) = PcSystemUIInitializer(context)
}
