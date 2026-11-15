package com.droidnode.data

import android.content.Context
import android.content.SharedPreferences
import android.util.Log
import com.droidnode.ui.v2.LocalConfig
import org.json.JSONObject
import java.net.URL
import java.security.MessageDigest
import java.security.SecureRandom
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import javax.net.ssl.HttpsURLConnection
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLSocketFactory
import javax.net.ssl.TrustManager
import javax.net.ssl.X509TrustManager

/**
 * agent 的 loopback 接口客户端（只读为主）。
 *
 * 端口由 E14 的配置决定（默认 10255），`DroidNodeApp` 在进程启动时写入。
 *
 * 证书信任：改用 **首用即固定（TOFU）**。第一次连接时记录 serving 证书的
 * SHA-256 指纹，之后只接受同一张证书。本地 `kubelet-ca.crt` 与集群 CSR 签发的
 * serving 证书可能不是同一条链（真机上验证过），按 CA 校验会把正常 agent 拒掉；
 * 指纹固定既避免「任何本地进程都能冒充」，也不依赖证书链类型。
 */
object AgentApi {

    private const val TAG = "AgentApi"
    private const val PREFS = "droidnode_agent_tls"
    private const val KEY_PIN = "kubelet_cert_sha256"

    /** Runtime kubelet port; must match what the agent process was started with. */
    @Volatile
    var port: Int = LocalConfig.DEFAULT_KUBELET_PORT

    @Volatile
    private var prefs: SharedPreferences? = null

    data class Response(val code: Int, val body: String)

    fun configure(context: Context) {
        port = LocalConfig.kubeletPort(context)
        prefs = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
    }

    private fun fingerprint(cert: X509Certificate): String {
        val digest = MessageDigest.getInstance("SHA-256").digest(cert.encoded)
        return digest.joinToString("") { "%02x".format(it) }
    }

    private val factory: SSLSocketFactory by lazy {
        val ctx = SSLContext.getInstance("TLS")
        ctx.init(
            null,
            arrayOf<TrustManager>(
                object : X509TrustManager {
                    override fun checkClientTrusted(chain: Array<X509Certificate>, authType: String) =
                        Unit

                    override fun checkServerTrusted(chain: Array<X509Certificate>, authType: String) {
                        val leaf = chain.firstOrNull()
                            ?: throw CertificateException("empty certificate chain")
                        leaf.checkValidity()
                        val store = prefs ?: return
                        val fp = fingerprint(leaf)
                        val known = store.getString(KEY_PIN, null)
                        when {
                            known == null -> store.edit().putString(KEY_PIN, fp).apply()
                            known != fp -> throw CertificateException(
                                "agent certificate changed (pinned $known, got $fp)",
                            )
                        }
                    }

                    override fun getAcceptedIssuers(): Array<X509Certificate> = arrayOf()
                },
            ),
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
        // The serving cert's SAN does not include 127.0.0.1; the pinned
        // fingerprint above is what prevents impersonation.
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
        Log.w(TAG, "$method $path failed: ${e.javaClass.simpleName}: ${e.message}")
        null
    }
}
