package com.droidnode.ui.v2

/**
 * Pure parsers for the agent log lines the settings page prints. Kept free of
 * Context/Tr so they can be unit-tested; the Activity adds the localized text.
 */
object SettingsText {

    private val CONFIGURED =
        Regex("registry mirrors configured docker=(\\S*) k8s=(\\S*) quay=(\\S*)")
    private val APPLIED = Regex("registry mirror applied.*?to=(\\S+)")

    sealed class Registry {
        data class Configured(val docker: String, val k8s: String, val quay: String) : Registry()
        data class Applied(val target: String) : Registry()
        object None : Registry()
    }

    /** Newest matching line wins; `null` when the agent has not logged anything yet. */
    fun registryFromLogs(texts: List<String>): Registry? {
        for (text in texts.asReversed()) {
            CONFIGURED.find(text)?.let {
                return Registry.Configured(it.groupValues[1], it.groupValues[2], it.groupValues[3])
            }
            if (text.contains("registry mirrors: none")) return Registry.None
            APPLIED.find(text)?.let { return Registry.Applied(it.groupValues[1]) }
        }
        return null
    }
}
