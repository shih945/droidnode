package com.droidnode.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ValidationTest {

    @Test
    fun nodeNamesFollowDns1123() {
        for (name in listOf("dn-k10", "droidnode-20pro", "a", "node.1", "k3s-main")) {
            assertTrue("$name should be valid", Validation.validNodeName(name))
        }
        for (name in listOf("Dn-K10", "-node", "node-", "node_1", "节点", "a".repeat(254))) {
            assertFalse("$name should be invalid", Validation.validNodeName(name))
        }
    }

    @Test
    fun emptyNodeNameIsAllowed() {
        assertTrue(Validation.validNodeName("   "))
    }

    @Test
    fun portRangeIsUnprivilegedOnly() {
        assertEquals(1024, Validation.parsePort("1024"))
        assertEquals(65535, Validation.parsePort(" 65535 "))
        assertNull(Validation.parsePort("1023"))
        assertNull(Validation.parsePort("65536"))
        assertNull(Validation.parsePort("abc"))
        assertNull(Validation.parsePort(""))
    }

    @Test
    fun logLevelMustBeKnown() {
        val levels = setOf("error", "warn", "info", "debug", "trace")
        assertTrue(Validation.validLogLevel("INFO", levels))
        assertFalse(Validation.validLogLevel("verbose", levels))
    }
}
