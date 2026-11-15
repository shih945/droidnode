package com.droidnode.ui.v2

import android.content.Context
import android.provider.Settings
import android.util.Base64
import com.droidnode.R
import java.io.File
import java.net.Inet4Address
import java.net.NetworkInterface

/**
 * 这一版界面自己的本地状态：kubeconfig 文件、排空请求，以及 E14 的节点配置
 *（节点名 / kubelet 端口 / 日志级别）。写在这里的值由 ForegroundServiceExposer
 * 在启动 agent 时读成环境变量；改完必须重启 agent 才生效。
 */
object LocalConfig {

    private const val PREFS = "droidnode_console"
    private const val KEY_DRAINED_AT = "drained_requested_at"
    private const val KEY_NODE_NAME = "node_name"
    private const val KEY_KUBELET_PORT = "kubelet_port"
    private const val KEY_LOG_LEVEL = "log_level"
    private const val KEY_APPEARANCE = "appearance"
    private const val KEY_AUTOSTART = "autostart"
    private const val KEY_LANGUAGE = "language"

    /** 空串 = 跟随系统；其余是 BCP-47 语言标签。 */
    const val LANGUAGE_SYSTEM = ""
    const val LANGUAGE_ZH = "zh"
    const val LANGUAGE_EN = "en"

    const val DEFAULT_KUBELET_PORT = 10255
    const val DEFAULT_LOG_LEVEL = "info"
    val LOG_LEVELS = setOf("error", "warn", "info", "debug", "trace")

    private fun prefs(ctx: Context) = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    // ── 外观：亮色 / 深色 / 跟随系统 ─────────────────────────────────────
    const val APPEARANCE_SYSTEM = "system"
    const val APPEARANCE_LIGHT = "light"
    const val APPEARANCE_DARK = "dark"
    val APPEARANCES = listOf(APPEARANCE_SYSTEM, APPEARANCE_LIGHT, APPEARANCE_DARK)

    /** 默认亮色：这台设备长期亮屏，浅色底在白天更好读。 */
    fun appearance(ctx: Context): String {
        val mode = prefs(ctx).getString(KEY_APPEARANCE, APPEARANCE_LIGHT).orEmpty()
        return if (mode in APPEARANCES) mode else APPEARANCE_LIGHT
    }

    fun setAppearance(ctx: Context, mode: String) {
        val value = if (mode in APPEARANCES) mode else APPEARANCE_LIGHT
        prefs(ctx).edit().putString(KEY_APPEARANCE, value).apply()
    }

    fun appearanceLabel(mode: String): String = when (mode) {
        APPEARANCE_LIGHT -> Tr.s("亮色")
        APPEARANCE_DARK -> Tr.s("深色")
        else -> Tr.s("跟随系统")
    }

    // ── 语言：空串 = 跟随系统 ─────────────────────────────────────────────

    fun language(ctx: Context): String = prefs(ctx).getString(KEY_LANGUAGE, LANGUAGE_SYSTEM).orEmpty()

    fun setLanguage(ctx: Context, tag: String) {
        prefs(ctx).edit().putString(KEY_LANGUAGE, tag).apply()
    }

    fun languageLabel(ctx: Context, tag: String): String = when (tag) {
        LANGUAGE_ZH -> Tr.s("简体中文")
        LANGUAGE_EN -> "English"
        else -> ctx.getString(R.string.settings_language_system)
    }

    // ── 开机自启：BootReceiver 读这个开关 ─────────────────────────────────
    fun autostart(ctx: Context): Boolean = prefs(ctx).getBoolean(KEY_AUTOSTART, true)

    fun setAutostart(ctx: Context, on: Boolean) {
        prefs(ctx).edit().putBoolean(KEY_AUTOSTART, on).apply()
    }

    fun drained(ctx: Context): Boolean = prefs(ctx).getLong(KEY_DRAINED_AT, 0L) > 0L

    fun setDrained(ctx: Context, drained: Boolean) {
        prefs(ctx).edit()
            .putLong(KEY_DRAINED_AT, if (drained) System.currentTimeMillis() else 0L)
            .apply()
    }

    // ── E14: node identity / kubelet port / log level ──────────────────────

    /** Empty means "derive from ANDROID_ID" (the historical default). */
    fun nodeName(ctx: Context): String = prefs(ctx).getString(KEY_NODE_NAME, "").orEmpty()

    fun setNodeName(ctx: Context, name: String) {
        prefs(ctx).edit().putString(KEY_NODE_NAME, name.trim()).apply()
    }

    fun autoNodeId(ctx: Context): String {
        val androidId = try {
            Settings.Secure.getString(ctx.contentResolver, Settings.Secure.ANDROID_ID)
        } catch (e: Exception) {
            null
        } ?: "unknown"
        return "droidnode-$androidId"
    }

