package com.androidpc.hello;

/**
 * Headless entry point: runs the app's dex and JNI code on ART without the
 * framework (no system_server, no install), via app_process. See README.
 */
public final class SelfTest {
    public static void main(String[] args) {
        String report = Native.report();
        System.out.println(report);
        boolean ok = Native.sum(100) == 5050 && report.contains("machine=x86_64");
        System.out.println(ok ? "HELLO_OK" : "HELLO_FAIL");
        System.exit(ok ? 0 : 1);
    }
}
