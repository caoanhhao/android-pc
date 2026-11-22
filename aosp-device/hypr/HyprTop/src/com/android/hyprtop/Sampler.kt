package com.android.hyprtop

import android.net.LocalSocket
import android.net.LocalSocketAddress
import android.net.TrafficStats
import android.os.StatFs
import android.os.SystemClock
import android.system.Os
import android.system.OsConstants
import android.util.Log
import java.io.File

/** One process row, as btop's process box shows it. */
data class Proc(
    val pid: Int,
    val uid: Int,
    val name: String,
    val state: Char,
    val threads: Int,
    val rssKb: Long,
    /** Share of all CPUs since the last sample, 0..100. */
    val cpu: Float,
)

data class Disk(val name: String, val totalBytes: Long, val freeBytes: Long)

/** Everything one refresh draws. */
class Snapshot(
    /** Busy share per core since the last sample, 0..1. */
    val cores: FloatArray,
    val total: Float,
    /** Current frequency per core in MHz, 0 when unknown. */
    val freqMhz: IntArray,
    val memTotalKb: Long,
    val memAvailKb: Long,
    val memFreeKb: Long,
    val cachedKb: Long,
    val swapTotalKb: Long,
    val swapFreeKb: Long,
    val loadAvg: String,
    val uptimeSec: Long,
    val tempC: Int?,
    val rxBytesPerSec: Long,
    val txBytesPerSec: Long,
    val rxTotal: Long,
    val txTotal: Long,
    val disks: List<Disk>,
    val procs: List<Proc>,
)

/**
 * Reads /proc and /sys once per refresh and turns counters into rates. Not thread-safe: the
 * activity calls [sample] from one background thread.
 */
class Sampler {
    private var lastCpu: List<LongArray> = emptyList()
    private val lastProcTicks = HashMap<Int, Long>()
    private var lastTotalTicks = 0L
    private var lastNet: Triple<Long, Long, Long>? = null
    private val tempPath: File? = findTemperatureSensor()
    private val pageKb = Os.sysconf(OsConstants._SC_PAGESIZE) / 1024
    private var daemon: LocalSocket? = null
    private var daemonReader: java.io.BufferedReader? = null

    fun sample(): Snapshot {
        val cpu = readCpu()
        val (cores, total) = cpuShares(cpu)
        val totalTicks = cpu.firstOrNull()?.sum() ?: 0L
        val procs = readProcs(totalTicks - lastTotalTicks)
        lastTotalTicks = totalTicks
        lastCpu = cpu

        val mem = readKeyValues("/proc/meminfo")
        val now = SystemClock.elapsedRealtime()
        val (rx, tx) = netTotals()
        val (rxRate, txRate) = lastNet?.let { (t, r, x) ->
            val dt = (now - t).coerceAtLeast(1)
            ((rx - r) * 1000 / dt).coerceAtLeast(0) to ((tx - x) * 1000 / dt).coerceAtLeast(0)
        } ?: (0L to 0L)
        lastNet = Triple(now, rx, tx)

        return Snapshot(
            cores = cores,
            total = total,
            freqMhz = IntArray(cores.size) { readFreqMhz(it) },
            memTotalKb = mem["MemTotal"] ?: 0,
            memAvailKb = mem["MemAvailable"] ?: 0,
            memFreeKb = mem["MemFree"] ?: 0,
            cachedKb = (mem["Cached"] ?: 0) + (mem["Buffers"] ?: 0),
            swapTotalKb = mem["SwapTotal"] ?: 0,
            swapFreeKb = mem["SwapFree"] ?: 0,
            loadAvg = readText("/proc/loadavg")?.split(' ')?.take(3)?.joinToString(" ") ?: "?",
            uptimeSec = SystemClock.elapsedRealtime() / 1000,
            tempC = tempPath?.let { readText(it.path)?.trim()?.toIntOrNull()?.div(1000) },
            rxBytesPerSec = rxRate,
            txBytesPerSec = txRate,
            rxTotal = rx,
            txTotal = tx,
            disks = listOf("/data" to "data", "/system" to "system").mapNotNull { (path, name) ->
                runCatching { StatFs(path) }.getOrNull()?.let { Disk(name, it.totalBytes, it.availableBytes) }
            },
            procs = procs,
        )
    }

    /** `/proc/stat` jiffies: index 0 is the `cpu` total line, then one array per core. */
    private fun readCpu(): List<LongArray> =
        readText("/proc/stat")?.lineSequence()
            ?.filter { it.startsWith("cpu") }
            ?.map { line -> line.split(' ').filter { it.isNotEmpty() }.drop(1).map { it.toLong() }.toLongArray() }
            ?.toList() ?: emptyList()

    private fun cpuShares(cpu: List<LongArray>): Pair<FloatArray, Float> {
        fun share(now: LongArray, before: LongArray?): Float {
            if (before == null) return 0f
            val idle = { a: LongArray -> a[3] + a.getOrElse(4) { 0 } }
            val dTotal = now.sum() - before.sum()
            val dIdle = idle(now) - idle(before)
            return if (dTotal <= 0) 0f else ((dTotal - dIdle).toFloat() / dTotal).coerceIn(0f, 1f)
        }
        val cores = FloatArray((cpu.size - 1).coerceAtLeast(0)) { share(cpu[it + 1], lastCpu.getOrNull(it + 1)) }
        return cores to (cpu.firstOrNull()?.let { share(it, lastCpu.firstOrNull()) } ?: 0f)
    }

