package com.android.systemui.wmshell;

import android.content.Context;

import com.android.wm.shell.ShellTaskOrganizer;
import com.android.wm.shell.common.DisplayController;
import com.android.wm.shell.common.ShellExecutor;
import com.android.wm.shell.dagger.WMShellModule;
import com.android.wm.shell.dagger.WMSingleton;
import com.android.wm.shell.desktopmode.DesktopUserRepositories;
import com.android.wm.shell.shared.annotations.ShellMainThread;
import com.android.wm.shell.sysui.ShellInit;
import com.android.wm.shell.transition.Transitions;

import dagger.Module;
import dagger.Provides;

import java.util.Optional;

/**
 * WM Shell bindings of PcSystemUI: AOSP's handheld WMShellModule plus the hyprtile pieces,
 * which are added next to Shell's own objects (Phase D1).
 *
 * Dagger cannot override a binding, so replacing one that WMShellModule provides (the desktop
 * task controller, the window decoration view model) means including WMShellBaseModule and the
 * other WMShellModule includes here and copying WMShellModule's remaining bindings, as
 * CarWMShellModule does. Bindings WMShellBaseModule declares as {@code @DynamicOverride} are the
 * intended override points. docs/hypr-desktop.md, "Shell extension points", lists which is which.
 */
@Module(includes = {WMShellModule.class})
public abstract class PcWMShellModule {

    @WMSingleton
    @Provides
    static HyprTileController provideHyprTileController(
            Context context,
            ShellInit shellInit,
            Transitions transitions,
            ShellTaskOrganizer taskOrganizer,
            DisplayController displayController,
            Optional<DesktopUserRepositories> userRepositories,
            @ShellMainThread ShellExecutor mainExecutor) {
        return new HyprTileController(context, shellInit, transitions, taskOrganizer, displayController,
                userRepositories, mainExecutor);
    }
}
