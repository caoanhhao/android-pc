package org.androidpc.waydroid;

import android.content.ComponentName;
import android.content.ContentResolver;
import android.content.Context;
import android.content.IIntentReceiver;
import android.content.IIntentSender;
import android.content.Intent;
import android.content.IntentSender;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageInstaller;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.net.Uri;
import android.os.Binder;
import android.os.Bundle;
import android.os.IBinder;
import android.os.Process;
import android.os.SystemProperties;
import android.os.UserHandle;
import android.provider.Settings;
import android.util.Log;

import lineageos.waydroid.AppInfo;
import lineageos.waydroid.IPlatform;

import java.io.File;
import java.io.FileInputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.TimeUnit;

/**
 * Waydroid's IPlatform for the AOSP 17 image: what the host's `waydroid app ...`,
 * `show-full-ui` and `prop` commands call (docs/waydroid-aosp17.md).
 */
final class PlatformService extends IPlatform.Stub {
    private static final String TAG = "WaydroidPlatform";
    // settingsPut/Get modes, as the host passes them (2 = global, for policy_control).
    private static final int SETTINGS_SYSTEM = 0;
    private static final int SETTINGS_SECURE = 1;
    private static final int SETTINGS_GLOBAL = 2;

    private final Context mContext;
    private final PackageManager mPm;

    PlatformService(Context context) {
        mContext = context;
        mPm = context.getPackageManager();
    }

    // The container has no SELinux, so nothing else keeps apps from calling this system-uid
    // service: only the host (root or the desktop user, uid 1000 in the container) and the shell.
    private static void enforceCaller() {
        int uid = Binder.getCallingUid();
        if (uid != Process.ROOT_UID && uid != Process.SYSTEM_UID && uid != Process.SHELL_UID) {
            throw new SecurityException("waydroidplatform: uid " + uid + " not allowed");
        }
    }

    @Override
    public String getprop(String name, String def) {
        enforceCaller();
        return SystemProperties.get(name, def);
    }

    @Override
    public void setprop(String name, String value) {
        enforceCaller();
        long token = Binder.clearCallingIdentity();
        try {
            SystemProperties.set(name, value);
        } finally {
            Binder.restoreCallingIdentity(token);
        }
    }

    @Override
    public List<AppInfo> getAppsInfo() {
        enforceCaller();
        long token = Binder.clearCallingIdentity();
        try {
            List<AppInfo> apps = new ArrayList<>();
            for (ApplicationInfo app : mPm.getInstalledApplications(0)) {
                AppInfo info = appInfo(app.packageName);
                if (info != null) apps.add(info);
            }
            return apps;
        } finally {
            Binder.restoreCallingIdentity(token);
        }
    }

    @Override
    public AppInfo getAppInfo(String packageName) {
        enforceCaller();
        long token = Binder.clearCallingIdentity();
        try {
            return appInfo(packageName);
        } finally {
            Binder.restoreCallingIdentity(token);
        }
    }

    // Apps with a launcher activity only, like the host's desktop entries.
    private AppInfo appInfo(String packageName) {
        Intent launch = mPm.getLaunchIntentForPackage(packageName);
        if (launch == null) return null;
        AppInfo info = new AppInfo();
        info.name = appName(packageName);
        info.packageName = packageName;
        info.action = launch.getAction();
        info.launchIntent = launch.toUri(0);
        ComponentName component = launch.getComponent();
        if (component != null) {
            info.componentPackageName = component.getPackageName();
            info.componentClassName = component.getClassName();
        }
        if (launch.getCategories() != null) info.categories.addAll(launch.getCategories());
        return info;
    }

    private String appName(String packageName) {
        try {
            return mPm.getApplicationLabel(mPm.getApplicationInfo(packageName, 0)).toString();
        } catch (PackageManager.NameNotFoundException e) {
            return "";
        }
    }

    @Override
    public String getAppName(String packageName) {
        enforceCaller();
        long token = Binder.clearCallingIdentity();
        try {
            return appName(packageName);
        } finally {
            Binder.restoreCallingIdentity(token);
        }
    }

    // The host copies the APK to /data/waydroid_tmp/base.apk first. Returns 0 on success.
    @Override
    public int installApp(String path) {
        enforceCaller();
        long token = Binder.clearCallingIdentity();
        try {
            PackageInstaller installer = mPm.getPackageInstaller();
            PackageInstaller.SessionParams params = new PackageInstaller.SessionParams(
                    PackageInstaller.SessionParams.MODE_FULL_INSTALL);
            int id = installer.createSession(params);
            File apk = new File(path);
            try (PackageInstaller.Session session = installer.openSession(id)) {
                try (InputStream in = new FileInputStream(apk);
                        OutputStream out = session.openWrite("base.apk", 0, apk.length())) {
                    byte[] buf = new byte[1 << 16];
                    for (int n; (n = in.read(buf)) > 0; ) out.write(buf, 0, n);
                    session.fsync(out);
                }
                LocalIntentReceiver receiver = new LocalIntentReceiver();
                session.commit(receiver.getIntentSender());
                return receiver.status();
            }
        } catch (Exception e) {
            Log.e(TAG, "installApp " + path, e);
            return PackageInstaller.STATUS_FAILURE;
        } finally {
            Binder.restoreCallingIdentity(token);
        }
    }

