package org.androidpc.waydroid;

import android.app.Application;
import android.os.ServiceManager;
import android.util.Log;

import java.io.File;

/**
 * A persistent system-uid app that registers Waydroid's "waydroidplatform" binder service, but
 * only in Waydroid's container (/vendor/waydroid.prop is the host's bind mount); on the PC it
 * does nothing.
 */
public class PlatformApp extends Application {
    private static final String TAG = "WaydroidPlatform";

    @Override
    public void onCreate() {
        super.onCreate();
        if (!new File("/vendor/waydroid.prop").exists()) return;
        ServiceManager.addService("waydroidplatform", new PlatformService(this));
        Log.i(TAG, "waydroidplatform registered");
    }
}
