package com.droidnode.data

import android.content.Context
import com.droidnode.ui.v2.LocalConfig
import org.json.JSONObject
import java.io.File
import java.net.URL
import java.security.KeyStore
import java.security.SecureRandom
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import javax.net.ssl.HttpsURLConnection
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLSocketFactory
import javax.net.ssl.TrustManager
import javax.net.ssl.TrustManagerFactory
import javax.net.ssl.X509TrustManager

/**
 * agent 的 loopback 接口客户端（只读为主）。
 *
 * 端口由 E14 的配置决定（默认 10255），`DroidNodeApp` 在进程启动时写入。
 *
 * 证书信任：agent 用自己的 kubelet CA 签发 loopback 证书，App 只信任
 * `files/kubelet-ca.crt` 里那把 CA 签出的证书。之前是「全信任」，任何能抢占
 * 127.0.0.1:10255 的本地进程都能冒充 agent。没有 CA 文件时（全新安装、agent 还没
 * 起过）退回旧的宽松策略，保证首次启动可用。
 */
object AgentApi {

    /** Runtime kubelet port; must match what the agent process was started with. */
    @Volatile
    var port: Int = LocalConfig.DEFAULT_KUBELET_PORT

    @Volatile
    private var caFile: File? = null

    data class Response(val code: Int, val body: String)

    fun configure(context: Context) {
        port = LocalConfig.kubeletPort(context)
        caFile = File(context.filesDir, "kubelet-ca.crt").takeIf { it.isFile }
    }

    private val factory: SSLSocketFactory by lazy { buildFactory() }

    private fun buildFactory(): SSLSocketFactory {
        val ctx = SSLContext.getInstance("TLS")
        ctx.init(null, trustManagers(caFile), SecureRandom())
        return ctx.socketFactory
    }

    /** Trust only the kubelet CA when we have it; otherwise keep the lenient fallback. */
    private fun trustManagers(ca: File?): Array<TrustManager> {
        val fallback = arrayOf<TrustManager>(object : X509TrustManager {
            override fun checkClientTrusted(chain: Array<X509Certificate>, authType: String) = Unit
            override fun checkServerTrusted(chain: Array<X509Certificate>, authType: String) = Unit
            override fun getAcceptedIssuers(): Array<X509Certificate> = arrayOf()
        })
        if (ca == null) return fallback
        return try {
            val store = KeyStore.getInstance(KeyStore.getDefaultType()).apply { load(null, null) }
            val certs = CertificateFactory.getInstance("X.509")
                .generateCertificates(ca.inputStream())
            certs.forEachIndexed { index, cert -> store.setCertificateEntry("kubelet-ca-$index", cert) }
            val tmf = TrustManagerFactory.getInstance(TrustManagerFactory.getDefaultAlgorithm())
            tmf.init(store)
            tmf.trustManagers
        } catch (e: Exception) {
            fallback
        }
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
        // The serving cert's SAN does not include 127.0.0.1, so hostname checking is
        // skipped; the CA pin above is what prevents impersonation.
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
