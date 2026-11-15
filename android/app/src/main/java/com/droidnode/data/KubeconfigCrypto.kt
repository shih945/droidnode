package com.droidnode.data

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * F2: kubeconfig at rest is encrypted with an AES-256-GCM key held by Android
 * Keystore. The plaintext is only materialized to a runtime file while the
 * foreground service is running (see LocalConfig.materializeKubeconfig).
 */
object KubeconfigCrypto {

    private const val ALIAS = "droidnode-kubeconfig"
    private const val KEYSTORE = "AndroidKeyStore"
    private const val TRANSFORMATION = "AES/GCM/NoPadding"
    private const val VERSION = 1

    private fun keyStore(): KeyStore =
        KeyStore.getInstance(KEYSTORE).apply { load(null) }

    private fun key(): SecretKey {
        val store = keyStore()
        (store.getKey(ALIAS, null) as? SecretKey)?.let { return it }
        val generator = KeyGenerator.getInstance(
            KeyProperties.KEY_ALGORITHM_AES,
            KEYSTORE,
        )
        generator.init(
            KeyGenParameterSpec.Builder(
                ALIAS,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build(),
        )
        return generator.generateKey()
    }

    /** Format: [version][ivLen][iv][ciphertext+tag]. */
    fun encrypt(plaintext: String): ByteArray {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, key())
        val iv = cipher.iv
        val body = cipher.doFinal(plaintext.toByteArray(Charsets.UTF_8))
        return ByteArray(2 + iv.size + body.size).also { out ->
            out[0] = VERSION.toByte()
            out[1] = iv.size.toByte()
            System.arraycopy(iv, 0, out, 2, iv.size)
            System.arraycopy(body, 0, out, 2 + iv.size, body.size)
        }
    }

    fun decrypt(blob: ByteArray): String? {
        if (blob.size < 3 || blob[0].toInt() != VERSION) return null
        val ivSize = blob[1].toInt()
        if (ivSize !in 12..16 || blob.size <= 2 + ivSize) return null
        return try {
            val iv = blob.copyOfRange(2, 2 + ivSize)
            val body = blob.copyOfRange(2 + ivSize, blob.size)
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, iv))
            String(cipher.doFinal(body), Charsets.UTF_8)
        } catch (e: Exception) {
            null
        }
    }
}
