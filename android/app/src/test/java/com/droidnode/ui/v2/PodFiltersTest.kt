package com.droidnode.ui.v2

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PodFiltersTest {

    private fun pod(phase: String, waitingReason: String? = null) = PodRow(
        name = "p",
        namespace = "ns",
        container = "c",
        phase = phase,
        ready = phase == "Running",
        restarts = 0,
        startedAt = null,
        podIp = null,
        message = null,
        waitingReason = waitingReason,
    )

    @Test
    fun runningPodIsNotAbnormal() {
        assertTrue(PodFilters.isRunning(pod("Running")))
        assertFalse(PodFilters.isBad(pod("Running")))
    }

    @Test
    fun pendingPlainIsNotAbnormal() {
        // ContainerCreating: worth showing, but not a failure yet.
        assertFalse(PodFilters.isBad(pod("Pending", "ContainerCreating")))
    }

    @Test
    fun imagePullWaitingReasonIsAbnormal() {
        assertTrue(PodFilters.isBad(pod("Pending", "ErrImagePull")))
        assertTrue(PodFilters.isBad(pod("Pending", "ImagePullBackOff")))
    }

    @Test
    fun crashLoopWaitingReasonIsAbnormal() {
        assertTrue(PodFilters.isBad(pod("Running", "CrashLoopBackOff")))
    }

    @Test
    fun failedPhaseIsAbnormal() {
        assertTrue(PodFilters.isBad(pod("Failed")))
    }

    @Test
    fun severitySortsFailuresBeforePendingBeforeRunning() {
        assertTrue(PodFilters.severity(pod("Failed")) < PodFilters.severity(pod("Pending")))
        assertTrue(PodFilters.severity(pod("Pending")) < PodFilters.severity(pod("Running")))
        assertEquals(PodFilters.severity(pod("Running")), PodFilters.severity(pod("Succeeded")))
    }
}
