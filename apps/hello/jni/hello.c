// JNI library for the sample app, built for x86_64 with the NDK.
#include <jni.h>
#include <stdio.h>
#include <sys/utsname.h>
#include <unistd.h>

#if !defined(__x86_64__)
#error "this sample is built for x86_64 only"
#endif

JNIEXPORT jstring JNICALL
Java_com_androidpc_hello_Native_info(JNIEnv *env, jclass cls) {
    (void)cls;
    struct utsname u;
    char buf[256];
    if (uname(&u) != 0) {
        snprintf(u.machine, sizeof u.machine, "?");
        snprintf(u.release, sizeof u.release, "?");
    }
    snprintf(buf, sizeof buf, "JNI: x86_64 native code\nmachine=%s kernel=%s\npid=%d",
             u.machine, u.release, (int)getpid());
    return (*env)->NewStringUTF(env, buf);
}

JNIEXPORT jlong JNICALL
Java_com_androidpc_hello_Native_sum(JNIEnv *env, jclass cls, jint n) {
    (void)env;
    (void)cls;
    jlong s = 0;
    for (jint i = 1; i <= n; i++) s += i;
    return s;
}
