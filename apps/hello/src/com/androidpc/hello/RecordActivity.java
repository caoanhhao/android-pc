package com.androidpc.hello;

import android.app.Activity;
import android.media.AudioFormat;
import android.media.AudioRecord;
import android.media.MediaRecorder;
import android.os.Bundle;
import android.os.Handler;
import android.util.Log;

/**
 * Records two seconds from the microphone (AudioRecord, MIC source, 16 kHz
 * mono: AudioFlinger, the audio HAL's "Built-In Mic", the kernel's
 * pcmC0D0c), then logs RECORD_OK with the RMS level of the second half and
 * the median zero-crossing frequency of its 100 ms windows, or RECORD_FAIL. Needs
 * RECORD_AUDIO granted (tests: `pm grant`).
 */
public class RecordActivity extends Activity {
    static final int RATE = 16000;
    static final int SECONDS = 2;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        new Thread(() -> {
            try {
                int min = AudioRecord.getMinBufferSize(RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT);
                AudioRecord rec = new AudioRecord(MediaRecorder.AudioSource.MIC, RATE, AudioFormat.CHANNEL_IN_MONO,
                        AudioFormat.ENCODING_PCM_16BIT, Math.max(min, RATE));
                short[] pcm = new short[RATE * SECONDS];
                rec.startRecording();
                int got = 0;
                while (got < pcm.length) {
                    int n = rec.read(pcm, got, pcm.length - got);
                    if (n <= 0) {
                        break;
                    }
                    got += n;
                }
                rec.stop();
                rec.release();
                // The second half (past any start-up silence), in 100 ms
                // windows: the RMS over all, the median of the windows'
                // zero-crossing frequencies (a gap where a late reader
                // lost frames only spoils its own window).
                int from = got / 2;
                int win = RATE / 10;
                double sum = 0;
                for (int i = from; i < got; i++) {
                    sum += (double) pcm[i] * pcm[i];
                }
                java.util.ArrayList<Double> freqs = new java.util.ArrayList<>();
                for (int w = from; w + win <= got; w += win) {
                    int crossings = 0;
                    for (int i = w + 1; i < w + win; i++) {
                        if ((pcm[i - 1] < 0) != (pcm[i] < 0)) {
                            crossings++;
                        }
                    }
                    freqs.add(crossings / 2.0 / ((double) win / RATE));
                }
                java.util.Collections.sort(freqs);
                double rms = Math.sqrt(sum / Math.max(got - from, 1));
                double freq = freqs.isEmpty() ? 0 : freqs.get(freqs.size() / 2);
                Log.i(MainActivity.TAG, String.format("RECORD_OK frames %d rms %.0f freq %.0f", got, rms, freq));
            } catch (RuntimeException e) {
                Log.e(MainActivity.TAG, "RECORD_FAIL " + e);
            }
        }).start();
        new Handler().postDelayed(this::finish, SECONDS * 1000 + 2000);
    }
}
