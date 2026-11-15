package com.droidnode.data

import android.content.Context
import java.io.File
import java.util.Base64

/**
 * kubeconfig persistence: template rendering, encrypted-at-rest storage and the
 * short-lived plaintext file handed to the agent while the service runs.
 */
object KubeconfigStore {

    private const val KUBECONFIG_ENC = "kubeconfig.enc"
    private const val KUBECONFIG_LEGACY = "kubeconfig"
    private const val KUBECONFIG_RUNTIME = "kubeconfig.runtime"

    /** Add `https://` if missing and drop the trailing slash; empty stays empty. */
    fun normalizeServer(raw: String): String {
        var s = raw.trim()
        if (s.isEmpty()) return ""
        if (!s.startsWith("http://") && !s.startsWith("https://")) s = "https://$s"
        return s.trimEnd('/')
    }

    /** Minimal kubeconfig matching the hand-imported format. */
    fun buildKubeconfig(server: String, token: String, caPem: String): String {
        val caB64 = Base64.getEncoder().encodeToString(caPem.toByteArray())
        return buildString {
            appendLine("apiVersion: v1")
            appendLine("kind: Config")
            appendLine("clusters:")
            appendLine("- name: droidnode")
            appendLine("  cluster:")
            appendLine("    certificate-authority-data: $caB64")
            appendLine("    server: ${server.trimEnd('/')}")
            appendLine("contexts:")
            appendLine("- name: droidnode")
            appendLine("  context:")
            appendLine("    cluster: droidnode")
            appendLine("    user: droidnode-agent")
            appendLine("current-context: droidnode")
            appendLine("users:")
            appendLine("- name: droidnode-agent")
            appendLine("  user:")
            append("    token: $token")
        }
    }

    /** Encrypted file shown in Settings / used for persistence. */
    fun file(ctx: Context): File = File(ctx.filesDir, KUBECONFIG_ENC)

    private fun legacyFile(ctx: Context): File = File(ctx.filesDir, KUBECONFIG_LEGACY)

    /**
     * Decrypt for the settings/test UI. Also migrates the old plaintext
     * `files/kubeconfig` to `files/kubeconfig.enc` on first read.
     */
    fun read(ctx: Context): String? = try {
        val enc = file(ctx)
        when {
            enc.exists() -> KubeconfigCrypto.decrypt(enc.readBytes())
            legacyFile(ctx).exists() -> {
                val text = legacyFile(ctx).readText()
                write(ctx, text)
                text
            }
            else -> null
        }
    } catch (e: Exception) {
        null
    }

    fun write(ctx: Context, text: String): Boolean = try {
        ctx.filesDir.mkdirs()
        val enc = KubeconfigCrypto.encrypt(text)
        file(ctx).writeBytes(enc)
        // Only remove the legacy plaintext after the encrypted copy is durable.
        legacyFile(ctx).delete()
        true
    } catch (e: Exception) {
        false
    }

    /**
     * Plaintext copy for the running agent only, deleted when the service stops.
     * The agent needs a path (KUBECONFIG), not an inline blob.
     */
    fun materialize(ctx: Context): File? {
        return try {
            val text = read(ctx) ?: return null
            val runtime = File(ctx.filesDir, KUBECONFIG_RUNTIME)
            runtime.writeText(text)
            runtime.setReadable(false, false)
            runtime.setReadable(true, true)
            runtime.setWritable(false, false)
            runtime.setWritable(true, true)
            runtime
        } catch (e: Exception) {
            null
        }
    }

    fun deleteRuntime(ctx: Context) {
        File(ctx.filesDir, KUBECONFIG_RUNTIME).delete()
    }

    /** Only the `server:` line, enough to show the control-plane address. */
    fun serverFrom(kubeconfig: String?): String? {
        if (kubeconfig.isNullOrBlank()) return null
        return Regex("(?m)^\\s*server:\\s*(\\S+)").find(kubeconfig)?.groupValues?.get(1)
    }
}
