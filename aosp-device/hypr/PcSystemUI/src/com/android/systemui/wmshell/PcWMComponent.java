package com.android.systemui.wmshell;

import com.android.wm.shell.dagger.WMComponent;
import com.android.wm.shell.dagger.WMSingleton;

import dagger.Subcomponent;

/**
 * WM Shell subcomponent of PcSystemUI, composed by {@link PcWMShellModule} instead of AOSP's
 * WMShellModule. Getters for the hyprtile pieces SysUI needs go here (as CarWMComponent does).
 */
@WMSingleton
@Subcomponent(modules = {PcWMShellModule.class})
public interface PcWMComponent extends WMComponent {

    /** Builder for a PcWMComponent. */
    @Subcomponent.Builder
    interface Builder extends WMComponent.Builder {
        PcWMComponent build();
    }
}
