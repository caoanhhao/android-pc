package com.androidpc.hello;

/** Bindings to the x86_64 JNI library built from jni/hello.c. */
public final class Native {
    static {
        System.loadLibrary("hello");
    }

    private Native() {}

    /** Machine, kernel release and pid as seen by native code. */
    public static native String info();

    /** Sum of 1..n computed natively. */
    public static native long sum(int n);

    /** The text the app shows; also what SelfTest prints. */
    public static String report() {
        return info() + "\nsum(1..100) = " + sum(100);
    }
}
