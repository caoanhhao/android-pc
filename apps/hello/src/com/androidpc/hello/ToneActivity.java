package com.androidpc.hello;

import android.app.Activity;
import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioManager;
import android.media.AudioTrack;
import android.os.Bundle;
import android.os.Handler;
import android.util.Log;

/**
 * Plays a 440 Hz sine for two seconds through a streaming AudioTrack (AudioFlinger,
 * the audio HAL, the kernel's ALSA device) at full media volume, then
 * finishes. Tests start it with `am start -n com.androidpc.hello/.ToneActivity`
 * and look for TONE_OK / TONE_FAIL in logcat.
 */
public class ToneActivity extends Activity {
    static final int RATE = 48000;
    static final int SECONDS = 2;
    AudioTrack track;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        try {
            AudioManager am = (AudioManager) getSystemService(AUDIO_SERVICE);
            am.setStreamVolume(AudioManager.STREAM_MUSIC, am.getStreamMaxVolume(AudioManager.STREAM_MUSIC), 0);
            short[] pcm = new short[RATE * SECONDS * 2];
            for (int i = 0; i < RATE * SECONDS; i++) {
                short v = (short) (Math.sin(2 * Math.PI * 440 * i / RATE) * 16000);
                pcm[2 * i] = v;
                pcm[2 * i + 1] = v;
            }
            track = new AudioTrack.Builder()
                    .setAudioAttributes(new AudioAttributes.Builder()
                            .setUsage(AudioAttributes.USAGE_MEDIA)
                            .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                            .build())
                    .setAudioFormat(new AudioFormat.Builder()
                            .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                            .setSampleRate(RATE)
                            .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                            .build())
                    .setTransferMode(AudioTrack.MODE_STREAM)
                    .build();
            track.play();
            final AudioTrack t = track;
            new Thread(() -> {
                int n = t.write(pcm, 0, pcm.length);
                Log.i(MainActivity.TAG, "TONE_OK wrote " + n + " samples, state " + t.getPlayState());
            }).start();
        } catch (RuntimeException e) {
            Log.e(MainActivity.TAG, "TONE_FAIL " + e);
        }
        new Handler().postDelayed(this::finish, SECONDS * 1000 + 1500);
    }

    @Override
    protected void onDestroy() {
        if (track != null) {
            track.release();
        }
        super.onDestroy();
    }
}
