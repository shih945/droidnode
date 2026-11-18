package com.droidnode.data

import android.content.Context
import android.provider.Settings

/**
 * Everything stored in the `droidnode_console` SharedPreferences: appearance,
 * language, autostart, drain marker and the node settings the agent reads as
 * environment variables at startup.
 */
object AppPrefs {

    private const val PREFS = "droidnode_console"
    private const val KEY_DRAINED_AT = "drained_requested_at"
    private const val KEY_NODE_NAME = "node_name"
    private const val KEY_KUBELET_PORT = "kubelet_port"
    private const val KEY_LOG_LEVEL = "log_level"
    private const val KEY_APPEARANCE = "appearance"
    private const val KEY_AUTOSTART = "autostart"
    private const val KEY_KEEP_ONLINE = "keep_online"
    private const val KEY_LANGUAGE = "language"

    /** Empty string = follow the system; otherwise a BCP-47 tag. */
    const val LANGUAGE_SYSTEM = ""
    const val LANGUAGE_ZH = "zh"
    const val LANGUAGE_EN = "en"

    const val DEFAULT_KUBELET_PORT = 10255
    const val DEFAULT_LOG_LEVEL = "info"
    val LOG_LEVELS = setOf("error", "warn", "info", "debug", "trace")

    const val APPEARANCE_SYSTEM = "system"
    const val APPEARANCE_LIGHT = "light"
    const val APPEARANCE_DARK = "dark"
    val APPEARANCES = listOf(APPEARANCE_SYSTEM, APPEARANCE_LIGHT, APPEARANCE_DARK)

    private fun prefs(ctx: Context) = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    // ── appearance (light by default: the phone's screen is on all day) ─────
    fun appearance(ctx: Context): String {
        val mode = prefs(ctx).getString(KEY_APPEARANCE, APPEARANCE_LIGHT).orEmpty()
        return if (mode in APPEARANCES) mode else APPEARANCE_LIGHT
    }

    fun setAppearance(ctx: Context, mode: String) {
        val value = if (mode in APPEARANCES) mode else APPEARANCE_LIGHT
        prefs(ctx).edit().putString(KEY_APPEARANCE, value).apply()
    }

    fun language(ctx: Context): String =
        prefs(ctx).getString(KEY_LANGUAGE, LANGUAGE_SYSTEM).orEmpty()

    fun setLanguage(ctx: Context, tag: String) {
        prefs(ctx).edit().putString(KEY_LANGUAGE, tag).apply()
    }

    fun autostart(ctx: Context): Boolean = prefs(ctx).getBoolean(KEY_AUTOSTART, true)

    fun setAutostart(ctx: Context, on: Boolean) {
        prefs(ctx).edit().putBoolean(KEY_AUTOSTART, on).apply()
    }

    /**
     * Hold the wake lock even when no Pod is running.
     *
     * On by default: releasing it lets the device sleep, the heartbeat stops and
     * the control plane marks the node NotReady after ~40 s. Users who care more
     * about battery than about staying schedulable can turn it off.
     */
    fun keepOnline(ctx: Context): Boolean = prefs(ctx).getBoolean(KEY_KEEP_ONLINE, true)

    fun setKeepOnline(ctx: Context, on: Boolean) {
        prefs(ctx).edit().putBoolean(KEY_KEEP_ONLINE, on).apply()
    }

    fun drained(ctx: Context): Boolean = prefs(ctx).getLong(KEY_DRAINED_AT, 0L) > 0L

    fun setDrained(ctx: Context, drained: Boolean) {
        prefs(ctx).edit()
            .putLong(KEY_DRAINED_AT, if (drained) System.currentTimeMillis() else 0L)
            .apply()
    }

    // ── node identity / kubelet port / log level ────────────────────────────
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
    fun resolveNodeId(ctx: Context): String = nodeName(ctx).ifBlank { autoNodeId(ctx) }

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
}
