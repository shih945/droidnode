package com.droidnode.brokers

import com.droidnode.data.AgentApi

import android.content.Context
import android.os.StatFs
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import org.json.JSONObject
import java.io.File

private const val TAG = "AgentStatusClient"
private const val PUSH_INTERVAL_MS = 10_000L
private const val RECONNECT_DELAY_MS = 3_000L
private const val READ_TIMEOUT_MS = 15_000

/**
 * E1/E2/E16: keeps a Unix-socket connection to the agent alive, pushes device
 * telemetry every 10 s and stores the structured status the agent sends back.
 *
 * The socket path is `<filesDir>/agent.sock` — app-private, so no extra
 * permission is needed and nothing outside the app can push fake telemetry.
 */
object AgentStatus {
    @Volatile var connected: Boolean = false
    @Volatile var podsRunning: Int = 0
    @Volatile var nodeName: String = ""
    @Volatile var apiserver: String = ""
    @Volatile var ready: Boolean = false
    @Volatile var restarts: Int = 0
    @Volatile var drained: Boolean = false
    @Volatile var maintenance: Boolean = false
    @Volatile var cordoned: Boolean = false
    @Volatile var kubeletPort: Int = 0
    @Volatile var updatedAt: Long = 0L
}

data class AgentStatusSnapshot(
    val nodeName: String,
    val apiserver: String,
    val ready: Boolean,
    val podsRunning: Int,
    val restarts: Int,
    val drained: Boolean,
    val maintenance: Boolean,
    val cordoned: Boolean,
    val kubeletPort: Int,
)

class AgentStatusClient(
    private val context: Context,
    private val batteryBroker: BatteryBroker,
    private val networkBroker: NetworkBroker,
    private val scope: CoroutineScope,
    private val onStatus: (AgentStatusSnapshot) -> Unit,
) {

    private var job: Job? = null

    fun start() {
        if (job?.isActive == true) return
        job = scope.launch(Dispatchers.IO) { runLoop() }
    }

    fun stop() {
        job?.cancel()
        job = null
        AgentStatus.connected = false
    }

    private suspend fun runLoop() {
        val socketPath = File(context.filesDir, "agent.sock").absolutePath
        while (currentCoroutineContext().isActive) {
            try {
                UnixSocketBroker(socketPath).use { socket ->
                    socket.connect()
                    socket.setReadTimeout(READ_TIMEOUT_MS)
                    Log.i(TAG, "connected to agent status socket")
                    while (currentCoroutineContext().isActive) {
                        socket.write((telemetryJson() + "\n").toByteArray())
                        val reply = socket.readLine() ?: break
                        handleReply(reply)
                        delay(PUSH_INTERVAL_MS)
                    }
                }
            } catch (e: Exception) {
                AgentStatus.connected = false
                Log.w(TAG, "status IPC error: ${e.message}")
            }
            delay(RECONNECT_DELAY_MS)
        }
    }

    private fun handleReply(line: String) {
        try {
            val o = JSONObject(line)
            val snapshot = AgentStatusSnapshot(
                nodeName = o.optString("node_name", ""),
                apiserver = o.optString("apiserver", ""),
                ready = o.optBoolean("ready", false),
                podsRunning = o.optInt("pods_running", 0),
                restarts = o.optInt("restarts", 0),
                drained = o.optBoolean("drained", false),
                maintenance = o.optBoolean("maintenance", false),
                cordoned = o.optBoolean("cordoned", false),
                kubeletPort = o.optInt("kubelet_port", 0),
            )
            AgentStatus.connected = true
            AgentStatus.nodeName = snapshot.nodeName
            AgentStatus.apiserver = snapshot.apiserver
            AgentStatus.ready = snapshot.ready
            AgentStatus.podsRunning = snapshot.podsRunning
            AgentStatus.restarts = snapshot.restarts
            AgentStatus.drained = snapshot.drained
            AgentStatus.maintenance = snapshot.maintenance
            AgentStatus.cordoned = snapshot.cordoned
            AgentStatus.kubeletPort = snapshot.kubeletPort
            // The agent is the source of truth for the port it actually bound.
            if (snapshot.kubeletPort > 0) AgentApi.port = snapshot.kubeletPort
            AgentStatus.updatedAt = System.currentTimeMillis()
            onStatus(snapshot)
        } catch (e: Exception) {
            Log.w(TAG, "bad agent status reply: ${e.message}")
        }
    }

    private fun telemetryJson(): String {
        val battery = batteryBroker.readState()
        val network = networkBroker.networkType.value.name.lowercase()
        val stat = StatFs(context.filesDir.path)
        val o = JSONObject()
        o.put("battery_percent", battery.percent)
        o.put("battery_charging", battery.charging)
        battery.temperatureC?.let { o.put("battery_temperature_c", it.toDouble()) }
        o.put("network_type", network)
        o.put("storage_total_bytes", stat.totalBytes)
        o.put("storage_available_bytes", stat.availableBytes)
        o.put("updated_at_ms", System.currentTimeMillis())
        return o.toString()
    }
}
