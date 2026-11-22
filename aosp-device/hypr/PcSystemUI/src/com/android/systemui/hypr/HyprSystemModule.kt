package com.android.systemui.hypr

import com.android.systemui.CoreStartable
import dagger.Binds
import dagger.Module
import dagger.multibindings.ClassKey
import dagger.multibindings.IntoMap

/** Starts [HyprSystemActions] with the rest of SystemUI's CoreStartables (Phase E9). */
@Module
interface HyprSystemModule {
    @Binds
    @IntoMap
    @ClassKey(HyprSystemActions::class)
    fun bindHyprSystemActions(impl: HyprSystemActions): CoreStartable
}
