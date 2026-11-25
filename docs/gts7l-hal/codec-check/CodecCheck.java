import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaCodecList;
import android.media.MediaExtractor;
import android.media.MediaFormat;
import java.nio.ByteBuffer;

/**
 * Lists, decodes and encodes with MediaCodec; run with app_process (build.sh):
 *   list                      every codec MediaCodecList reports
 *   decode FILE [CODEC]       decodes every track of FILE (by default with the first codec
 *                             MediaCodecList picks), prints the codec, frame count and time
 *   encode MIME [W H FRAMES]  encodes FRAMES synthetic YUV frames, prints the output size
 */
public class CodecCheck {
    public static void main(String[] args) throws Exception {
        String mode = args.length > 0 ? args[0] : "list";
        int rc = 0;
        if (mode.equals("list")) list();
        else if (mode.equals("decode")) {
            rc = decodeAll(args[1], args.length > 2 ? args[2] : null) ? 0 : 1;
        } else if (mode.equals("encode")) {
            int w = args.length > 3 ? Integer.parseInt(args[2]) : 1280;
            int h = args.length > 3 ? Integer.parseInt(args[3]) : 720;
            int n = args.length > 4 ? Integer.parseInt(args[4]) : 60;
            rc = encode(args[1], w, h, n) ? 0 : 1;
        }
        System.exit(rc);
    }

    static void list() {
        for (MediaCodecInfo info : new MediaCodecList(MediaCodecList.ALL_CODECS).getCodecInfos()) {
            System.out.println((info.isEncoder() ? "enc " : "dec ")
                    + (info.isHardwareAccelerated() ? "hw " : "sw ") + info.getName() + " "
                    + String.join(",", info.getSupportedTypes()));
        }
    }

    static boolean decodeAll(String path, String codecName) throws Exception {
        MediaExtractor ex = new MediaExtractor();
        ex.setDataSource(path);
        boolean ok = true;
        for (int t = 0; t < ex.getTrackCount(); t++) {
            ok &= decodeTrack(path, t, codecName);
        }
        ex.release();
        return ok;
    }

    static boolean decodeTrack(String path, int track, String codecName) throws Exception {
        MediaExtractor ex = new MediaExtractor();
        ex.setDataSource(path);
        ex.selectTrack(track);
        MediaFormat fmt = ex.getTrackFormat(track);
        String mime = fmt.getString(MediaFormat.KEY_MIME);
        MediaCodec c = codecName != null ? MediaCodec.createByCodecName(codecName)
                : MediaCodec.createDecoderByType(mime);
        c.configure(fmt, null, null, 0);
        c.start();
        MediaCodec.BufferInfo bi = new MediaCodec.BufferInfo();
        long t0 = System.nanoTime();
        int inSamples = 0, outFrames = 0;
        long outBytes = 0;
        boolean inEos = false, outEos = false;
        while (!outEos && System.nanoTime() - t0 < 60_000_000_000L) {
            if (!inEos) {
                int i = c.dequeueInputBuffer(10_000);
                if (i >= 0) {
                    int n = ex.readSampleData(c.getInputBuffer(i), 0);
                    if (n < 0) {
                        c.queueInputBuffer(i, 0, 0, 0, MediaCodec.BUFFER_FLAG_END_OF_STREAM);
                        inEos = true;
                    } else {
                        c.queueInputBuffer(i, 0, n, ex.getSampleTime(), 0);
                        inSamples++;
                        ex.advance();
                    }
                }
            }
            int o = c.dequeueOutputBuffer(bi, 10_000);
            if (o >= 0) {
                if (bi.size > 0) {
                    outFrames++;
                    outBytes += bi.size;
                }
                outEos = (bi.flags & MediaCodec.BUFFER_FLAG_END_OF_STREAM) != 0;
                c.releaseOutputBuffer(o, false);
            }
        }
        long ms = (System.nanoTime() - t0) / 1_000_000;
        MediaFormat of = c.getOutputFormat();
        String shape = mime.startsWith("video/")
                ? of.getInteger(MediaFormat.KEY_WIDTH) + "x" + of.getInteger(MediaFormat.KEY_HEIGHT)
                : of.getInteger(MediaFormat.KEY_SAMPLE_RATE) + "Hz/"
                        + of.getInteger(MediaFormat.KEY_CHANNEL_COUNT) + "ch";
        System.out.println("decode " + path + " track " + track + " " + mime + " codec="
                + c.getName() + " in=" + inSamples + " out=" + outFrames + " bytes=" + outBytes
                + " " + shape + " " + ms + "ms" + (outEos ? " EOS" : " TIMEOUT"));
        c.stop();
        c.release();
        ex.release();
        return outEos && outFrames > 0;
    }

    static boolean encode(String mime, int w, int h, int frames) throws Exception {
        MediaFormat fmt = MediaFormat.createVideoFormat(mime, w, h);
        fmt.setInteger(MediaFormat.KEY_COLOR_FORMAT,
                MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible);
        fmt.setInteger(MediaFormat.KEY_BIT_RATE, 4_000_000);
        fmt.setInteger(MediaFormat.KEY_FRAME_RATE, 30);
        fmt.setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 1);
        MediaCodec c = MediaCodec.createEncoderByType(mime);
        c.configure(fmt, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
        c.start();
        byte[] frame = new byte[w * h * 3 / 2];
        MediaCodec.BufferInfo bi = new MediaCodec.BufferInfo();
        long t0 = System.nanoTime();
        int queued = 0, packets = 0;
        long bytes = 0;
        boolean outEos = false;
        while (!outEos && System.nanoTime() - t0 < 60_000_000_000L) {
            if (queued <= frames) {
                int i = c.dequeueInputBuffer(10_000);
                if (i >= 0) {
                    ByteBuffer b = c.getInputBuffer(i);
                    if (queued == frames) {
                        c.queueInputBuffer(i, 0, 0, 0, MediaCodec.BUFFER_FLAG_END_OF_STREAM);
                    } else {
                        // A moving gradient, so the encoder has motion to code.
                        for (int y = 0; y < h; y++) {
                            for (int x = 0; x < w; x++) frame[y * w + x] = (byte) (x + y + queued * 4);
                        }
                        b.clear();
                        b.put(frame, 0, Math.min(frame.length, b.remaining()));
                        c.queueInputBuffer(i, 0, b.position(), queued * 33_333L, 0);
                    }
                    queued++;
                }
            }
            int o = c.dequeueOutputBuffer(bi, 10_000);
            if (o >= 0) {
                if (bi.size > 0) {
                    packets++;
                    bytes += bi.size;
                }
                outEos = (bi.flags & MediaCodec.BUFFER_FLAG_END_OF_STREAM) != 0;
                c.releaseOutputBuffer(o, false);
            }
        }
        long ms = (System.nanoTime() - t0) / 1_000_000;
        System.out.println("encode " + mime + " " + w + "x" + h + " codec=" + c.getName()
                + " frames=" + frames + " packets=" + packets + " bytes=" + bytes + " " + ms + "ms"
                + (outEos ? " EOS" : " TIMEOUT"));
        c.stop();
        c.release();
        return outEos && packets > 0;
    }
}
