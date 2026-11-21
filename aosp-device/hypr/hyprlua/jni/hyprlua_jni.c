/*
 * hyprlua: a sandboxed Lua 5.4 state for HyprLauncher's configuration (Phase D5,
 * docs/hypr-lua.md), on the tree's external/lua.
 *
 * Only the base, coroutine, table, string, math and utf8 libraries are opened; os, io, debug
 * and package (loadlib) never are. Every entry from Java runs under an instruction budget and
 * a wall-clock deadline (a count hook raises an error once either is spent, and from then on
 * after every instruction, so a pcall in the script cannot swallow it), and all allocations
 * of the state are capped. The script reaches Java through one function, __host(op, ...),
 * which calls LuaState.host(String, String[]) and returns its string (or raises its error).
 */

#include <jni.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "lauxlib.h"
#include "lua.h"
#include "lualib.h"

#define HOOK_STEP 1000
#define MAX_HOST_ARGS 64

typedef struct {
    JNIEnv *env;         /* of the current entry from Java */
    jobject self;        /* global ref to the LuaState */
    size_t used;         /* bytes allocated */
    size_t limit;        /* allocation cap */
    long long remaining; /* instructions left in the current entry */
    long long deadline_ns;
    int exhausted;
} Sandbox;

static jmethodID g_host;
static jclass g_bytes;
static jclass g_lua_exception;
static jmethodID g_lua_exception_init;

/*
 * Strings cross JNI as UTF-8 byte arrays, never as jstring: a Lua string can hold any bytes,
 * and NewStringUTF aborts the process (CheckJNI) on what is not modified UTF-8.
 */
static jbyteArray to_bytes(JNIEnv *env, const char *s, size_t len) {
    jbyteArray a = (*env)->NewByteArray(env, (jsize)len);
    if (a) (*env)->SetByteArrayRegion(env, a, 0, (jsize)len, (const jbyte *)s);
    return a;
}

/* Pushes the bytes of [a] as a Lua string. */
static void push_bytes(JNIEnv *env, lua_State *L, jbyteArray a) {
    jsize len = (*env)->GetArrayLength(env, a);
    jbyte *b = (*env)->GetByteArrayElements(env, a, NULL);
    lua_pushlstring(L, (const char *)b, (size_t)len);
    (*env)->ReleaseByteArrayElements(env, a, b, JNI_ABORT);
}

static long long now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

static void *sandbox_alloc(void *ud, void *ptr, size_t osize, size_t nsize) {
    Sandbox *sb = ud;
    size_t old = ptr ? osize : 0;
    if (nsize == 0) {
        free(ptr);
        sb->used -= old;
        return NULL;
    }
    if (nsize > old && sb->used - old + nsize > sb->limit) return NULL;
    void *p = realloc(ptr, nsize);
    if (p) sb->used = sb->used - old + nsize;
    return p;
}

static Sandbox *sandbox_of(lua_State *L) {
    void *ud;
    lua_getallocf(L, &ud);
    return ud;
}

static void budget_hook(lua_State *L, lua_Debug *ar) {
    (void)ar;
    Sandbox *sb = sandbox_of(L);
    sb->remaining -= HOOK_STEP;
    if (sb->remaining < 0 || sb->exhausted || now_ns() > sb->deadline_ns) {
        if (!sb->exhausted) {
            sb->exhausted = 1;
            /* From now on every instruction raises, so the error reaches the entry point. */
            lua_sethook(L, budget_hook, LUA_MASKCOUNT, 1);
        }
        luaL_error(L, "instruction limit exceeded");
    }
}

/* __host(op, ...): strings (or numbers, booleans, nil) to LuaState.host; returns its string. */
static int l_host(lua_State *L) {
    Sandbox *sb = sandbox_of(L);
    JNIEnv *env = sb->env;
    int n = lua_gettop(L);
    if (n < 1) return luaL_error(L, "__host needs an operation");
    if (n > MAX_HOST_ARGS) return luaL_error(L, "too many arguments");
    for (int i = 1; i <= n; i++) {
        if (!lua_isnil(L, i) && !lua_isboolean(L, i) && !lua_isstring(L, i))
            return luaL_error(L, "argument %d: expected a string, got %s", i, luaL_typename(L, i));
    }
    jobjectArray args = (*env)->NewObjectArray(env, n, g_bytes, NULL);
    if (!args) {
        (*env)->ExceptionClear(env);
        return luaL_error(L, "out of memory");
    }
    for (int i = 1; i <= n; i++) {
        if (lua_isnil(L, i)) continue;
        size_t len;
        const char *s;
        if (lua_isboolean(L, i)) {
            s = lua_toboolean(L, i) ? "true" : "false";
            len = strlen(s);
        } else {
            s = lua_tolstring(L, i, &len);
        }
        jbyteArray b = to_bytes(env, s, len);
        (*env)->SetObjectArrayElement(env, args, i - 1, b);
        (*env)->DeleteLocalRef(env, b);
    }
    jbyteArray result = (*env)->CallObjectMethod(env, sb->self, g_host, args);
    (*env)->DeleteLocalRef(env, args);
    jthrowable ex = (*env)->ExceptionOccurred(env);
    if (ex) {
        (*env)->ExceptionClear(env);
        /* LuaState.host wraps every failure in a LuaException whose bytes are the message. */
        jclass cls = (*env)->GetObjectClass(env, ex);
        jfieldID f = (*env)->GetFieldID(env, cls, "utf8", "[B");
        if ((*env)->ExceptionCheck(env)) (*env)->ExceptionClear(env);
        jbyteArray m = f ? (*env)->GetObjectField(env, ex, f) : NULL;
        if (m) {
            push_bytes(env, L, m);
            (*env)->DeleteLocalRef(env, m);
        } else {
            lua_pushstring(L, "host error");
        }
        (*env)->DeleteLocalRef(env, cls);
        (*env)->DeleteLocalRef(env, ex);
        return lua_error(L);
    }
    if (!result) {
        lua_pushnil(L);
        return 1;
    }
    push_bytes(env, L, result);
    (*env)->DeleteLocalRef(env, result);
    return 1;
}

