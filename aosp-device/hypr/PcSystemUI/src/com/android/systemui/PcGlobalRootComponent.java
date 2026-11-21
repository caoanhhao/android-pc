package com.android.systemui;

import com.android.systemui.dagger.GlobalModule;
import com.android.systemui.dagger.GlobalRootComponent;
import com.android.systemui.wmshell.PcWMComponent;

import dagger.Component;

import javax.inject.Singleton;

/**
 * Root component of PcSystemUI: AOSP's ReferenceGlobalRootComponent with PcWMComponent in place
 * of WMComponent.
 */
@Singleton
@Component(modules = {GlobalModule.class})
public interface PcGlobalRootComponent extends GlobalRootComponent {

    /** Builder for a PcGlobalRootComponent. */
    @Component.Builder
    interface Builder extends GlobalRootComponent.Builder {
        PcGlobalRootComponent build();
    }

    /** Builder for the WM Shell subcomponent. */
    @Override
    PcWMComponent.Builder getWMComponentBuilder();

    /** Builder for the SysUI subcomponent. */
    @Override
    PcSysUIComponent.Builder getSysUIComponent();
}
