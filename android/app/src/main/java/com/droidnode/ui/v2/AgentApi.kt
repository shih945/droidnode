package com.droidnode.ui.v2

import org.json.JSONObject
import java.net.URL
import java.security.SecureRandom
import java.security.cert.X509Certificate
import javax.net.ssl.HttpsURLConnection
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLSocketFactory
import javax.net.ssl.TrustManager
import javax.net.ssl.X509TrustManager

/**
 * agent 的 loopback 接口客户端（只读为主）。
 *
 * 端口由 E14 的配置决定（默认 10255），`DroidNodeApp` 在进程启动时写入；
 * 证书是设备自己生成的，访问范围仅限本进程，所以这里沿用旧界面的宽松信任策略。
 */
object AgentApi {

    /** Runtime kubelet port; must match what the agent process was started with. */
    @Volatile
    var port: Int = LocalConfig.DEFAULT_KUBELET_PORT

    data class Response(val code: Int, val body: String)

    private val factory: SSLSocketFactory by lazy {
        val ctx = SSLContext.getInstance("TLS")
        ctx.init(
            null,
            arrayOf<TrustManager>(object : X509TrustManager {
                override fun checkClientTrusted(chain: Array<X509Certificate>, authType: String) = Unit
                override fun checkServerTrusted(chain: Array<X509Certificate>, authType: String) = Unit
                override fun getAcceptedIssuers(): Array<X509Certificate> = arrayOf()
            }),
            SecureRandom(),
        )
        ctx.socketFactory
    }

    fun get(path: String, timeoutMs: Int = 2500): Response? = request("GET", path, timeoutMs)

    fun post(path: String, timeoutMs: Int = 5000): Response? = request("POST", path, timeoutMs)

    fun json(path: String, timeoutMs: Int = 2500): JSONObject? =
        get(path, timeoutMs)?.takeIf { it.code in 200..299 }?.let {
            try {
                JSONObject(it.body)
            } catch (e: Exception) {
                null
            }
        }

    fun text(path: String, timeoutMs: Int = 2500): String? =
        get(path, timeoutMs)?.takeIf { it.code in 200..299 }?.body

    private fun request(method: String, path: String, timeoutMs: Int): Response? = try {
        val conn = URL("https://127.0.0.1:$port$path").openConnection() as HttpsURLConnection
        conn.sslSocketFactory = factory
        conn.setHostnameVerifier { _, _ -> true }
        conn.connectTimeout = timeoutMs
        conn.readTimeout = timeoutMs
        conn.requestMethod = method
        if (method == "POST") {
            conn.doOutput = true
            conn.outputStream.use { it.write(ByteArray(0)) }
        }
        val code = conn.responseCode
        val stream = if (code in 200..299) conn.inputStream else conn.errorStream
        val body = stream?.bufferedReader()?.use { it.readText() } ?: ""
        conn.disconnect()
        Response(code, body)
    } catch (e: Exception) {
        null
    }
}
