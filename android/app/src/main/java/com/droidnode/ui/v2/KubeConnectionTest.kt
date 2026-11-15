package com.droidnode.ui.v2

import java.util.Base64
import java.io.ByteArrayInputStream
import java.net.HttpURLConnection
import java.net.SocketTimeoutException
import java.net.URL
import java.net.UnknownHostException
import java.security.KeyFactory
import java.security.KeyStore
import java.security.PrivateKey
import java.security.SecureRandom
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.security.spec.PKCS8EncodedKeySpec
import javax.net.ssl.HostnameVerifier
import javax.net.ssl.HttpsURLConnection
import javax.net.ssl.KeyManager
import javax.net.ssl.KeyManagerFactory
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLException
import javax.net.ssl.TrustManager
import javax.net.ssl.TrustManagerFactory
import javax.net.ssl.X509TrustManager

/**
 * E13 的真正「连接测试」：不只探测 TCP 端口，而是用 kubeconfig 里的
 * CA + 凭据（token 或客户端证书）走完 TLS 握手并请求 `/version`。
 *
 * 结果分类对排障很重要：
 *  - 连不上      → 网络 / 防火墙 / 地址写错
 *  - TLS 失败    → CA 不匹配、服务器证书过期、证书 SAN 不含该 IP
 *  - 401 / 403   → 凭据无效或 RBAC 不足（网络与 TLS 都是通的）
 *  - 200         → 记录集群版本，确认身份与地址都对
 */
object KubeConnectionTest {

    /** kubeconfig 实际用哪种凭据连上的；401 时用来区分「没带凭据」和「凭据被拒」。 */
    enum class Auth { TOKEN, CLIENT_CERT, NONE }

    sealed class Result {
        data class Ok(
            val server: String,
            val version: String,
            val millis: Long,
            val auth: Auth = Auth.NONE,
        ) : Result()

        data class AuthFailed(
            val server: String,
            val code: Int,
            val auth: Auth = Auth.NONE,
        ) : Result()

        data class HttpFailed(val server: String, val code: Int) : Result()
        data class TlsFailed(val message: String) : Result()
        data class Unreachable(val server: String?, val message: String) : Result()
        data class BadConfig(val message: String) : Result()
    }

    fun test(kubeconfig: String?): Result {
        if (kubeconfig.isNullOrBlank()) return Result.BadConfig(Tr.s("没有 kubeconfig"))
        val server = serverFrom(kubeconfig)
            ?: return Result.BadConfig(Tr.s("kubeconfig 里没有 server: 行"))
        val insecure = Regex("(?m)^\\s*insecure-skip-tls-verify:\\s*\"?true\"?\\s*$")
            .containsMatchIn(kubeconfig)

        val token = firstValue(kubeconfig, "token")
        val keyManagers = try {
            clientKeyManagers(kubeconfig)
        } catch (e: Exception) {
            null
        }
        val auth = when {
            !token.isNullOrBlank() -> Auth.TOKEN
            keyManagers != null -> Auth.CLIENT_CERT
            else -> Auth.NONE
        }

        val ssl = try {
            buildSslContext(kubeconfig, insecure, keyManagers)
        } catch (e: Exception) {
            return Result.TlsFailed(Tr.s("证书配置解析失败：%1\$s", e.message ?: e.javaClass.simpleName))
        }

        val started = System.currentTimeMillis()
        return try {
            val url = URL(server.trimEnd('/') + "/version")
            val conn = url.openConnection() as HttpURLConnection
            if (conn is HttpsURLConnection) {
                conn.sslSocketFactory = ssl.socketFactory
                if (insecure) conn.hostnameVerifier = HostnameVerifier { _, _ -> true }
            }
            conn.connectTimeout = 4000
            conn.readTimeout = 6000
            conn.requestMethod = "GET"
            conn.instanceFollowRedirects = false
            if (!token.isNullOrBlank()) {
                conn.setRequestProperty("Authorization", "Bearer $token")
            }
            conn.setRequestProperty("Accept", "application/json")
            val code = conn.responseCode
            val body = try {
                (if (code in 200..299) conn.inputStream else conn.errorStream)
                    ?.bufferedReader()?.use { it.readText() }.orEmpty()
            } catch (_: Exception) {
                ""
            }
            val millis = System.currentTimeMillis() - started
            when {
                code in 200..299 -> {
                    val version = Regex("\"gitVersion\"\\s*:\\s*\"([^\"]+)\"").find(body)
                        ?.groupValues?.get(1) ?: "unknown"
                    Result.Ok(server, version, millis, auth)
                }
                code == 401 || code == 403 -> Result.AuthFailed(server, code, auth)
                else -> Result.HttpFailed(server, code)
            }
        } catch (e: SSLException) {
            Result.TlsFailed(describeTls(e))
        } catch (e: SocketTimeoutException) {
            Result.Unreachable(server, Tr.s("连接超时（4 秒）"))
        } catch (e: UnknownHostException) {
            Result.Unreachable(server, Tr.s("域名解析失败"))
        } catch (e: Exception) {
            Result.Unreachable(server, e.message ?: e.javaClass.simpleName)
        }
    }

