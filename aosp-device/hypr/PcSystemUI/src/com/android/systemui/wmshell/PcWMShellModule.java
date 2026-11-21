package com.android.systemui.wmshell;

import com.android.wm.shell.dagger.WMShellModule;

import dagger.Module;

/**
 * WM Shell bindings of PcSystemUI. Phase D0: exactly AOSP's handheld WMShellModule, so the
 * behaviour is unchanged.
 *
 * Dagger cannot override a binding, so replacing one that WMShellModule provides (the desktop
 * task controller, the window decoration view model) means including WMShellBaseModule and the
 * other WMShellModule includes here and copying WMShellModule's remaining bindings, as
 * CarWMShellModule does. Bindings WMShellBaseModule declares as {@code @DynamicOverride} are the
 * intended override points. docs/hypr-desktop.md, "Shell extension points", lists which is which.
 */
@Module(includes = {WMShellModule.class})
public abstract class PcWMShellModule {
}
