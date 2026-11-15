package com.droidnode.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class RegistryMirrorStoreTest {

    @Test
    fun parsesKeysAndIgnoresCommentsAndJunk() {
        val text = """
            # written by Settings; read by the agent at startup
            DROIDNODE_REGISTRY_MIRROR=mirror.local:5000
            DROIDNODE_REGISTRY_MIRROR_K8S="k8s.mirror.local"
            unrelated=value
            not-a-pair
            DROIDNODE_REGISTRY_MIRROR_QUAY=
        """.trimIndent()
        val parsed = RegistryMirrorStore.parse(text)
        assertEquals("mirror.local:5000", parsed[RegistryMirrorStore.DOCKER])
        assertEquals("k8s.mirror.local", parsed[RegistryMirrorStore.K8S])
        assertEquals(2, parsed.size)
    }

    @Test
    fun lastDuplicateWins() {
        val text = """
            DROIDNODE_REGISTRY_MIRROR=first
            DROIDNODE_REGISTRY_MIRROR=second
        """.trimIndent()
        assertEquals("second", RegistryMirrorStore.parse(text)[RegistryMirrorStore.DOCKER])
    }

    @Test
    fun renderReturnsNullWhenNothingIsConfigured() {
        assertNull(RegistryMirrorStore.render(emptyMap()))
        assertNull(RegistryMirrorStore.render(mapOf(RegistryMirrorStore.DOCKER to "")))
    }

    @Test
    fun renderStaysInKeyOrderAndIsAsciiSafe() {
        val body = RegistryMirrorStore.render(
            mapOf(
                RegistryMirrorStore.QUAY to "quay.mirror:5000",
                RegistryMirrorStore.DOCKER to "docker.mirror:5000",
            ),
        )!!
        val lines = body.trim().lines()
        assertTrue(lines.first().startsWith("#"))
        assertEquals("DROIDNODE_REGISTRY_MIRROR=docker.mirror:5000", lines[1])
        assertEquals("DROIDNODE_REGISTRY_MIRROR_QUAY=quay.mirror:5000", lines[2])
        assertEquals(3, lines.size)
        assertTrue(body.all { it.code < 128 })
    }

    @Test
    fun renderRoundTripsThroughParse() {
        val entries = mapOf(
            RegistryMirrorStore.DOCKER to "docker.mirror:5000",
            RegistryMirrorStore.K8S to "k8s.mirror:5000",
            RegistryMirrorStore.QUAY to "quay.mirror:5000",
        )
        assertEquals(entries, RegistryMirrorStore.parse(RegistryMirrorStore.render(entries)!!))
    }
}
