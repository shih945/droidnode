package com.droidnode.data

import android.content.Context
import android.util.Log
import com.droidnode.ui.v2.LocalConfig
import org.json.JSONObject
import java.io.ByteArrayInputStream
import java.io.File
import java.net.URL
import java.security.KeyStore
import java.security.SecureRandom
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.util.Base64
import javax.net.ssl.HttpsURLConnection
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLException
import javax.net.ssl.SSLSocketFactory
import javax.net.ssl.TrustManager
import javax.net.ssl.TrustManagerFactory
import javax.net.ssl.X509TrustManager

/**
 * agent 的 loopback 接口客户端（只读为主）。
 *
 * 端口由 E14 的配置决定（默认 10255），`DroidNodeApp` 在进程启动时写入。
 *
 * 证书信任：agent 每次启动都会重新签发 serving 证书，所以不能固定叶子证书指纹。
 * 改为信任**签发它的 CA**：本地的 `files/kubelet-ca.crt`（回退用）和 kubeconfig
 * 里的 `certificate-authority-data`（CSR 路径用集群 CA 签发）。两者都放进一个
 * TrustManager，证书轮换不受影响，同时仍然拒绝无关证书。
 */
object AgentApi {

    private const val TAG = "AgentApi"

    /** Runtime kubelet port; must match what the agent process was started with. */
    @Volatile
    var port: Int = LocalConfig.DEFAULT_KUBELET_PORT

    @Volatile
    private var trustedCAs: List<X509Certificate> = emptyList()

    /** Kept so the trust store can be reloaded after a CA rotation. */
    @Volatile
    private var appContext: Context? = null

    private var cachedFactory: SSLSocketFactory? = null
    private var cachedForCAs: List<X509Certificate> = emptyList()

    data class Response(val code: Int, val body: String)

    fun configure(context: Context) {
        val app = context.applicationContext
        appContext = app
        port = LocalConfig.kubeletPort(app)
        val cas = loadTrustedCAs(app)
        synchronized(this) {
            trustedCAs = cas
            // Joining (or re-joining) a cluster rotates the kubelet CA; a cached
            // context built from the old anchors would reject every request.
            cachedFactory = null
        }
    }

    private fun loadTrustedCAs(context: Context): List<X509Certificate> {
        val factory = CertificateFactory.getInstance("X.509")
        val out = ArrayList<X509Certificate>(4)

        fun addPem(pem: String?) {
            if (pem.isNullOrBlank()) return
            runCatching {
                factory.generateCertificates(ByteArrayInputStream(pem.toByteArray()))
                    .forEach { out.add(it as X509Certificate) }
            }
        }

        for (name in listOf("kubelet-ca.crt", "client-ca.crt")) {
            val file = File(NodeStorage.config(context), name)
            if (file.isFile) addPem(file.readText())
        }
        // CSR-issued serving certs are signed by the cluster CA from the kubeconfig.
        KubeconfigStore.read(context)?.let { kube ->
            Regex("certificate-authority-data:\\s*([A-Za-z0-9+/=]+)")
                .find(kube)
                ?.groupValues
                ?.getOrNull(1)
                ?.let { b64 ->
                    runCatching { String(Base64.getDecoder().decode(b64)) }
                        .getOrNull()
                        ?.let(::addPem)
                }
        }
        return out
    }

    private fun trustManager(cas: List<X509Certificate>): X509TrustManager {
        if (cas.isEmpty()) {
            // Fresh install: the agent has not written a CA yet. Accept only until
            // a CA appears (loadTrustedCAs runs again on the next process start).
            return object : X509TrustManager {
                override fun checkClientTrusted(chain: Array<X509Certificate>, authType: String) =
                    Unit

                override fun checkServerTrusted(chain: Array<X509Certificate>, authType: String) =
                    Unit

                override fun getAcceptedIssuers(): Array<X509Certificate> = arrayOf()
            }
        }
        val store = KeyStore.getInstance(KeyStore.getDefaultType()).apply { load(null, null) }
        cas.forEachIndexed { i, cert -> store.setCertificateEntry("ca-$i", cert) }
        val tmf = TrustManagerFactory.getInstance(TrustManagerFactory.getDefaultAlgorithm())
        tmf.init(store)
        return tmf.trustManagers.filterIsInstance<X509TrustManager>().first()
    }

    private fun factory(): SSLSocketFactory {
        synchronized(this) {
            val cas = trustedCAs
            cachedFactory?.let { if (cachedForCAs == cas) return it }
            val ctx = SSLContext.getInstance("TLS")
            ctx.init(null, arrayOf<TrustManager>(trustManager(cas)), SecureRandom())
            val built = ctx.socketFactory
            cachedFactory = built
            cachedForCAs = cas
            return built
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
        call(method, path, timeoutMs)
    } catch (e: SSLException) {
        // Joining a cluster rotates the kubelet CA, so a trust store loaded at
        // process start no longer matches the serving cert the agent just
        // issued. Reload it from disk and retry once instead of staying "无响应"
        // until the app is restarted.
        Log.w(TAG, "$method $path: agent certificate not trusted (${e.message}); reloading CAs")
        appContext?.let { configure(it) }
        try {
            call(method, path, timeoutMs)
        } catch (retry: Exception) {
            Log.w(TAG, "$method $path failed: ${retry.javaClass.simpleName}: ${retry.message}")
            null
        }
    } catch (e: Exception) {
        Log.w(TAG, "$method $path failed: ${e.javaClass.simpleName}: ${e.message}")
        null
    }

    private fun call(method: String, path: String, timeoutMs: Int): Response {
        val conn = URL("https://127.0.0.1:$port$path").openConnection() as HttpsURLConnection
        conn.sslSocketFactory = factory()
        // The serving cert's SAN does not include 127.0.0.1; the CA trust above is
        // what prevents impersonation.
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
        return Response(code, body)
    }
}
