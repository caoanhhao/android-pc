/*
 * Phase L1 (docs/nvidia-proprietary.md): shows the tail of /data/local/tmp/logstrip.txt (a
 * filtered kernel log written by pclogstrip.sh) in a strip on the right of the screen. The
 * window is an overlay that never takes focus or touches, so the UI under and beside it keeps
 * working; it is refreshed once a second.
 */
package com.androidpc.logstrip;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.graphics.PixelFormat;
import android.graphics.Typeface;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.util.DisplayMetrics;
import android.view.Gravity;
import android.view.WindowManager;
import android.widget.TextView;

import java.io.RandomAccessFile;

public class LogStripService extends Service {
    private static final String LOG_FILE = "/data/local/tmp/logstrip.txt";
    private static final String CHANNEL = "logstrip";
    private static final int TAIL_BYTES = 16 * 1024;

    private final Handler mHandler = new Handler(Looper.getMainLooper());
    private TextView mText;
    private long mLastLength = -1;

    private final Runnable mRefresh = new Runnable() {
        @Override
        public void run() {
            refresh();
            mHandler.postDelayed(this, 1000);
        }
    };

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        if (mText != null) return START_STICKY;
        NotificationManager nm = getSystemService(NotificationManager.class);
        nm.createNotificationChannel(
                new NotificationChannel(CHANNEL, "Log strip", NotificationManager.IMPORTANCE_LOW));
        Notification n = new Notification.Builder(this, CHANNEL)
                .setContentTitle("Kernel log strip")
                .setSmallIcon(android.R.drawable.ic_dialog_info)
                .build();
        startForeground(1, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE);

        DisplayMetrics dm = getResources().getDisplayMetrics();
        mText = new TextView(this);
        mText.setTypeface(Typeface.MONOSPACE);
        mText.setTextSize(10);
        mText.setTextColor(0xffd8ffd8);
        mText.setBackgroundColor(0xb0000000);
        // Newest line last, from the top: the bottom of the screen belongs to the taskbar and
        // notifications, which cover an overlay. One screen line per log line (no wrapping),
        // so the number of lines that fit is known.
        mText.setGravity(Gravity.TOP | Gravity.START);
        mText.setHorizontallyScrolling(true);
        int dp = (int) dm.density;
        mText.setPadding(6 * dp, 32 * dp, 6 * dp, 96 * dp);
        WindowManager.LayoutParams lp = new WindowManager.LayoutParams(
                dm.widthPixels * 30 / 100, WindowManager.LayoutParams.MATCH_PARENT,
                WindowManager.LayoutParams.TYPE_APPLICATION_OVERLAY,
                WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE
                        | WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE
                        | WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN,
                PixelFormat.TRANSLUCENT);
        lp.gravity = Gravity.TOP | Gravity.END;
        lp.setTitle("PcLogStrip");
        getSystemService(WindowManager.class).addView(mText, lp);
        mHandler.post(mRefresh);
        return START_STICKY;
    }

    private void refresh() {
        if (mText.getHeight() == 0) return;   // not laid out yet
        try (RandomAccessFile f = new RandomAccessFile(LOG_FILE, "r")) {
            long len = f.length();
            if (len == mLastLength) return;
            mLastLength = len;
            long start = Math.max(0, len - TAIL_BYTES);
            byte[] buf = new byte[(int) (len - start)];
            f.seek(start);
            f.readFully(buf);
            String[] lines = new String(buf).split("\n");
            int room = mText.getHeight() - mText.getPaddingTop() - mText.getPaddingBottom();
            int fit = Math.max(1, room / Math.max(1, mText.getLineHeight()));
            int from = Math.max(start > 0 ? 1 : 0, lines.length - fit);
            StringBuilder sb = new StringBuilder();
            for (int i = from; i < lines.length; i++) sb.append(lines[i]).append('\n');
            mText.setText(sb);
        } catch (Exception e) {
            mText.setText("log strip: " + e);
        }
    }
}
