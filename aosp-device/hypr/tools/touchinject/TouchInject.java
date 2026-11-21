import android.os.SystemClock;
import android.view.InputDevice;
import android.view.InputEvent;
import android.view.MotionEvent;

import java.lang.reflect.Method;

/**
 * Injects multi-finger touchscreen gestures for testing the shell's touch support (Phase D6);
 * `input` only injects one pointer. Run as shell (INJECT_EVENTS):
 *
 *   CLASSPATH=/data/local/tmp/touchinject.dex app_process / TouchInject <command> ...
 *
 * Commands (display pixels, milliseconds):
 *   swipe N X Y DX DY MS        N fingers 120 px apart, starting at (X, Y), moving by (DX, DY)
 *   pinch CX CY R0 R1 MS        two fingers on a horizontal line around (CX, CY), from R0 to R1
 *   hold X Y MS [X2 Y2 MS2]     one finger held still for MS, then optionally dragged to (X2, Y2)
 *   tap X Y
 */
public final class TouchInject {
    private static Object sInput;
    private static Method sInject;

    public static void main(String[] args) throws Exception {
        Class<?> global = Class.forName("android.hardware.input.InputManagerGlobal");
        sInput = global.getMethod("getInstance").invoke(null);
        sInject = global.getMethod("injectInputEvent", InputEvent.class, int.class);
        switch (args[0]) {
            case "swipe": {
                int n = Integer.parseInt(args[1]);
                float x = f(args[2]), y = f(args[3]), dx = f(args[4]), dy = f(args[5]);
                long ms = Long.parseLong(args[6]);
                float[][] from = new float[n][], to = new float[n][];
                for (int i = 0; i < n; i++) {
                    from[i] = new float[] {x + 120 * i, y};
                    to[i] = new float[] {x + 120 * i + dx, y + dy};
                }
                gesture(from, to, 0, ms);
                break;
            }
            case "pinch": {
                float cx = f(args[1]), cy = f(args[2]), r0 = f(args[3]), r1 = f(args[4]);
                long ms = Long.parseLong(args[5]);
                gesture(new float[][] {{cx - r0, cy}, {cx + r0, cy}},
                        new float[][] {{cx - r1, cy}, {cx + r1, cy}}, 0, ms);
                break;
            }
            case "hold": {
                float x = f(args[1]), y = f(args[2]);
                long ms = Long.parseLong(args[3]);
                float[][] from = {{x, y}};
                float[][] to = args.length > 4 ? new float[][] {{f(args[4]), f(args[5])}} : from;
                gesture(from, to, ms, args.length > 6 ? Long.parseLong(args[6]) : 0);
                break;
            }
            case "tap":
                gesture(new float[][] {{f(args[1]), f(args[2])}},
                        new float[][] {{f(args[1]), f(args[2])}}, 50, 0);
                break;
            default:
                throw new IllegalArgumentException("unknown command " + args[0]);
        }
    }

    /** Fingers go down one after another, hold [hold] ms, move over [ms] ms, then lift. */
    private static void gesture(float[][] from, float[][] to, long hold, long ms)
            throws Exception {
        int n = from.length;
        long down = SystemClock.uptimeMillis();
        float[][] at = new float[n][];
        for (int i = 0; i < n; i++) at[i] = from[i].clone();
        for (int i = 0; i < n; i++) {
            int action = i == 0 ? MotionEvent.ACTION_DOWN
                    : MotionEvent.ACTION_POINTER_DOWN | (i << MotionEvent.ACTION_POINTER_INDEX_SHIFT);
            inject(down, action, at, i + 1);
            Thread.sleep(8);
        }
        long end = SystemClock.uptimeMillis() + hold;
        while (SystemClock.uptimeMillis() < end) {
            Thread.sleep(16);
            inject(down, MotionEvent.ACTION_MOVE, at, n);
        }
        int steps = (int) Math.max(1, ms / 16);
        for (int s = 1; s <= steps && ms > 0; s++) {
            float t = s / (float) steps;
            for (int i = 0; i < n; i++) {
                at[i][0] = from[i][0] + (to[i][0] - from[i][0]) * t;
                at[i][1] = from[i][1] + (to[i][1] - from[i][1]) * t;
            }
            inject(down, MotionEvent.ACTION_MOVE, at, n);
            Thread.sleep(16);
        }
        for (int i = n - 1; i > 0; i--) {
            inject(down, MotionEvent.ACTION_POINTER_UP | (i << MotionEvent.ACTION_POINTER_INDEX_SHIFT),
                    at, i + 1);
        }
        inject(down, MotionEvent.ACTION_UP, at, 1);
    }

    private static void inject(long down, int action, float[][] at, int count) throws Exception {
        MotionEvent.PointerProperties[] props = new MotionEvent.PointerProperties[count];
        MotionEvent.PointerCoords[] coords = new MotionEvent.PointerCoords[count];
        for (int i = 0; i < count; i++) {
            props[i] = new MotionEvent.PointerProperties();
            props[i].id = i;
            props[i].toolType = MotionEvent.TOOL_TYPE_FINGER;
            coords[i] = new MotionEvent.PointerCoords();
            coords[i].x = at[i][0];
            coords[i].y = at[i][1];
            coords[i].pressure = 1f;
            coords[i].size = 0.1f;
        }
        MotionEvent e = MotionEvent.obtain(down, SystemClock.uptimeMillis(), action, count, props,
                coords, 0, 0, 1f, 1f, 0, 0, InputDevice.SOURCE_TOUCHSCREEN, 0);
        sInject.invoke(sInput, e, 0);
        e.recycle();
    }

    private static float f(String s) {
        return Float.parseFloat(s);
    }
}
