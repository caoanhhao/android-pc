import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioRecord;
import android.media.AudioTrack;
import android.media.MediaRecorder;

/** Plays a tone and records from a source; run with app_process. */
public class AudioCheck {
    public static void main(String[] args) throws Exception {
        String mode = args.length > 0 ? args[0] : "play";
        int secs = args.length > 1 ? Integer.parseInt(args[1]) : 3;
        if (mode.equals("play")) play(secs, null);
        else if (mode.equals("loop")) loop(secs, 0);
        else if (mode.startsWith("loopfx")) loop(secs, Integer.parseInt(mode.substring(6)));
        else if (mode.equals("fx")) {
            for (android.media.audiofx.AudioEffect.Descriptor d :
                    android.media.audiofx.AudioEffect.queryEffects()) {
                System.out.println("effect " + d.name + " / " + d.implementor + " type=" + d.type);
            }
        }
        else record(Integer.parseInt(mode.substring(3)), secs);  // "rec1" = MIC, "rec8" = REMOTE_SUBMIX
        System.exit(0);
    }

    static int fxGainMb;

    static void play(int secs, android.media.AudioDeviceInfo pref) throws Exception {
        int rate = 48000;
        AudioTrack t = new AudioTrack.Builder()
                .setAudioAttributes(new AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC).build())
                .setAudioFormat(new AudioFormat.Builder().setSampleRate(rate)
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO).build())
                .setTransferMode(AudioTrack.MODE_STREAM).build();
        if (pref != null) System.out.println("setPreferredDevice=" + t.setPreferredDevice(pref));
        System.out.println("track state=" + t.getState() + " routed=" + t.getRoutedDevice());
        android.media.audiofx.LoudnessEnhancer le = null;
        if (fxGainMb != 0) {
            le = new android.media.audiofx.LoudnessEnhancer(t.getAudioSessionId());
            le.setTargetGain(fxGainMb);
            System.out.println("LoudnessEnhancer enable=" + le.setEnabled(true) + " gain="
                    + le.getTargetGain() + " enabled=" + le.getEnabled());
        }
        short[] buf = new short[rate / 10 * 2];
        t.play();
        long n = 0;
        for (int i = 0; i < secs * 10; i++) {
            for (int j = 0; j < buf.length / 2; j++, n++) {
                short v = (short) (Math.sin(2 * Math.PI * 1000 * n / rate) * 8000);
                buf[2 * j] = v;
                buf[2 * j + 1] = v;
            }
            int w = t.write(buf, 0, buf.length);
            if (i % 10 == 0) {
                android.media.AudioDeviceInfo d = t.getRoutedDevice();
                System.out.println("t=" + i / 10 + "s wrote=" + w + " head=" + t.getPlaybackHeadPosition()
                        + " underruns=" + t.getUnderrunCount()
                        + " routed=" + (d == null ? "null" : d.getType() + ":" + d.getProductName()));
            }
        }
        Thread.sleep(300);
        System.out.println("end head=" + t.getPlaybackHeadPosition() + " written=" + n
                + " underruns=" + t.getUnderrunCount());
        t.stop();
        t.release();
    }

    /** Plays into the Remote Submix output while a second thread records REMOTE_SUBMIX. */
    static void loop(int secs, int gainMb) throws Exception {
        // AudioManager.getDevicesStatic is hidden; no Context is needed under app_process.
        android.media.AudioDeviceInfo[] outs = (android.media.AudioDeviceInfo[])
                android.media.AudioManager.class.getMethod("getDevicesStatic", int.class)
                        .invoke(null, android.media.AudioManager.GET_DEVICES_OUTPUTS);
        android.media.AudioDeviceInfo sub = null;
        for (android.media.AudioDeviceInfo d : outs) {
            System.out.println("output device type=" + d.getType() + " addr=" + d.getAddress());
            if (d.getType() == android.media.AudioDeviceInfo.TYPE_REMOTE_SUBMIX) sub = d;
        }
        Thread rec = new Thread(() -> {
            try { record(MediaRecorder.AudioSource.REMOTE_SUBMIX, secs); }
            catch (Exception e) { e.printStackTrace(); }
        });
        rec.start();
        Thread.sleep(200);
        fxGainMb = gainMb;
        play(secs, sub);
        rec.join();
    }

    static void record(int source, int secs) throws Exception {
        int rate = 48000;
        int min = AudioRecord.getMinBufferSize(rate, AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT);
        AudioRecord r = new AudioRecord.Builder().setAudioSource(source)
                .setAudioFormat(new AudioFormat.Builder().setSampleRate(rate)
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setChannelMask(AudioFormat.CHANNEL_IN_MONO).build())
                .setBufferSizeInBytes(Math.max(min, rate)).build();
        System.out.println("source=" + source + " minbuf=" + min + " state=" + r.getState());
        if (r.getState() != AudioRecord.STATE_INITIALIZED) return;
        r.startRecording();
        System.out.println("recordingState=" + r.getRecordingState());
        short[] buf = new short[rate / 10];
        long total = 0, nonzero = 0;
        double sum = 0;
        int peak = 0;
        long start = System.nanoTime();
        for (int i = 0; i < secs * 10; i++) {
            int got = r.read(buf, 0, buf.length);
            if (got < 0) { System.out.println("read error " + got); break; }
            total += got;
            for (int j = 0; j < got; j++) {
                int v = buf[j];
                if (v != 0) nonzero++;
                sum += (double) v * v;
                peak = Math.max(peak, Math.abs(v));
            }
        }
        double el = (System.nanoTime() - start) / 1e9;
        android.media.AudioDeviceInfo d = r.getRoutedDevice();
        System.out.printf("frames=%d in %.2fs nonzero=%d rms=%.1f peak=%d routed=%s%n", total, el,
                nonzero, Math.sqrt(sum / Math.max(1, total)), peak,
                d == null ? "null" : d.getType() + ":" + d.getProductName());
        r.stop();
        r.release();
    }
}
