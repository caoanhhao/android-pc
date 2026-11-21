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

    /** The hyprtile controller; created before Shell initializes so it can register itself. */
    HyprTileController getHyprTileController();

    /** Hyprland-style keybinds, routed to the controller's dispatchers. */
    HyprKeybinds getHyprKeybinds();

    /** Routes Shell's snap events to hyprtile instead of Shell's two-window tiling. */
    HyprSnapHandler getHyprSnapHandler();

    /**
     * Creates the hyprtile controller after all of Shell's objects (getShell() pulls in the
     * whole graph), so its ShellInit callback runs last and its transition handler is the newest
     * one, the first that Transitions asks.
     */
    @Override
    default void init() {
        getShell();
        getHyprTileController();
        getHyprKeybinds();
        getHyprSnapHandler();
        WMComponent.super.init();
    }
}
