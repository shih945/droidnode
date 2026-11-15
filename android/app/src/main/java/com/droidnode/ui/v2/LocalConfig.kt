package com.droidnode.ui.v2

import android.content.Context
import com.droidnode.R
import com.droidnode.data.AppPrefs
import com.droidnode.data.KubeconfigStore
import com.droidnode.data.NetworkInterfaces
import com.droidnode.data.RegistryMirrorStore
import java.io.File

/**
 * Compatibility facade over the `data/` stores. New code should use
 * [AppPrefs], [KubeconfigStore], [NetworkInterfaces] and [RegistryMirrorStore]
 * directly; this object only keeps the existing call sites compiling while the
 * split is finished.
 */
object LocalConfig {

    // ── preferences ────────────────────────────────────────────────────────
    const val LANGUAGE_SYSTEM = AppPrefs.LANGUAGE_SYSTEM
    const val LANGUAGE_ZH = AppPrefs.LANGUAGE_ZH
    const val LANGUAGE_EN = AppPrefs.LANGUAGE_EN

    const val DEFAULT_KUBELET_PORT = AppPrefs.DEFAULT_KUBELET_PORT
    const val DEFAULT_LOG_LEVEL = AppPrefs.DEFAULT_LOG_LEVEL
    val LOG_LEVELS = AppPrefs.LOG_LEVELS

    const val APPEARANCE_SYSTEM = AppPrefs.APPEARANCE_SYSTEM
    const val APPEARANCE_LIGHT = AppPrefs.APPEARANCE_LIGHT
    const val APPEARANCE_DARK = AppPrefs.APPEARANCE_DARK
    val APPEARANCES = AppPrefs.APPEARANCES

    fun appearance(ctx: Context): String = AppPrefs.appearance(ctx)

    fun setAppearance(ctx: Context, mode: String) = AppPrefs.setAppearance(ctx, mode)

    fun appearanceLabel(mode: String): String = when (mode) {
        APPEARANCE_LIGHT -> Tr.s("亮色")
        APPEARANCE_DARK -> Tr.s("深色")
        else -> Tr.s("跟随系统")
    }

    fun language(ctx: Context): String = AppPrefs.language(ctx)

    fun setLanguage(ctx: Context, tag: String) = AppPrefs.setLanguage(ctx, tag)

    fun languageLabel(ctx: Context, tag: String): String = when (tag) {
        LANGUAGE_ZH -> Tr.s("简体中文")
        LANGUAGE_EN -> "English"
        else -> ctx.getString(R.string.settings_language_system)
    }

    fun autostart(ctx: Context): Boolean = AppPrefs.autostart(ctx)

    fun setAutostart(ctx: Context, on: Boolean) = AppPrefs.setAutostart(ctx, on)

    fun drained(ctx: Context): Boolean = AppPrefs.drained(ctx)

    fun setDrained(ctx: Context, drained: Boolean) = AppPrefs.setDrained(ctx, drained)

    fun nodeName(ctx: Context): String = AppPrefs.nodeName(ctx)

    fun setNodeName(ctx: Context, name: String) = AppPrefs.setNodeName(ctx, name)

    fun autoNodeId(ctx: Context): String = AppPrefs.autoNodeId(ctx)

    fun resolveNodeId(ctx: Context): String = AppPrefs.resolveNodeId(ctx)

    fun kubeletPort(ctx: Context): Int = AppPrefs.kubeletPort(ctx)

    fun setKubeletPort(ctx: Context, port: Int) = AppPrefs.setKubeletPort(ctx, port)

    fun logLevel(ctx: Context): String = AppPrefs.logLevel(ctx)

    fun setLogLevel(ctx: Context, level: String) = AppPrefs.setLogLevel(ctx, level)

    // ── registry mirrors ───────────────────────────────────────────────────
    private const val REGISTRY_MIRRORS_ENV = "registry-mirrors.env"

    const val MIRROR_DOCKER = RegistryMirrorStore.DOCKER
    const val MIRROR_K8S = RegistryMirrorStore.K8S
    const val MIRROR_QUAY = RegistryMirrorStore.QUAY