    fun serverFrom(kubeconfig: String): String? =
        firstValue(kubeconfig, "server")?.trim()?.trim('"')?.takeIf { it.isNotEmpty() }

    private fun describeTls(e: SSLException): String {
        val msg = e.message.orEmpty()
        return when {
            msg.contains("Trust anchor", true) || msg.contains("PKIX", true) ->
                Tr.s("CA 与服务器证书不匹配（x509 校验失败）")
            msg.contains("No subject alternative", true) || msg.contains("hostname", true) ->
                Tr.s("证书 SAN 不包含这个地址")
            msg.contains("expired", true) -> Tr.s("服务器证书已过期")
            else -> Tr.s("TLS 握手失败：%1\$s", msg)
        }
    }

    private fun buildSslContext(
        kubeconfig: String,
        insecure: Boolean,
        keyManagers: Array<KeyManager>?,
    ): SSLContext {
        val ctx = SSLContext.getInstance("TLS")
        if (insecure) {
            val trustAll = arrayOf<TrustManager>(object : X509TrustManager {
                override fun checkClientTrusted(chain: Array<X509Certificate>, authType: String) {}
                override fun checkServerTrusted(chain: Array<X509Certificate>, authType: String) {}
                override fun getAcceptedIssuers(): Array<X509Certificate> = arrayOf()
            })
            ctx.init(keyManagers, trustAll, SecureRandom())
            return ctx
        }

        val caPem = firstValue(kubeconfig, "certificate-authority-data")
            ?.let { String(Base64.getDecoder().decode(it.trim())) }
        val trustManagers: Array<TrustManager> = if (caPem != null) {
            val ks = KeyStore.getInstance(KeyStore.getDefaultType())
            ks.load(null, null)
            val cert = CertificateFactory.getInstance("X.509")
                .generateCertificate(ByteArrayInputStream(caPem.toByteArray()))
            ks.setCertificateEntry("droidnode-ca", cert)
            val tmf = TrustManagerFactory
                .getInstance(TrustManagerFactory.getDefaultAlgorithm())
                .apply { init(ks) }
            tmf.trustManagers
        } else {
            // No CA in the kubeconfig: fall back to the system trust store.
            TrustManagerFactory
                .getInstance(TrustManagerFactory.getDefaultAlgorithm())
                .apply { init(null as KeyStore?) }
                .trustManagers
        }

        ctx.init(keyManagers, trustManagers, SecureRandom())
        return ctx
    }

    // ── client credentials ────────────────────────────────────────────────

