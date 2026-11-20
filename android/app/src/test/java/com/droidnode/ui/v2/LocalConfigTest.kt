package com.droidnode.ui.v2

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.Base64

/** Pure helpers from LocalConfig: string normalisation and kubeconfig templating. */
class LocalConfigTest {

    @Test
    fun serverGetsHttpsAndNoTrailingSlash() {
        assertEquals("https://192.0.2.10:6443", LocalConfig.normalizeServer(" 192.0.2.10:6443 "))
        assertEquals("https://a.example:6443", LocalConfig.normalizeServer("https://a.example:6443/"))
        assertEquals("http://a.example:8080", LocalConfig.normalizeServer("http://a.example:8080/"))
    }

    @Test
    fun emptyServerStaysEmpty() {
        assertEquals("", LocalConfig.normalizeServer("   "))
    }

    @Test
    fun mirrorHostIsLowercasedAndStripped() {
        assertEquals("docker.io", LocalConfig.normalizeMirrorHost("https://Docker.IO/"))
        assertEquals("mirror.local:5000", LocalConfig.normalizeMirrorHost("mirror.local:5000"))
        assertEquals("", LocalConfig.normalizeMirrorHost("  "))
    }

    @Test
    fun invalidMirrorHostIsRejected() {
        assertNull(LocalConfig.normalizeMirrorHost("bad host!"))
    }

    @Test
    fun tetherInterfacesAreDetected() {
        for (name in listOf("p2p0", "rndis0", "usb0", "tun0", "tap1", "P2P0")) {
            assertTrue("$name should be tether-like", LocalConfig.isTetherLike(name))
        }
        for (name in listOf("wlan0", "eth0", "rmnet_data0")) {
            assertFalse("$name should not be tether-like", LocalConfig.isTetherLike(name))
        }
    }

    @Test
    fun kubeconfigEmbedsServerTokenAndCa() {
        val ca = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n"
        val text = LocalConfig.buildKubeconfig("https://192.0.2.10:6443/", "tok-123", ca)

        assertTrue(text.contains("server: https://192.0.2.10:6443"))
        assertTrue(text.contains("token: tok-123"))
        assertTrue(text.contains("current-context: droidnode"))
        assertTrue(text.contains("user: droidnode-agent"))

        val encoded = text.lineSequence()
            .first { it.trimStart().startsWith("certificate-authority-data:") }
            .substringAfter(':')
            .trim()
        assertEquals(ca, String(Base64.getDecoder().decode(encoded)))
    }
}
