package com.droidnode.services.orchestration

import android.util.Log
import com.droidnode.data.AppPrefs
import com.droidnode.services.foundation.AgentLifecycleService
import com.droidnode.services.foundation.ResourceGuardService
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import java.net.URL
import java.security.SecureRandom
import java.security.cert.X509Certificate
import javax.net.ssl.HttpsURLConnection
import javax.net.ssl.SSLContext
import javax.net.ssl.TrustManager
import javax.net.ssl.X509TrustManager

private const val TAG = "NodeReadinessService"
private const val MONITOR_INTERVAL_MS = 10_000L

/**
 * Orchestration service: combine battery + network + agent process state to decide
 * whether the node should be considered ready, and keep the agent alive.
 *
 * E7: on low battery we no longer kill the agent (that SIGKILLs every container).
 * Instead we ask it to **drain** — stop pods gracefully but keep reporting status —
 * so power recovery resumes instantly.
 */
class NodeReadinessService(
    private val agentLifecycle: AgentLifecycleService,
    private val resourceGuard: ResourceGuardService,
    /** Read per request: the kubelet port is configurable at runtime (E14). */
    private val kubeletPort: () -> Int = { AppPrefs.DEFAULT_KUBELET_PORT },
) {
    private var drained = false

    /// Consecutive failed starts (agent exits immediately, e.g. missing
    /// kubeconfig). Without backoff this loop respawned every 10 s forever.
    private var failedStarts = 0
    private var nextStartAtMs = 0L

    suspend fun runLoop() {
        Log.i(TAG, "node readiness monitor started")

        while (true) {
            val readiness = resourceGuard.evaluate()

            if (!readiness.ready) {
                Log.w(TAG, "node not ready: ${readiness.reason}")
                if (agentLifecycle.isRunning() && !drained) {
                    Log.i(TAG, "draining node instead of killing the agent")
                    drained = postDrain(true)
                }
            } else {
                if (drained) {
                    Log.i(TAG, "resources recovered — undraining node")
                    drained = !postDrain(false)
                }
                val running = agentLifecycle.isRunning()
                if (!running) {
                    val now = System.currentTimeMillis()
                    if (now >= nextStartAtMs) {
                        // Count a failure only when we are actually about to
                        // retry; otherwise a stale exit code inflates the
                        // counter on every 10 s tick while waiting.
                        if (agentLifecycle.exitCode() != null) {
                            failedStarts += 1
                        }
                        Log.i(TAG, "agent not running — starting (failed=$failedStarts)")
                        try {
                            agentLifecycle.start()
                        } catch (e: Exception) {
                            Log.e(TAG, "failed to start agent: ${e.message}")
                        }
                        nextStartAtMs = now + backoffMs(failedStarts)
                    }
                } else {
                    // Alive: reset the failure counter and any pending backoff.
                    failedStarts = 0
                    nextStartAtMs = 0L
                }
            }

            delay(MONITOR_INTERVAL_MS)
        }
    }

    private fun backoffMs(failedStarts: Int): Long {
        val shift = failedStarts.coerceIn(0, 5)
        return (10_000L shl shift).coerceAtMost(300_000L)
    }

    fun isNodeReady(): Boolean {
        val readiness = resourceGuard.evaluate()
        return readiness.ready && agentLifecycle.isRunning() && !drained
    }

    /** POST /drain or /undrain to the agent's loopback API. */
    private suspend fun postDrain(drain: Boolean): Boolean = withContext(Dispatchers.IO) {
        try {
            val ctx = SSLContext.getInstance("TLS")
            ctx.init(null, arrayOf<TrustManager>(object : X509TrustManager {
                override fun checkClientTrusted(chain: Array<X509Certificate>, authType: String) {}
                override fun checkServerTrusted(chain: Array<X509Certificate>, authType: String) {}
                override fun getAcceptedIssuers(): Array<X509Certificate> = arrayOf()
            }), SecureRandom())

            val path = if (drain) "/drain" else "/undrain"
            val conn = URL("https://127.0.0.1:${kubeletPort()}$path").openConnection() as HttpsURLConnection
            conn.sslSocketFactory = ctx.socketFactory
            conn.setHostnameVerifier { _, _ -> true }
            conn.connectTimeout = 2000
            conn.readTimeout = 3000
            conn.requestMethod = "POST"
            conn.doOutput = true
            conn.outputStream.use { it.write(ByteArray(0)) }
            val code = conn.responseCode
            conn.disconnect()
            Log.i(TAG, "POST $path -> HTTP $code")
            code in 200..299
        } catch (e: Exception) {
            Log.w(TAG, "drain request failed: ${e.message}")
            false
        }
    }
}