    /** What the agent will actually register as. */
    fun resolveNodeId(ctx: Context): String =
        nodeName(ctx).ifBlank { autoNodeId(ctx) }

    fun kubeletPort(ctx: Context): Int {
        val port = prefs(ctx).getInt(KEY_KUBELET_PORT, DEFAULT_KUBELET_PORT)
        return if (port in 1024..65535) port else DEFAULT_KUBELET_PORT
    }

    fun setKubeletPort(ctx: Context, port: Int) {
        prefs(ctx).edit().putInt(KEY_KUBELET_PORT, port).apply()
    }

    fun logLevel(ctx: Context): String {
        val level = prefs(ctx).getString(KEY_LOG_LEVEL, DEFAULT_LOG_LEVEL).orEmpty().lowercase()
        return if (level in LOG_LEVELS) level else DEFAULT_LOG_LEVEL
    }

    fun setLogLevel(ctx: Context, level: String) {
        prefs(ctx).edit().putString(KEY_LOG_LEVEL, level.lowercase()).apply()
    }

    // ── 镜像源：写 files/registry-mirrors.env，agent 启动时读 ──────────────
    private const val REGISTRY_MIRRORS_ENV = "registry-mirrors.env"

    /** 三个键名必须和 agent 的 registry_mirror 模块一致。 */
    const val MIRROR_DOCKER = "DROIDNODE_REGISTRY_MIRROR"
    const val MIRROR_K8S = "DROIDNODE_REGISTRY_MIRROR_K8S"
    const val MIRROR_QUAY = "DROIDNODE_REGISTRY_MIRROR_QUAY"

    val MIRROR_KEYS = listOf(MIRROR_DOCKER, MIRROR_K8S, MIRROR_QUAY)

    fun registryMirrorsFile(ctx: Context): File = File(ctx.filesDir, REGISTRY_MIRRORS_ENV)

    /**
     * 只接受主机名（可带端口）：镜像改写替换的是 `image.registry`，带路径的 URL 没有意义。
     * 空串表示「不配置」；格式不对返回 null，调用方提示用户。
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

    /** 读 agent 会读的那份文件；空值/缺失的键不返回。 */
    fun readRegistryMirrors(ctx: Context): Map<String, String> {
        val file = registryMirrorsFile(ctx)
        if (!file.exists()) return emptyMap()
        return try {
            file.readLines()
                .map { it.trim() }
                .filter { it.isNotEmpty() && !it.startsWith("#") }
                .mapNotNull { line ->
                    val idx = line.indexOf('=')
                    if (idx <= 0) return@mapNotNull null
                    val key = line.substring(0, idx).trim()
                    var value = line.substring(idx + 1).trim().trim('"', '\'')
                    if (key !in MIRROR_KEYS || value.isEmpty()) null else key to value
                }
                .toMap()
        } catch (e: Exception) {
            emptyMap()
        }
    }

    /** 三个都为空时删掉文件，等于「未配置，直连上游」。 */
    fun writeRegistryMirrors(
        ctx: Context,
        docker: String,
        k8s: String,
        quay: String,
    ): Boolean = try {
        val file = registryMirrorsFile(ctx)
        val entries = linkedMapOf(MIRROR_DOCKER to docker, MIRROR_K8S to k8s, MIRROR_QUAY to quay)
            .filterValues { it.isNotEmpty() }
        if (entries.isEmpty()) {
            file.delete()
        } else {
            ctx.filesDir.mkdirs()
            val body = buildString {
                // ASCII only: the file is written as US-ASCII for the Rust parser.
                appendLine("# written by Settings; read by the agent at startup")
                entries.forEach { (k, v) -> appendLine("$k=$v") }
            }
            file.writeText(body, Charsets.US_ASCII)
        }
        true
    } catch (e: Exception) {
        false
    }

    // ── 网络接口偏好 ──────────────────────────────────────────────────────

    private const val NETWORK_PREF_ENV = "network-preference.env"
    private const val KEY_NODE_IFACE = "DROIDNODE_NODE_IFACE"

    fun networkPreferenceFile(ctx: Context): File = File(ctx.filesDir, NETWORK_PREF_ENV)

    /** 空串表示自动选择；agent 启动时读这个文件。 */
    fun nodeIface(ctx: Context): String = try {
        val file = networkPreferenceFile(ctx)
        if (file.exists()) {
            file.readLines()
                .map { it.trim() }
                .firstOrNull { it.startsWith("$KEY_NODE_IFACE=") }
                ?.substringAfter('=')
                ?.trim()
                .orEmpty()
        } else {
            ""
        }
    } catch (e: Exception) {
        ""
    }

    fun setNodeIface(ctx: Context, iface: String): Boolean = try {
        val file = networkPreferenceFile(ctx)
        if (iface.isBlank()) {
            file.delete()
        } else {
            ctx.filesDir.mkdirs()
            // ASCII only: the file is parsed by the Rust side.
            val body = "# written by Settings; read by the agent at startup\n" +
                "$KEY_NODE_IFACE=${iface.trim()}\n"
            file.writeText(body, Charsets.US_ASCII)
        }
        true
    } catch (e: Exception) {
        false
    }

