package pc.test.guesthook;

import android.app.Activity;
import android.os.Bundle;
import android.util.Log;
import dalvik.system.DexClassLoader;

// H3 test: libguesthook.so (arm64 only) hooks `read` in the host libartbase.so, then a
// DexClassLoader makes ART read this APK's dex through the hook (art::ReadMagicAndReset, the
// call Where Winds Meet crashed in).
public class MainActivity extends Activity {
    private static final String TAG = "GuestHookTest";

    static {
        System.loadLibrary("guesthook");
    }

    private static native int hookRead();
    private static native int hookedReads();

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        int r = hookRead();
        Log.i(TAG, "hookRead() = " + r);
        try {
            new DexClassLoader(getApplicationInfo().sourceDir, getCodeCacheDir().getPath(), null,
                    getClassLoader());
            Log.i(TAG, "DexClassLoader done, reads through the arm64 hook: " + hookedReads());
        } catch (Throwable t) {
            Log.e(TAG, "DexClassLoader failed", t);
        }
        Log.i(TAG, (r == 0 && hookedReads() > 0) ? "PASS" : "FAIL");
        finish();
    }
}
