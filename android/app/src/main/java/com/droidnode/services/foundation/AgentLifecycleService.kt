package com.droidnode.services.foundation

import android.util.Log
import com.droidnode.LogBuffer
import com.droidnode.brokers.AGENT_STOP_GRACE_MILLIS
import com.droidnode.brokers.RustProcessBroker
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

private const val TAG = "AgentLifecycleService"

class AgentLifecycleService(
    private val processBroker: RustProcessBroker,
    private val onStatusChange: ((running: Boolean) -> Unit)? = null,
) {

    private var agentProcess: Process? = null

    suspend fun start(): Process = withContext(Dispatchers.IO) {
        if (agentProcess?.isAlive == true) {
            Log.d(TAG, "agent already running")
            return@withContext agentProcess!!
        }
        val proc = processBroker.spawn()
        agentProcess = proc
        Log.i(TAG, "agent started")
        onStatusChange?.invoke(true)

        Thread {
            try {
                proc.inputStream.bufferedReader().forEachLine { line ->
                    Log.i("DroidNodeRust", line)
                    LogBuffer.append(line)
                }
            } catch (_: Exception) {}
            val code = try { proc.exitValue() } catch (_: Exception) { -1 }
            Log.w(TAG, "agent process ended, exit code=$code")
            LogBuffer.append("agent process ended (exit=$code)")
            onStatusChange?.invoke(false)
        }.also { it.isDaemon = true; it.name = "agent-log-reader" }.start()

        // stderr 单独读：panic 信息和 anyhow 的错误链都走 stderr，只读 stdout 时
        // 进程异常退出会看不到任何原因（曾经的 exit code=101 就是这么丢的）。
        Thread {
            try {
                proc.errorStream.bufferedReader().forEachLine { line ->
                    Log.w("DroidNodeRustErr", line)
                    LogBuffer.append("[stderr] $line")
                }
            } catch (_: Exception) {}
        }.also { it.isDaemon = true; it.name = "agent-err-reader" }.start()

        proc
    }

    /**
     * 停止 agent。默认给 12 秒 SIGTERM 宽限（见 RustProcessBroker.kill）。
     * 调用方必须是 IO 线程；系统拆卸服务时用更短的宽限，避免阻塞主线程。
     */
    suspend fun stop(graceMillis: Long = AGENT_STOP_GRACE_MILLIS) = withContext(Dispatchers.IO) {
        processBroker.kill(graceMillis)
        agentProcess = null
        Log.i(TAG, "agent stopped")
        onStatusChange?.invoke(false)
    }

    suspend fun restart(): Process {
        stop()
        return start()
    }

    fun isRunning(): Boolean = processBroker.isAlive()

    fun exitCode(): Int? = processBroker.exitCode()
}
