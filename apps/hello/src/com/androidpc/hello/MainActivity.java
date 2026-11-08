package com.androidpc.hello;

import android.app.Activity;
import android.os.Bundle;
import android.util.Log;
import android.widget.TextView;

/** One Activity that shows a string produced by the x86_64 JNI library. */
public class MainActivity extends Activity {
    static final String TAG = "AndroidPcHello";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.main);
        String info = Native.report();
        ((TextView) findViewById(R.id.native_info)).setText(info);
        // Tests look for this line in logcat.
        Log.i(TAG, "HELLO_OK " + info.replace('\n', ' '));
    }
}