    val MIRROR_KEYS = RegistryMirrorStore.KEYS

    fun registryMirrorsFile(ctx: Context): File = File(ctx.filesDir, REGISTRY_MIRRORS_ENV)

    /**
     * Hostname only (optionally with a port): the agent rewrites the image
     * registry, so a URL with a path is meaningless. Blank means "not set";
     * malformed input returns null so the caller can warn.
     */
    fun normalizeMirrorHost(raw: String): String? {
        var host = raw.trim()
        if (host.isEmpty()) return ""
        host = host.removePrefix("https://").removePrefix("http://")
        host = host.trimEnd('/')
        if (!MIRROR_HOST_PATTERN.matches(host)) return null
        return host.lowercase()
    }

    private val MIRROR_HOST_PATTERN =
        Regex("^[a-z0-9]([a-z0-9.-]*[a-z0-9])?(:[0-9]{1,5})?$", RegexOption.IGNORE_CASE)

    fun readRegistryMirrors(ctx: Context): Map<String, String> {
        val file = registryMirrorsFile(ctx)
        if (!file.exists()) return emptyMap()
        return try {
            RegistryMirrorStore.parse(file.readText())
        } catch (e: Exception) {
            emptyMap()
        }
    }

    /** All three blank deletes the file, which means "direct upstream". */
    fun writeRegistryMirrors(
        ctx: Context,
        docker: String,
        k8s: String,
        quay: String,
    ): Boolean = try {
        val entries = linkedMapOf(MIRROR_DOCKER to docker, MIRROR_K8S to k8s, MIRROR_QUAY to quay)
            .filterValues { it.isNotEmpty() }
        val body = RegistryMirrorStore.render(entries)
        if (body == null) {
            registryMirrorsFile(ctx).delete()
        } else {
            ctx.filesDir.mkdirs()
            // ASCII only: the file is written as US-ASCII for the Rust parser.
            registryMirrorsFile(ctx).writeText(body, Charsets.US_ASCII)
        }
        true
    } catch (e: Exception) {
        false
    }

    // ── network preference ────────────────────────────────────────────────
    fun networkPreferenceFile(ctx: Context): File = NetworkInterfaces.preferenceFile(ctx)

    fun nodeIface(ctx: Context): String = NetworkInterfaces.nodeIface(ctx)

    fun setNodeIface(ctx: Context, iface: String): Boolean = NetworkInterfaces.setNodeIface(ctx, iface)

    fun listIpv4Interfaces(): List<Pair<String, String>> = NetworkInterfaces.listIpv4()

    fun isTetherLike(name: String): Boolean = NetworkInterfaces.isTetherLike(name)

    // ── kubeconfig ────────────────────────────────────────────────────────
    fun normalizeServer(raw: String): String = KubeconfigStore.normalizeServer(raw)

    fun buildKubeconfig(server: String, token: String, caPem: String): String =
        KubeconfigStore.buildKubeconfig(server, token, caPem)

    fun kubeconfigFile(ctx: Context): File = KubeconfigStore.file(ctx)

    fun readKubeconfig(ctx: Context): String? = KubeconfigStore.read(ctx)

    fun writeKubeconfig(ctx: Context, text: String): Boolean = KubeconfigStore.write(ctx, text)

    fun materializeKubeconfig(ctx: Context): File? = KubeconfigStore.materialize(ctx)

    fun deleteRuntimeKubeconfig(ctx: Context) = KubeconfigStore.deleteRuntime(ctx)

    fun serverFrom(kubeconfig: String?): String? = KubeconfigStore.serverFrom(kubeconfig)

    /** Credential type for the settings "identity" row. */
    fun authLabel(kubeconfig: String?): String = when {
        kubeconfig.isNullOrBlank() -> "—"
        Regex("(?m)^\\s*token:\\s*\\S+").containsMatchIn(kubeconfig) -> "token"
        kubeconfig.contains("client-certificate-data") -> Tr.s("客户端证书")
        else -> Tr.s("无凭据")
    }
}
