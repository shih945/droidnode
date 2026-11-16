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
        // Copied addresses sometimes contain a space before `://` (terminal
        // wrapping); no valid server URL contains whitespace.
        var s = raw.filterNot { it.isWhitespace() }
        if (s.isEmpty()) return ""
        if (!s.startsWith("http://") && !s.startsWith("https://")) s = "https://$s"
        return s.trimEnd('/')
    }

    /** Minimal kubeconfig matching the hand-imported format. */
    fun buildKubeconfig(server: String, token: String, caPem: String): String {
        val caB64 = Base64.getEncoder().encodeToString(caPem.toByteArray())
        // ServiceAccount JWTs never contain whitespace. Terminals wrap long
        // tokens, so a pasted token may contain spaces/newlines; strip them or
        // the YAML token line becomes unparseable and the API returns 401.
        val cleanToken = token.filterNot { it.isWhitespace() }
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
            append("    token: $cleanToken")
        }
    }

    /** Encrypted file shown in Settings / used for persistence. */
    fun file(ctx: Context): File = NodeStorage.kubeconfigEncrypted(ctx)

    private fun legacyFile(ctx: Context): File = NodeStorage.legacyKubeconfig(ctx)

    /**
     * Decrypt for the settings/test UI. Also migrates the old plaintext
     * `files/kubeconfig` to `files/kubeconfig.enc` on first read.
     */
    fun read(ctx: Context): String? = try {
        // A plaintext file (e.g. imported by a restore script) is newer than the
        // encrypted copy, so migrate it first and make sure it never lingers.
        val legacy = legacyFile(ctx)
        if (legacy.exists()) {
            val text = legacy.readText()
            write(ctx, text)
            text
        } else {
            val enc = file(ctx)
            if (enc.exists()) KubeconfigCrypto.decrypt(enc.readBytes()) else null
        }
    } catch (e: Exception) {
        null
    }

    fun write(ctx: Context, text: String): Boolean = try {
        NodeStorage.config(ctx).mkdirs()
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
            val runtime = NodeStorage.kubeconfigRuntime(ctx)
            runtime.parentFile?.mkdirs()
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
        NodeStorage.kubeconfigRuntime(ctx).delete()
    }

    /** Only the `server:` line, enough to show the control-plane address. */
    fun serverFrom(kubeconfig: String?): String? {
        if (kubeconfig.isNullOrBlank()) return null
        return Regex("(?m)^\\s*server:\\s*(\\S+)").find(kubeconfig)?.groupValues?.get(1)
    }
}
