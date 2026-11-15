package com.droidnode.data

/**
 * Parser/serialiser for `$filesDir/registry-mirrors.env`, the file the agent
 * reads at startup. Format is deliberately trivial (`KEY=VALUE` lines, `#`
 * comments, US-ASCII) so the Rust side does not need a YAML parser.
 */
object RegistryMirrorStore {

    const val DOCKER = "DROIDNODE_REGISTRY_MIRROR"
    const val K8S = "DROIDNODE_REGISTRY_MIRROR_K8S"
    const val QUAY = "DROIDNODE_REGISTRY_MIRROR_QUAY"

    val KEYS = listOf(DOCKER, K8S, QUAY)

    /**
     * Unknown keys, blank values and malformed lines are ignored; duplicate
     * keys keep the last value (matching a shell `source` of the file).
     */
    fun parse(text: String): Map<String, String> {
        val out = LinkedHashMap<String, String>()
        text.lineSequence().forEach { rawLine ->
            val line = rawLine.trim()
            if (line.isEmpty() || line.startsWith("#")) return@forEach
            val idx = line.indexOf('=')
            if (idx <= 0) return@forEach
            val key = line.substring(0, idx).trim()
            val value = line.substring(idx + 1).trim().trim('"', '\'')
            if (key in KEYS && value.isNotEmpty()) out[key] = value
        }
        return out
    }

    /** `null` when nothing is configured — callers then delete the file. */
    fun render(entries: Map<String, String>): String? {
        val ordered = KEYS.mapNotNull { key -> entries[key]?.takeIf { it.isNotEmpty() }?.let { key to it } }
        if (ordered.isEmpty()) return null
        return buildString {
            appendLine("# written by Settings; read by the agent at startup")
            ordered.forEach { (key, value) -> appendLine("$key=$value") }
        }
    }
}
