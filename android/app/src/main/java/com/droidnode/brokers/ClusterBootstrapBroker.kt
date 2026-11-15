package com.droidnode.brokers

import java.io.ByteArrayInputStream
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest
import java.security.SecureRandom
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import javax.net.ssl.HostnameVerifier
import javax.net.ssl.HttpsURLConnection
import javax.net.ssl.SSLContext
import javax.net.ssl.TrustManager
import javax.net.ssl.X509TrustManager

/**
 * Broker: 从控制面取集群 CA。
 *
 * k3s 在 `GET /cacerts` 上**不要求认证**地提供自己的 CA —— `k3s agent --server --token`
 * 就是这么拿到 CA 的（先用不校验的 TLS 连上去，取到 CA 后再用它校验真正的连接）。
 * 我们照做：这里是唯一的「先信任后校验」点，所以必须把指纹交给界面显示，
 * 让用户能和主节点上的证书对一次。
 */
object ClusterBootstrapBroker {

    data class CaBundle(val pem: String, val sha256Fingerprint: String)

    sealed class Result {
        data class Ok(val bundle: CaBundle) : Result()
        data class Failed(val message: String) : Result()
    }

    fun fetchCa(serverUrl: String): Result = try {
        val url = URL(serverUrl.trimEnd('/') + "/cacerts")
        val conn = url.openConnection() as HttpURLConnection
        if (conn is HttpsURLConnection) {
            conn.sslSocketFactory = trustAllContext().socketFactory
            conn.hostnameVerifier = HostnameVerifier { _, _ -> true }
        }
        conn.connectTimeout = 5000
        conn.readTimeout = 8000
        conn.requestMethod = "GET"
        val code = conn.responseCode
        if (code !in 200..299) {
            Result.Failed("$url 返回 HTTP $code（k3s 才有 /cacerts；非 k3s 集群请手动粘贴 CA）")
        } else {
            val body = conn.inputStream.bufferedReader().use { it.readText() }.trim()
            if (!body.contains("BEGIN CERTIFICATE")) {
                Result.Failed("$url 返回的不是证书")
            } else {
                Result.Ok(CaBundle(body, fingerprint(body)))
            }
        }
    } catch (e: Exception) {
        Result.Failed(e.message ?: e.javaClass.simpleName)
    }

    /** SHA-256 of the first certificate in the bundle, colon-separated. */
    fun fingerprint(pem: String): String = try {
        val cert = CertificateFactory.getInstance("X.509")
            .generateCertificate(ByteArrayInputStream(pem.toByteArray())) as X509Certificate
        MessageDigest.getInstance("SHA-256").digest(cert.encoded)
            .joinToString(":") { "%02X".format(it) }
    } catch (_: Exception) {
        ""
    }

    private fun trustAllContext(): SSLContext {
        val trustAll = arrayOf<TrustManager>(object : X509TrustManager {
            override fun checkClientTrusted(chain: Array<X509Certificate>, authType: String) {}
            override fun checkServerTrusted(chain: Array<X509Certificate>, authType: String) {}
            override fun getAcceptedIssuers(): Array<X509Certificate> = arrayOf()
        })
        return SSLContext.getInstance("TLS").apply { init(null, trustAll, SecureRandom()) }
    }
}
