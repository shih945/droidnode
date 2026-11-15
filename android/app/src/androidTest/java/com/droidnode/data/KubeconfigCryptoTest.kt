package com.droidnode.data

import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * T11: kubeconfig at rest is AES-256-GCM encrypted with an Android Keystore key.
 * Runs on a device/emulator because the Keystore provider is not available on
 * the plain JVM.
 */
@RunWith(AndroidJUnit4::class)
class KubeconfigCryptoTest {

    private val sample = """
        apiVersion: v1
        kind: Config
        clusters:
        - name: droidnode
          cluster:
            server: https://192.168.2.7:6443
        users:
        - name: droidnode-agent
          user:
            token: super-secret-token
    """.trimIndent()

    @Test
    fun encryptDecryptRoundTrips() {
        val encrypted = KubeconfigCrypto.encrypt(sample)
        assertNotEquals(sample, String(encrypted))
        assertTrue("ciphertext should contain the secret only encrypted", !String(encrypted).contains("super-secret-token"))
        assertEquals(sample, KubeconfigCrypto.decrypt(encrypted))
    }

    @Test
    fun encryptingTwiceUsesFreshNonces() {
        val a = KubeconfigCrypto.encrypt(sample)
        val b = KubeconfigCrypto.encrypt(sample)
        assertTrue("same plaintext must not produce identical ciphertext", !a.contentEquals(b))
        assertEquals(sample, KubeconfigCrypto.decrypt(a))
        assertEquals(sample, KubeconfigCrypto.decrypt(b))
    }
}