    /** 设备上的 IPv4 网卡（名字 → 地址），用于「网络」设置里让用户挑一张。 */
    fun listIpv4Interfaces(): List<Pair<String, String>> = try {
        NetworkInterface.getNetworkInterfaces()
            .toList()
            .filter { it.isUp && !it.isLoopback }
            .flatMap { nif ->
                nif.inetAddresses.toList()
                    .filterIsInstance<Inet4Address>()
                    .map { nif.name to it.hostAddress.orEmpty() }
            }
            .filter { it.second.isNotEmpty() }
            .distinctBy { it.second }
    } catch (e: Exception) {
        emptyList()
    }

    /** Wi-Fi Direct / USB 共享 / 隧道：地址通常是私有的，但集群路由不到。 */
    fun isTetherLike(name: String): Boolean {
        val n = name.lowercase()
        return listOf("p2p", "rndis", "usb", "tun", "tap").any { n.startsWith(it) }
    }



    // ── 加入集群：server + token (+ CA) → kubeconfig ───────────────────────

    /** 补 `https://` 前缀、去掉结尾斜杠；空串原样返回。 */
    fun normalizeServer(raw: String): String {
        var s = raw.trim()
        if (s.isEmpty()) return ""
        if (!s.startsWith("http://") && !s.startsWith("https://")) s = "https://$s"
        return s.trimEnd('/')
    }

    /** 用 server + token + CA(PEM) 拼一份最小 kubeconfig（与手工导入的格式一致）。 */
    fun buildKubeconfig(server: String, token: String, caPem: String): String {
        val caB64 = Base64.encodeToString(caPem.toByteArray(), Base64.NO_WRAP)
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

    // ── F2: kubeconfig encrypted at rest ───────────────────────────────────
    private const val KUBECONFIG_ENC = "kubeconfig.enc"
    private const val KUBECONFIG_LEGACY = "kubeconfig"
    private const val KUBECONFIG_RUNTIME = "kubeconfig.runtime"

    /** Encrypted file shown in Settings / used for persistence. */
    fun kubeconfigFile(ctx: Context): File = File(ctx.filesDir, KUBECONFIG_ENC)

    private fun legacyKubeconfigFile(ctx: Context): File =
        File(ctx.filesDir, KUBECONFIG_LEGACY)

    /**
     * Decrypt for the settings/test UI. Also migrates the old plaintext
     * `files/kubeconfig` to `files/kubeconfig.enc` on first read.
     */
    fun readKubeconfig(ctx: Context): String? = try {
        val enc = kubeconfigFile(ctx)
        when {
            enc.exists() -> KubeconfigCrypto.decrypt(enc.readBytes())
            legacyKubeconfigFile(ctx).exists() -> {
                val text = legacyKubeconfigFile(ctx).readText()
                writeKubeconfig(ctx, text)
                text
            }
            else -> null
        }
    } catch (e: Exception) {
        null
    }

    fun writeKubeconfig(ctx: Context, text: String): Boolean = try {
        ctx.filesDir.mkdirs()
        val enc = KubeconfigCrypto.encrypt(text)
        kubeconfigFile(ctx).writeBytes(enc)
        // Only remove the legacy plaintext after the encrypted copy is durable.
        legacyKubeconfigFile(ctx).delete()
        true
    } catch (e: Exception) {
        false
    }

    /**
     * Write the plaintext only for the running agent, to a private file that is
     * deleted when the service stops. The agent needs a path (KUBECONFIG), not
     * an inline blob, so this is the minimum-exposure handoff.
     */
    fun materializeKubeconfig(ctx: Context): File? {
        return try {
            val text = readKubeconfig(ctx) ?: return null
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

    fun deleteRuntimeKubeconfig(ctx: Context) {
        File(ctx.filesDir, KUBECONFIG_RUNTIME).delete()
    }

    /** 只取 `server:` 一行，够用来显示「控制面地址」。 */
    fun serverFrom(kubeconfig: String?): String? {
        if (kubeconfig.isNullOrBlank()) return null
        return Regex("(?m)^\\s*server:\\s*(\\S+)").find(kubeconfig)?.groupValues?.get(1)
    }

    /** 从 kubeconfig 文本推断身份类型，给设置页「身份」一行用。 */
    fun authLabel(kubeconfig: String?): String = when {
        kubeconfig.isNullOrBlank() -> "—"
        Regex("(?m)^\\s*token:\\s*\\S+").containsMatchIn(kubeconfig) -> "token"
        kubeconfig.contains("client-certificate-data") -> Tr.s("客户端证书")
        else -> Tr.s("无凭据")
    }
}