    @Override
    public int removeApp(String packageName) {
        enforceCaller();
        long token = Binder.clearCallingIdentity();
        try {
            LocalIntentReceiver receiver = new LocalIntentReceiver();
            mPm.getPackageInstaller().uninstall(packageName, receiver.getIntentSender());
            return receiver.status();
        } catch (Exception e) {
            Log.e(TAG, "removeApp " + packageName, e);
            return PackageInstaller.STATUS_FAILURE;
        } finally {
            Binder.restoreCallingIdentity(token);
        }
    }

    @Override
    public void launchApp(String packageName) {
        enforceCaller();
        long token = Binder.clearCallingIdentity();
        try {
            Intent launch = mPm.getLaunchIntentForPackage(packageName);
            if (launch == null) {
                Log.w(TAG, "launchApp: no launcher activity in " + packageName);
                return;
            }
            launch.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            mContext.startActivityAsUser(launch, UserHandle.CURRENT);
        } finally {
            Binder.restoreCallingIdentity(token);
        }
    }

    // Returns the package that handles the intent ("" if none), for the host's active_apps.
    @Override
    public String launchIntent(String action, String uri) {
        enforceCaller();
        long token = Binder.clearCallingIdentity();
        try {
            Intent intent = new Intent(action, uri == null || uri.isEmpty() ? null : Uri.parse(uri));
            intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            ResolveInfo ri = mPm.resolveActivity(intent, 0);
            if (ri == null || ri.activityInfo == null) return "";
            mContext.startActivityAsUser(intent, UserHandle.CURRENT);
            return ri.activityInfo.packageName;
        } finally {
            Binder.restoreCallingIdentity(token);
        }
    }

    @Override
    public void settingsPutString(int mode, String key, String value) {
        enforceCaller();
        long token = Binder.clearCallingIdentity();
        try {
            ContentResolver cr = mContext.getContentResolver();
            switch (mode) {
                case SETTINGS_SYSTEM -> Settings.System.putString(cr, key, value);
                case SETTINGS_SECURE -> Settings.Secure.putString(cr, key, value);
                case SETTINGS_GLOBAL -> Settings.Global.putString(cr, key, value);
                default -> Log.w(TAG, "settingsPutString: unknown mode " + mode);
            }
        } finally {
            Binder.restoreCallingIdentity(token);
        }
    }

    @Override
    public String settingsGetString(int mode, String key) {
        enforceCaller();
        long token = Binder.clearCallingIdentity();
        try {
            ContentResolver cr = mContext.getContentResolver();
            return switch (mode) {
                case SETTINGS_SYSTEM -> Settings.System.getString(cr, key);
                case SETTINGS_SECURE -> Settings.Secure.getString(cr, key);
                case SETTINGS_GLOBAL -> Settings.Global.getString(cr, key);
                default -> null;
            };
        } finally {
            Binder.restoreCallingIdentity(token);
        }
    }

    @Override
    public void settingsPutInt(int mode, String key, int value) {
        settingsPutString(mode, key, Integer.toString(value));
    }

    @Override
    public int settingsGetInt(int mode, String key) {
        String value = settingsGetString(mode, key);
        try {
            return value == null ? 0 : Integer.parseInt(value);
        } catch (NumberFormatException e) {
            return 0;
        }
    }

    /** Waits for a PackageInstaller result, as `pm install` does. */
    private static final class LocalIntentReceiver {
        private final LinkedBlockingQueue<Intent> mResult = new LinkedBlockingQueue<>();

        private final IIntentSender.Stub mSender = new IIntentSender.Stub() {
            @Override
            public void send(int code, Intent intent, String resolvedType, IBinder whitelistToken,
                    IIntentReceiver finishedReceiver, String requiredPermission, Bundle options) {
                mResult.offer(intent);
            }
        };

        IntentSender getIntentSender() {
            return new IntentSender((IIntentSender) mSender);
        }

        int status() throws InterruptedException {
            Intent result = mResult.poll(5, TimeUnit.MINUTES);
            if (result == null) return PackageInstaller.STATUS_FAILURE;
            return result.getIntExtra(PackageInstaller.EXTRA_STATUS,
                    PackageInstaller.STATUS_FAILURE);
        }
    }
}