static int traceback(lua_State *L) {
    const char *m = lua_tostring(L, 1);
    if (!m) m = luaL_tolstring(L, 1, NULL);
    luaL_traceback(L, L, m, 1);
    return 1;
}

static void throw_lua(JNIEnv *env, const char *msg, size_t len) {
    if (!msg) {
        msg = "lua error";
        len = strlen(msg);
    }
    jbyteArray b = to_bytes(env, msg, len);
    jobject ex = (*env)->NewObject(env, g_lua_exception, g_lua_exception_init, b);
    if (ex) (*env)->Throw(env, ex);
}

static void throw_top(JNIEnv *env, lua_State *L) {
    size_t len = 0;
    const char *m = lua_tolstring(L, -1, &len);
    throw_lua(env, m, len);
}

static void begin(JNIEnv *env, lua_State *L, jlong budget, jlong timeout_ms) {
    Sandbox *sb = sandbox_of(L);
    sb->env = env;
    sb->remaining = budget;
    sb->deadline_ns = now_ns() + (long long)timeout_ms * 1000000LL;
    sb->exhausted = 0;
    lua_sethook(L, budget_hook, LUA_MASKCOUNT, HOOK_STEP);
}

/* Runs the function below its arguments on the stack in protected mode; throws on error. */
static int protected_call(JNIEnv *env, lua_State *L, int nargs, int nresults) {
    int base = lua_gettop(L) - nargs;
    lua_pushcfunction(L, traceback);
    lua_insert(L, base);
    int status = lua_pcall(L, nargs, nresults, base);
    lua_remove(L, base);
    if (status != LUA_OK) {
        throw_top(env, L);
        lua_pop(L, 1);
        return 0;
    }
    return 1;
}

static int open_libs(lua_State *L) {
    static const luaL_Reg libs[] = {
        {LUA_GNAME, luaopen_base},       {LUA_COLIBNAME, luaopen_coroutine},
        {LUA_TABLIBNAME, luaopen_table}, {LUA_STRLIBNAME, luaopen_string},
        {LUA_MATHLIBNAME, luaopen_math}, {LUA_UTF8LIBNAME, luaopen_utf8},
        {NULL, NULL},
    };
    for (const luaL_Reg *lib = libs; lib->func; lib++) {
        luaL_requiref(L, lib->name, lib->func, 1);
        lua_pop(L, 1);
    }
    /* os.time, os.date and os.clock only, as __clock: the prelude moves them to hl. */
    luaL_requiref(L, "__os", luaopen_os, 0);
    lua_newtable(L);
    const char *keep[] = {"time", "date", "clock", NULL};
    for (int i = 0; keep[i]; i++) {
        lua_getfield(L, -2, keep[i]);
        lua_setfield(L, -2, keep[i]);
    }
    lua_setglobal(L, "__clock");
    lua_pop(L, 1);
    /* No file or bytecode loading: load() is wrapped to text mode by the prelude. */
    const char *drop[] = {"dofile", "loadfile", NULL};
    for (int i = 0; drop[i]; i++) {
        lua_pushnil(L);
        lua_setglobal(L, drop[i]);
    }
    lua_getglobal(L, LUA_STRLIBNAME);
    lua_pushnil(L);
    lua_setfield(L, -2, "dump");
    lua_pop(L, 1);
    lua_pushcfunction(L, l_host);
    lua_setglobal(L, "__host");
    return 0;
}

