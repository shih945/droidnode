package com.droidnode.ui.v2

import com.droidnode.data.KubeConnectionTest

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.security.KeyPairGenerator
import java.security.interfaces.ECPrivateKey
import java.security.interfaces.RSAPrivateKey
import java.util.Base64

/**
 * Pure-JVM tests for the kubeconfig parsing helpers used by the join flow and
 * the connection test. Kept free of Android APIs so they run with `gradlew test`.
 */
class KubeConnectionTestTest {

    private fun pem(label: String, der: ByteArray): String {
        val body = Base64.getMimeEncoder(64, "\n".toByteArray()).encodeToString(der)
        return "-----BEGIN $label-----\n$body\n-----END $label-----"
    }

    private fun kubeconfig(
        server: String = "https://192.0.2.10:6443",
        token: String? = "abc.def.ghi",
    ): String = buildString {
        appendLine("apiVersion: v1")
        appendLine("kind: Config")
        appendLine("clusters:")
        appendLine("- name: droidnode")
        appendLine("  cluster:")
        appendLine("    server: $server")
        if (token != null) {
            appendLine("users:")
            appendLine("- name: droidnode")
            appendLine("  user:")
            appendLine("    token: $token")
        }
    }

    @Test
    fun serverFromParsesTokenKubeconfig() {
        assertEquals("https://192.0.2.10:6443", KubeConnectionTest.serverFrom(kubeconfig()))
    }

    @Test
    fun serverFromTrimsQuotes() {
        val text = kubeconfig(server = "\"https://10.0.0.1:6443\"")
        assertEquals("https://10.0.0.1:6443", KubeConnectionTest.serverFrom(text))
    }

    @Test
    fun serverFromReturnsNullWhenMissing() {
        assertNull(KubeConnectionTest.serverFrom("apiVersion: v1\nkind: Config\n"))
    }

    @Test
    fun firstValueReadsTokenFromUsersBlock() {
        assertEquals("abc.def.ghi", KubeConnectionTest.firstValue(kubeconfig(), "token"))
    }

    @Test
    fun buildKubeconfigStripsWhitespaceFromToken() {
        // Terminals wrap long JWTs and put spaces in the pasted text; the
        // token line must still be parseable as a single value.
        val wrapped = "eyJhbGciOiJSUzI1NiJ9\n  payload.sig nature"
        val config = com.droidnode.data.KubeconfigStore.buildKubeconfig(
            "https://192.0.2.10:6443",
            wrapped,
            "-----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----",
        )
        assertEquals("eyJhbGciOiJSUzI1NiJ9payload.signature", KubeConnectionTest.firstValue(config, "token"))
    }

    @Test
    fun parsePrivateKeyAcceptsPkcs8Rsa() {
        val pair = KeyPairGenerator.getInstance("RSA").apply { initialize(2048) }.generateKeyPair()
        val der = pair.private.encoded
        val key = KubeConnectionTest.parsePrivateKey(pem("PRIVATE KEY", der))
        assertTrue(key is RSAPrivateKey)
    }

    @Test
    fun parsePrivateKeyAcceptsPkcs8Ec() {
        val pair = KeyPairGenerator.getInstance("EC").apply { initialize(256) }.generateKeyPair()
        val key = KubeConnectionTest.parsePrivateKey(pem("PRIVATE KEY", pair.private.encoded))
        assertTrue(key is ECPrivateKey)
    }

    @Test
    fun parsePrivateKeyRejectsGarbage() {
        assertNull(KubeConnectionTest.parsePrivateKey("not a pem at all"))
    }

    @Test
    fun clientKeyManagersReturnsNullOnGarbageCredentials() {
        val broken = kubeconfig().let {
            it + "\n    client-certificate-data: bm90LWEtY2VydA==\n    client-key-data: bm90LWEta2V5\n"
        }
        assertNull(KubeConnectionTest.clientKeyManagers(broken))
    }
}
