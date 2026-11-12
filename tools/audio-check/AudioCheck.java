// End-to-end check of the audio path through the framework (AudioFlinger and the audio
// HAL), run from adb with app_process; see tools/audio-check.sh.
//
//   play SECONDS HZ    plays a sine tone (media usage, 48 kHz stereo)
//   record SECONDS     records from the default microphone (48 kHz mono) and prints the
//                      peak and RMS levels

import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioRecord;
import android.media.AudioTrack;
import android.media.MediaRecorder;

public final class AudioCheck {
    private static final int RATE = 48000;

    public static void main(String[] args) throws Exception {
        if (args.length >= 3 && args[0].equals("play")) {
            play(Double.parseDouble(args[1]), Double.parseDouble(args[2]));
        } else if (args.length >= 2 && args[0].equals("record")) {
            record(Double.parseDouble(args[1]));
        } else {
            System.err.println("usage: AudioCheck play SECONDS HZ | record SECONDS");
            System.exit(2);
        }
        // AudioSystem leaves a non-daemon looper thread behind.
        System.exit(0);
    }

    private static void play(double seconds, double hz) {
        AudioTrack track = new AudioTrack.Builder()
                .setAudioAttributes(new AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                        .build())
                .setAudioFormat(new AudioFormat.Builder()
                        .setSampleRate(RATE)
                        .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .build())
                .setTransferMode(AudioTrack.MODE_STREAM)
                .build();
        int frames = (int) (seconds * RATE);
        short[] chunk = new short[2 * RATE / 10];
        track.play();
        for (int done = 0; done < frames; ) {
            int n = Math.min(chunk.length / 2, frames - done);
            for (int i = 0; i < n; i++) {
                short s = (short) (12000 * Math.sin(2 * Math.PI * hz * (done + i) / RATE));
                chunk[2 * i] = s;
                chunk[2 * i + 1] = s;
            }
            track.write(chunk, 0, 2 * n);
            done += n;
        }
        track.stop();
        track.release();
        System.out.println("played " + seconds + " s of " + hz + " Hz");
    }

    private static void record(double seconds) {
        int min = AudioRecord.getMinBufferSize(RATE, AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT);
        AudioRecord record = new AudioRecord(MediaRecorder.AudioSource.MIC, RATE,
                AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT, 4 * min);
        short[] buf = new short[RATE / 10];
        int frames = (int) (seconds * RATE);
        long sumSquares = 0;
        int peak = 0;
        int read = 0;
        record.startRecording();
        while (read < frames) {
            int n = record.read(buf, 0, Math.min(buf.length, frames - read));
            if (n <= 0) {
                System.err.println("read failed: " + n);
                break;
            }
            for (int i = 0; i < n; i++) {
                peak = Math.max(peak, Math.abs(buf[i]));
                sumSquares += (long) buf[i] * buf[i];
            }
            read += n;
        }
        record.stop();
        record.release();
        double rms = read > 0 ? Math.sqrt((double) sumSquares / read) : 0;
        System.out.printf("recorded %d frames, peak %d, rms %.1f%n", read, peak, rms);
    }
}