JNIEXPORT jlong JNICALL Java_com_android_hyprlua_LuaState_nativeOpen(
        JNIEnv *env, jobject self, jlong memory_limit) {
    Sandbox *sb = calloc(1, sizeof(Sandbox));
    if (!sb) return 0;
    sb->limit = (size_t)memory_limit;
    sb->env = env;
    lua_State *L = lua_newstate(sandbox_alloc, sb);
    if (!L) {
        free(sb);
        return 0;
    }
    sb->self = (*env)->NewGlobalRef(env, self);
    begin(env, L, 10000000, 5000);
    lua_pushcfunction(L, open_libs);
    if (!protected_call(env, L, 0, 0)) {
        lua_close(L);
        (*env)->DeleteGlobalRef(env, sb->self);
        free(sb);
        return 0;
    }
    return (jlong)(intptr_t)L;
}

JNIEXPORT void JNICALL Java_com_android_hyprlua_LuaState_nativeClose(
        JNIEnv *env, jobject self, jlong ptr) {
    (void)self;
    lua_State *L = (lua_State *)(intptr_t)ptr;
    if (!L) return;
    Sandbox *sb = sandbox_of(L);
    sb->env = env;
    lua_close(L);
    (*env)->DeleteGlobalRef(env, sb->self);
    free(sb);
}

/* Compiles [code] as text (never bytecode) under [chunk] and runs it. */
JNIEXPORT void JNICALL Java_com_android_hyprlua_LuaState_nativeRun(
        JNIEnv *env, jobject self, jlong ptr, jbyteArray code, jbyteArray chunk, jlong budget,
        jlong timeout_ms) {
    (void)self;
    lua_State *L = (lua_State *)(intptr_t)ptr;
    begin(env, L, budget, timeout_ms);
    push_bytes(env, L, chunk);
    const char *name = lua_tostring(L, -1);
    jsize len = (*env)->GetArrayLength(env, code);
    jbyte *c = (*env)->GetByteArrayElements(env, code, NULL);
    int status = luaL_loadbufferx(L, (const char *)c, (size_t)len, name, "t");
    (*env)->ReleaseByteArrayElements(env, code, c, JNI_ABORT);
    if (status != LUA_OK) {
        throw_top(env, L);
        lua_settop(L, 0);
        return;
    }
    lua_remove(L, -2);
    protected_call(env, L, 0, 0);
    lua_settop(L, 0);
}

/* Calls the global function [fn] with string arguments; returns its first result as a string. */
JNIEXPORT jbyteArray JNICALL Java_com_android_hyprlua_LuaState_nativeCall(
        JNIEnv *env, jobject self, jlong ptr, jbyteArray fn, jobjectArray args, jlong budget,
        jlong timeout_ms) {
    (void)self;
    lua_State *L = (lua_State *)(intptr_t)ptr;
    begin(env, L, budget, timeout_ms);
    jsize n = args ? (*env)->GetArrayLength(env, args) : 0;
    if (!lua_checkstack(L, n + 3)) {
        throw_lua(env, "stack overflow", 14);
        return NULL;
    }
    lua_pushglobaltable(L);
    push_bytes(env, L, fn);
    lua_rawget(L, -2);
    lua_remove(L, -2);
    if (!lua_isfunction(L, -1)) {
        lua_settop(L, 0);
        throw_lua(env, "no such function", 16);
        return NULL;
    }
    for (jsize i = 0; i < n; i++) {
        jbyteArray a = (*env)->GetObjectArrayElement(env, args, i);
        if (!a) {
            lua_pushnil(L);
            continue;
        }
        push_bytes(env, L, a);
        (*env)->DeleteLocalRef(env, a);
    }
    jbyteArray out = NULL;
    if (protected_call(env, L, n, 1)) {
        size_t len;
        const char *r = lua_tolstring(L, -1, &len);
        if (r) out = to_bytes(env, r, len);
    }
    lua_settop(L, 0);
    return out;
}

JNIEXPORT jlong JNICALL Java_com_android_hyprlua_LuaState_nativeMemory(
        JNIEnv *env, jobject self, jlong ptr) {
    (void)env;
    (void)self;
    return (jlong)sandbox_of((lua_State *)(intptr_t)ptr)->used;
}

JNIEXPORT jint JNI_OnLoad(JavaVM *vm, void *reserved) {
    (void)reserved;
    JNIEnv *env;
    if ((*vm)->GetEnv(vm, (void **)&env, JNI_VERSION_1_6) != JNI_OK) return JNI_ERR;
    jclass state = (*env)->FindClass(env, "com/android/hyprlua/LuaState");
    if (!state) return JNI_ERR;
    g_host = (*env)->GetMethodID(env, state, "host", "([[B)[B");
    g_bytes = (*env)->NewGlobalRef(env, (*env)->FindClass(env, "[B"));
    g_lua_exception = (*env)->NewGlobalRef(env, (*env)->FindClass(env, "com/android/hyprlua/LuaException"));
    if (!g_host || !g_bytes || !g_lua_exception) return JNI_ERR;
    g_lua_exception_init = (*env)->GetMethodID(env, g_lua_exception, "<init>", "([B)V");
    if (!g_lua_exception_init) return JNI_ERR;
    return JNI_VERSION_1_6;
}
