package com.droidnode.data

/**
 * Pure validation rules shared by the settings and join screens. Kept free of
 * Android APIs so they can be unit-tested on the JVM.
 */
object Validation {

    /** k8s node name: lowercase alphanumerics, '-' and '.', max 253 chars. */
    private val NODE_NAME = Regex("^[a-z0-9]([-a-z0-9.]*[a-z0-9])?$")

    /** Empty is allowed (the agent then falls back to a generated name). */
    fun validNodeName(raw: String): Boolean {
        val name = raw.trim()
        if (name.isEmpty()) return true
        return name.length <= 253 && NODE_NAME.matches(name)
    }

    /** kubelet port must be unprivileged: 1024..65535. */
    fun parsePort(raw: String): Int? =
        raw.trim().toIntOrNull()?.takeIf { it in 1024..65535 }

    fun validLogLevel(raw: String, levels: Set<String>): Boolean =
        raw.trim().lowercase() in levels
}
