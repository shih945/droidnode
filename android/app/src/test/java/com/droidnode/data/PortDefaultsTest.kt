package com.droidnode.data

import org.junit.Assert.assertEquals
import org.junit.Test

/** Locks the port value shared with executor::config::DEFAULT_KUBELET_PORT. */
class PortDefaultsTest {
    @Test
    fun defaultKubeletPortMatchesRust() {
        assertEquals(10255, AppPrefs.DEFAULT_KUBELET_PORT)
    }
}