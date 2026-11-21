package com.android.hyprlua

import java.io.Closeable

/** An error raised by Lua code or by the sandbox (instruction limit, memory). */
class LuaException(@JvmField val utf8: ByteArray) : RuntimeException(String(utf8, Charsets.UTF_8)) {
    constructor(message: String) : this(message.toByteArray(Charsets.UTF_8))
}

/**
 * One sandboxed Lua 5.4 state (libhyprlua_jni, on external/lua): base, coroutine, table, string,
 * math and utf8 only, no os, io, debug or package; `load` and `require` come from the prelude
 * and read text only. Every [run] and [call] gets an instruction budget and a time limit, and
 * the state's memory is capped. The script reaches the host through `__host(op, ...)`, which
 * calls [host]. Not thread-safe: one thread uses a state.
 */
class LuaState(
    private val onHost: (op: String, args: List<String?>) -> String?,
    memoryLimit: Long = DEFAULT_MEMORY,
) : Closeable {
    private var ptr: Long

    init {
        loadLibrary()
        ptr = nativeOpen(memoryLimit)
        check(ptr != 0L) { "lua state could not be created" }
    }

    /** Runs [code] (text only) as the chunk [name] (`@init.lua` shows as `init.lua:LINE:`). */
    fun run(code: String, name: String, budget: Long = DEFAULT_BUDGET, timeoutMs: Long = DEFAULT_TIMEOUT_MS) {
        check(ptr != 0L) { "closed" }
        nativeRun(ptr, code.toByteArray(Charsets.UTF_8), name.toByteArray(Charsets.UTF_8), budget, timeoutMs)
    }

    /** Calls the global function [fn] with [args]; returns its first result as a string. */
    fun call(fn: String, vararg args: String?, budget: Long = DEFAULT_BUDGET, timeoutMs: Long = DEFAULT_TIMEOUT_MS): String? {
        check(ptr != 0L) { "closed" }
        val a = Array(args.size) { args[it]?.toByteArray(Charsets.UTF_8) }
        return nativeCall(ptr, fn.toByteArray(Charsets.UTF_8), a, budget, timeoutMs)
            ?.toString(Charsets.UTF_8)
    }

    /** Bytes the state has allocated. */
    val memory: Long get() = if (ptr == 0L) 0 else nativeMemory(ptr)

    override fun close() {
        if (ptr != 0L) nativeClose(ptr)
        ptr = 0
    }

    /** Called from native code for `__host(op, ...)`; any failure becomes a Lua error. */
    @Suppress("unused")
    private fun host(args: Array<ByteArray?>): ByteArray? {
        try {
            val strings = args.map { it?.toString(Charsets.UTF_8) }
            val op = strings.firstOrNull() ?: throw LuaException("no operation")
            return onHost(op, strings.drop(1))?.toByteArray(Charsets.UTF_8)
        } catch (e: LuaException) {
            throw e
        } catch (e: Throwable) {
            throw LuaException(e.message ?: e.toString())
        }
    }

    private external fun nativeOpen(memoryLimit: Long): Long
    private external fun nativeClose(ptr: Long)
    private external fun nativeRun(ptr: Long, code: ByteArray, chunk: ByteArray, budget: Long, timeoutMs: Long)
    private external fun nativeCall(
        ptr: Long, fn: ByteArray, args: Array<ByteArray?>, budget: Long, timeoutMs: Long,
    ): ByteArray?
    private external fun nativeMemory(ptr: Long): Long

    companion object {
        /** Instructions per entry: init.lua and its requires together, or one hook call. */
        const val DEFAULT_BUDGET = 20_000_000L
        const val DEFAULT_TIMEOUT_MS = 2000L
        const val DEFAULT_MEMORY = 32L shl 20

        /**
         * Where to load libhyprlua_jni from when the class loader does not find it, e.g.
         * `/system_ext/priv-app/HyprLauncher/HyprLauncher.apk!/lib/x86_64/libhyprlua_jni.so`:
         * PackageManager keeps a system app's native ABI from its last scan until the build
         * fingerprint changes, so an APK that newly carries the library is not given it.
         */
        @Volatile
        var fallbackPath: String? = null

        @Volatile
        private var loaded = false

        private fun loadLibrary() {
            if (loaded) return
            synchronized(this) {
                if (loaded) return
                try {
                    System.loadLibrary("hyprlua_jni")
                } catch (e: UnsatisfiedLinkError) {
                    System.load(fallbackPath ?: throw e)
                }
                loaded = true
            }
        }
    }
}
