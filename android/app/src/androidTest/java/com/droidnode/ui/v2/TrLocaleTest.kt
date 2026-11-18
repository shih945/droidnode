package com.droidnode.ui.v2

import android.content.Context
import android.content.res.Configuration
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import java.util.Locale

/**
 * Switching the language must change what `Tr.s()` renders (not silently
 * fall back to Chinese). Runs on a device because resources are involved.
 */
@RunWith(AndroidJUnit4::class)
class TrLocaleTest {

    private fun contextWith(base: Context, locale: Locale): Context {
        val config = Configuration(base.resources.configuration)
        config.setLocale(locale)
        return base.createConfigurationContext(config)
    }

    @Test
    fun chineseAndEnglishRenderDifferently() {
        val app = ApplicationProvider.getApplicationContext<Context>()
        val zh = contextWith(app, Locale.SIMPLIFIED_CHINESE)
        val en = contextWith(app, Locale.ENGLISH)

        Tr.bind(zh)
        val zhText = Tr.s("运行中")
        assertEquals("运行中", zhText)

        Tr.bind(en)
        val enText = Tr.s("运行中")
        assertEquals("Running", enText)
        assertEquals("Node running", Tr.s("节点运行中"))
    }
}