    fun clientKeyManagers(kubeconfig: String): Array<KeyManager>? {
        val certB64 = firstValue(kubeconfig, "client-certificate-data") ?: return null
        val keyB64 = firstValue(kubeconfig, "client-key-data") ?: return null
        return try {
            val certPem = String(Base64.getDecoder().decode(certB64.trim()))
            val keyPem = String(Base64.getDecoder().decode(keyB64.trim()))
            val certs = CertificateFactory.getInstance("X.509")
                .generateCertificates(ByteArrayInputStream(certPem.toByteArray()))
                .toTypedArray()
            val key = parsePrivateKey(keyPem) ?: return null
            val ks = KeyStore.getInstance(KeyStore.getDefaultType())
            ks.load(null, null)
            val password = "droidnode".toCharArray()
            ks.setKeyEntry("client", key, password, certs)
            KeyManagerFactory.getInstance(KeyManagerFactory.getDefaultAlgorithm())
                .apply { init(ks, password) }
                .keyManagers
        } catch (_: Exception) {
            null
        }
    }

    /**
     * kubeconfig 的私钥常见三种 PEM 头，全部支持：
     *  - `BEGIN PRIVATE KEY`     → PKCS#8，可直接用
     *  - `BEGIN RSA PRIVATE KEY` → PKCS#1，按 rsaEncryption 包一层
     *  - `BEGIN EC PRIVATE KEY`  → SEC1（k3s / openssl 默认），曲线取 `[0]` 参数，
     *                              没有参数时按私钥长度推断 P-256/384/521
     * 包装逻辑用 JDK 单独验证过（RSA 字节级一致；EC 两种形态均能验签）。
     */
    internal fun parsePrivateKey(pem: String): PrivateKey? {
        val block = pemBlocks(pem).firstOrNull { it.first.endsWith("PRIVATE KEY") } ?: return null
        val pkcs8: ByteArray = when (block.first) {
            "PRIVATE KEY" -> block.second
            "RSA PRIVATE KEY" -> wrapRsa(block.second)
            "EC PRIVATE KEY" -> {
                val sec1 = block.second
                val curve = sec1CurveOid(sec1)
                    ?: inferCurveOid(sec1PrivateKeyLength(sec1) ?: 0)
                    ?: return null
                wrapEc(sec1, curve)
            }
            else -> return null
        }
        for (algorithm in arrayOf("RSA", "EC", "Ed25519")) {
            try {
                return KeyFactory.getInstance(algorithm)
                    .generatePrivate(PKCS8EncodedKeySpec(pkcs8))
            } catch (_: Exception) {
                // try the next algorithm
            }
        }
        return null
    }

    /** All PEM blocks in the text, as (label, DER bytes). */
    private fun pemBlocks(pem: String): List<Pair<String, ByteArray>> {
        val re = Regex("-----BEGIN ([A-Z0-9 ]+)-----([\\s\\S]*?)-----END \\1-----")
        return re.findAll(pem).mapNotNull { m ->
            val label = m.groupValues[1].trim()
            val body = m.groupValues[2].replace(Regex("\\s"), "")
            val der = try {
                Base64.getDecoder().decode(body)
            } catch (_: Exception) {
                null
            }
            der?.let { label to it }
        }.toList()
    }

    // ── minimal DER helpers (write) ───────────────────────────────────────

    private val OID_RSA = hexToBytes("06092A864886F70D010101") // 1.2.840.113549.1.1.1
    private val OID_EC = hexToBytes("06072A8648CE3D0201") // 1.2.840.10045.2.1
    private val OID_P256 = hexToBytes("06082A8648CE3D030107") // prime256v1
    private val OID_P384 = hexToBytes("06052B81040022") // secp384r1
    private val OID_P521 = hexToBytes("06052B81040023") // secp521r1
    private val DER_NULL = byteArrayOf(0x05, 0x00)
    private val DER_INT0 = byteArrayOf(0x02, 0x01, 0x00)