    private class Raw(val pid: Int, val uid: Int, val ticks: Long, val threads: Int, val rssPages: Long,
        val state: Char, val name: String)

    private fun readProcs(dTotalTicks: Long): List<Proc> {
        val raws = fromDaemon() ?: fromProc()
        val seen = HashSet<Int>()
        val procs = raws.map { r ->
            val before = lastProcTicks.put(r.pid, r.ticks)
            seen += r.pid
            val cpu = if (before == null || dTotalTicks <= 0) 0f
                else ((r.ticks - before).toFloat() * 100f / dTotalTicks).coerceIn(0f, 100f)
            Proc(r.pid, r.uid, r.name, r.state, r.threads, r.rssPages * pageKb, cpu)
        }
        lastProcTicks.keys.retainAll(seen)
        return procs
    }

    /**
     * Every process, from hyprtopd (SELinux keeps apps out of other domains' /proc files): one
     * byte asks, `pid uid ticks threads rss_pages state name` lines answer, `.` ends them.
     */
    private fun fromDaemon(): List<Raw>? = try {
        val socket = daemon ?: LocalSocket().also {
            it.connect(LocalSocketAddress("hyprtopd", LocalSocketAddress.Namespace.RESERVED))
            daemon = it
            daemonReader = it.inputStream.bufferedReader()
        }
        socket.outputStream.write('\n'.code)
        val rows = ArrayList<Raw>()
        while (true) {
            val line = daemonReader?.readLine() ?: throw java.io.EOFException()
            if (line == ".") break
            val f = line.split(' ', limit = 7)
            if (f.size < 7) continue
            rows += Raw(f[0].toInt(), f[1].toInt(), f[2].toLong(), f[3].toInt(), f[4].toLong(),
                f[5].firstOrNull() ?: '?', f[6])
        }
        rows
    } catch (e: Exception) {
        if (daemon != null) Log.w(TAG, "hyprtopd: $e")
        runCatching { daemon?.close() }
        daemon = null
        daemonReader = null
        null
    }

    /** Without hyprtopd: what /proc shows this app (its own uid's processes). */
    private fun fromProc(): List<Raw> = File("/proc").list().orEmpty().mapNotNull { entry ->
        val pid = entry.toIntOrNull() ?: return@mapNotNull null
        val stat = readText("/proc/$pid/stat") ?: return@mapNotNull null
        val open = stat.indexOf('(')
        val close = stat.lastIndexOf(')')
        if (open < 0 || close < open) return@mapNotNull null
        // Fields after the command name start at field 3 (state).
        val f = stat.substring(close + 2).split(' ')
        if (f.size < 22) return@mapNotNull null
        val cmdline = readText("/proc/$pid/cmdline")?.substringBefore('\u0000')?.takeIf { it.isNotBlank() }
        Raw(pid, runCatching { Os.stat("/proc/$pid").st_uid }.getOrDefault(-1),
            f[11].toLong() + f[12].toLong(), f[17].toIntOrNull() ?: 0, f[21].toLong(),
            f[0].firstOrNull() ?: '?', cmdline?.substringAfterLast('/') ?: stat.substring(open + 1, close))
    }

    /** Bytes received and sent by every interface but loopback since boot. */
    private fun netTotals(): Pair<Long, Long> {
        val rx = TrafficStats.getTotalRxBytes()
        val tx = TrafficStats.getTotalTxBytes()
        if (rx > 0 || tx > 0) return rx.coerceAtLeast(0) to tx.coerceAtLeast(0)
        // Without the eBPF traffic counters (Waydroid) TrafficStats has nothing: the kernel's.
        var r = 0L
        var t = 0L
        readText("/proc/net/dev")?.lineSequence()?.drop(2)?.forEach { line ->
            val name = line.substringBefore(':').trim()
            if (name.isEmpty() || name == "lo") return@forEach
            val f = line.substringAfter(':').trim().split(Regex("\\s+"))
            r += f.getOrNull(0)?.toLongOrNull() ?: 0
            t += f.getOrNull(8)?.toLongOrNull() ?: 0
        }
        return r to t
    }

    private fun readFreqMhz(core: Int): Int =
        readText("/sys/devices/system/cpu/cpu$core/cpufreq/scaling_cur_freq")?.trim()?.toIntOrNull()?.div(1000) ?: 0

    private fun findTemperatureSensor(): File? {
        val zones = File("/sys/class/thermal").listFiles { f -> f.name.startsWith("thermal_zone") } ?: return null
        val preferred = listOf("x86_pkg_temp", "cpu", "soc", "acpitz")
        return preferred.firstNotNullOfOrNull { want ->
            zones.firstOrNull { readText("${it.path}/type")?.trim()?.startsWith(want) == true }
        }?.let { File(it, "temp") }?.takeIf { readText(it.path) != null }
    }

    private fun readKeyValues(path: String): Map<String, Long> =
        readText(path)?.lineSequence()?.mapNotNull { line ->
            val key = line.substringBefore(':', "").takeIf { it.isNotEmpty() } ?: return@mapNotNull null
            line.substringAfter(':').trim().substringBefore(' ').toLongOrNull()?.let { key to it }
        }?.toMap() ?: emptyMap()

    private fun readText(path: String): String? = try {
        File(path).readText()
    } catch (e: Exception) {
        null
    }

    private companion object {
        const val TAG = "HyprTop"
    }
}
