package com.droidnode.ui.v2

/**
 * Pod list filtering rules, kept as plain functions so they can be unit-tested
 * without an Activity. `phase` is the kubelet phase; `waitingReason` is the
 * container-level reason (`ErrImagePull`, `CrashLoopBackOff`, ...).
 */
object PodFilters {

    fun isRunning(pod: PodRow): Boolean = pod.phase.startsWith("Running")

    fun isBad(pod: PodRow): Boolean =
        pod.phase.contains("CrashLoop") ||
            pod.phase.contains("ImagePull") ||
            pod.phase == "Failed" ||
            pod.waitingReason?.contains("ImagePull") == true ||
            pod.waitingReason == "CrashLoopBackOff"

    /** Sort key: running/succeeded last, pending in the middle, failures first. */
    fun severity(pod: PodRow): Int = when {
        pod.phase.startsWith("Running") || pod.phase.startsWith("Succeeded") -> 2
        pod.phase == "Pending" -> 1
        else -> 0
    }
}