    private fun wrapRsa(pkcs1: ByteArray): ByteArray =
        derTlv(0x30, derConcat(DER_INT0, derTlv(0x30, derConcat(OID_RSA, DER_NULL)), derTlv(0x04, pkcs1)))

    private fun wrapEc(sec1: ByteArray, curveOid: ByteArray): ByteArray =
        derTlv(0x30, derConcat(DER_INT0, derTlv(0x30, derConcat(OID_EC, curveOid)), derTlv(0x04, sec1)))

    private fun derTlv(tag: Int, content: ByteArray): ByteArray {
        val len = derLen(content.size)
        val out = ByteArray(1 + len.size + content.size)
        out[0] = tag.toByte()
        System.arraycopy(len, 0, out, 1, len.size)
        System.arraycopy(content, 0, out, 1 + len.size, content.size)
        return out
    }

    private fun derLen(n: Int): ByteArray = when {
        n < 0x80 -> byteArrayOf(n.toByte())
        n < 0x100 -> byteArrayOf(0x81.toByte(), n.toByte())
        else -> byteArrayOf(0x82.toByte(), (n shr 8).toByte(), n.toByte())
    }

    private fun derConcat(vararg parts: ByteArray): ByteArray {
        val out = ByteArray(parts.sumOf { it.size })
        var at = 0
        for (p in parts) {
            System.arraycopy(p, 0, out, at, p.size)
            at += p.size
        }
        return out
    }

    /** One parsed DER element. */
    private data class Der(
        val tag: Int,
        val start: Int,
        val contentStart: Int,
        val contentLen: Int,
    ) {
        val end: Int get() = contentStart + contentLen
    }

    /** DER reader for one element at [off]. */
    private fun derRead(b: ByteArray, off: Int): Der {
        val tag = b[off].toInt() and 0xFF
        var i = off + 1
        var len = b[i].toInt() and 0xFF
        i++
        if (len and 0x80 != 0) {
            val n = len and 0x7F
            len = 0
            for (k in 0 until n) {
                len = (len shl 8) or (b[i + k].toInt() and 0xFF)
            }
            i += n
        }
        return Der(tag, off, i, len)
    }

    /** Children of the SEQUENCE at offset 0. */
    private fun derChildren(b: ByteArray): List<Der> {
        if (b.size < 2) return emptyList()
        val top = derRead(b, 0)
        val out = ArrayList<Der>(4)
        var p = top.contentStart
        while (p < top.end && p < b.size) {
            val node = derRead(b, p)
            out.add(node)
            p = node.end
        }
        return out
    }

    /** The named-curve OID (`[0]` params) inside a SEC1 structure, if present. */
    private fun sec1CurveOid(sec1: ByteArray): ByteArray? {
        for (child in derChildren(sec1)) {
            if (child.tag == 0xA0) {
                val inner = derRead(sec1, child.contentStart)
                if (inner.tag == 0x06) return sec1.copyOfRange(child.contentStart, inner.end)
                return null // explicit parameters, not a named curve
            }
        }
        return null
    }

    /** Private-key OCTET STRING length inside a SEC1 structure. */
    private fun sec1PrivateKeyLength(sec1: ByteArray): Int? =
        derChildren(sec1).firstOrNull { it.tag == 0x04 }?.contentLen

    private fun inferCurveOid(rawLen: Int): ByteArray? = when (rawLen) {
        32 -> OID_P256
        48 -> OID_P384
        66 -> OID_P521
        else -> null
    }

    private fun hexToBytes(hex: String): ByteArray =
        ByteArray(hex.length / 2) { i ->
            ((Character.digit(hex[i * 2], 16) shl 4) + Character.digit(hex[i * 2 + 1], 16)).toByte()
        }

    internal fun firstValue(kubeconfig: String, key: String): String? =
        Regex("(?m)^\\s*" + Regex.escape(key) + ":\\s*(\\S+)\\s*$")
            .find(kubeconfig)?.groupValues?.get(1)
}
