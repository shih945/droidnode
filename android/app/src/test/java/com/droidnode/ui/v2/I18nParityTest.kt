package com.droidnode.ui.v2

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/**
 * The app ships Chinese as the default `values/` and English in `values-en/`.
 * A missing translation silently falls back to Chinese, so key parity is the
 * cheapest guard against half-translated screens.
 */
class I18nParityTest {

    private fun resFile(relative: String): File {
        val candidates = listOf(
            File("src/main/res/$relative"),
            File("app/src/main/res/$relative"),
        )
        return candidates.firstOrNull { it.exists() }
            ?: error("resource file not found: $relative (cwd=${File(".").absolutePath})")
    }

    private fun keys(relative: String): Set<String> {
        val text = resFile(relative).readText()
        return Regex("<string name=\"([^\"]+)\"").findAll(text)
            .map { it.groupValues[1] }
            .toSet()
    }

    @Test
    fun chineseAndEnglishDefineTheSameKeys() {
        val zh = keys("values/strings.xml")
        val en = keys("values-en/strings.xml")
        assertTrue("default strings.xml looks empty", zh.size > 100)
        assertEquals("missing English keys: ${zh - en}", emptySet<String>(), zh - en)
        assertEquals("extra English keys: ${en - zh}", emptySet<String>(), en - zh)
    }

    @Test
    fun everyKeyNameIsUniqueAndLowercase() {
        val text = resFile("values/strings.xml").readText()
        val names = Regex("<string name=\"([^\"]+)\"").findAll(text).map { it.groupValues[1] }.toList()
        assertEquals("duplicate string names", names.size, names.toSet().size)
        val bad = names.filter { it != it.lowercase() }
        assertEquals("non-lowercase string names: $bad", emptyList<String>(), bad)
    }
}
