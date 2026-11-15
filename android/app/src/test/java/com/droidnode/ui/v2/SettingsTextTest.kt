package com.droidnode.ui.v2

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class SettingsTextTest {

    @Test
    fun parsesConfiguredMirrorsFromNewestLine() {
        val lines = listOf(
            "registry mirrors configured docker=old.mirror k8s=oldk8s quay=oldquay",
            "registry mirrors configured docker=docker.mirror:5000 k8s=k8s.mirror quay=quay.mirror",
        )
        val parsed = SettingsText.registryFromLogs(lines)
        assertEquals(
            SettingsText.Registry.Configured("docker.mirror:5000", "k8s.mirror", "quay.mirror"),
            parsed,
        )
    }

    @Test
    fun parsesNoneAndAppliedLines() {
        assertEquals(
            SettingsText.Registry.None,
            SettingsText.registryFromLogs(listOf("registry mirrors: none")),
        )
        assertEquals(
            SettingsText.Registry.Applied("docker.mirror:5000/library/alpine:latest"),
            SettingsText.registryFromLogs(
                listOf("registry mirror applied image=alpine to=docker.mirror:5000/library/alpine:latest"),
            ),
        )
    }

    @Test
    fun returnsNullWhenNothingLoggedYet() {
        assertNull(SettingsText.registryFromLogs(emptyList()))
        assertNull(SettingsText.registryFromLogs(listOf("starting droidnode agent", "pod scheduled")))
    }
}
