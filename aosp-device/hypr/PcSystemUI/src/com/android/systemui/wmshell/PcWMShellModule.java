package com.android.systemui.wmshell;

import android.content.Context;
import android.hardware.input.InputManager;

import com.android.wm.shell.ShellTaskOrganizer;
import com.android.wm.shell.common.DisplayController;
import com.android.wm.shell.common.ShellExecutor;
import com.android.wm.shell.dagger.WMShellModule;
import com.android.wm.shell.dagger.WMSingleton;
import com.android.wm.shell.desktopmode.DesktopMixedTransitionHandler;
import com.android.wm.shell.desktopmode.DesktopModeKeyGestureHandler;
import com.android.wm.shell.desktopmode.DesktopTasksController;
import com.android.wm.shell.desktopmode.DesktopUserRepositories;
import com.android.wm.shell.desktopmode.SnapController;
import com.android.wm.shell.desktopmode.multidesks.DeskSwitchTransitionHandler;
import com.android.wm.shell.shared.annotations.ShellMainThread;
import com.android.wm.shell.sysui.ShellController;
import com.android.wm.shell.sysui.ShellInit;
import com.android.wm.shell.transition.FocusTransitionObserver;
import com.android.wm.shell.transition.Transitions;
import com.android.wm.shell.windowdecor.DesktopModeWindowDecorViewModel;

import dagger.Module;
import dagger.Provides;

import java.util.Optional;

/**
 * WM Shell bindings of PcSystemUI: AOSP's handheld WMShellModule plus the hyprtile pieces,
 * which are added next to Shell's own objects (Phase D1: the controller; D2: keybinds and the
 * snap handler that keeps Shell's two-window tiling out of the way; D3: decorations, animations
 * and mouse binds, owned by the controller; D4: IHyprShell for HyprLauncher).
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
            Optional<DesktopTasksController> desktopTasksController,
            DeskSwitchTransitionHandler deskSwitchTransitionHandler,
            FocusTransitionObserver focusTransitionObserver,
            Optional<DesktopModeKeyGestureHandler> shellKeyGestureHandler,
            @ShellMainThread ShellExecutor mainExecutor,
            SnapController snapController,
            Optional<DesktopMixedTransitionHandler> mixedHandler,
            InputManager inputManager,
            Optional<DesktopModeWindowDecorViewModel> decorViewModel) {
        return new HyprTileController(context, shellInit, transitions, taskOrganizer, displayController,
                userRepositories, desktopTasksController, deskSwitchTransitionHandler,
                focusTransitionObserver, shellKeyGestureHandler, mainExecutor, snapController,
                mixedHandler, inputManager, decorViewModel);
    }

    @WMSingleton
    @Provides
    static HyprKeybinds provideHyprKeybinds(
            ShellInit shellInit,
            InputManager inputManager,
            HyprTileController controller,
            Optional<DesktopModeKeyGestureHandler> shellKeyGestureHandler,
            @ShellMainThread ShellExecutor mainExecutor) {
        return new HyprKeybinds(shellInit, inputManager, controller, shellKeyGestureHandler,
                mainExecutor);
    }

    @WMSingleton
    @Provides
    static HyprSnapHandler provideHyprSnapHandler(
            ShellInit shellInit,
            SnapController snapController,
            Optional<DesktopModeWindowDecorViewModel> decorViewModel,
            HyprTileController controller) {
        return new HyprSnapHandler(shellInit, snapController, decorViewModel, controller);
    }

    @WMSingleton
    @Provides
    static HyprShell provideHyprShell(
            Context context,
            ShellInit shellInit,
            ShellController shellController,
            HyprTileController controller,
            @ShellMainThread ShellExecutor mainExecutor) {
        return new HyprShell(context, shellInit, shellController, controller, mainExecutor);
    }
}
