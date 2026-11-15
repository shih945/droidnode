package com.droidnode.ui.v2

import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * T13 runtime side: every string that goes through Tr.s() must exist as a
 * resource in the current locale, and must not silently fall back to the
 * Chinese key. Runs on a device (resources + locale).
 */
@RunWith(AndroidJUnit4::class)
class TrTest {

    @Test
    fun everyKnownKeyResolvesToAResource() {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        Tr.bind(context)
        val entries = Tr.entriesForTest()
        assertTrue("Tr map is empty", entries.size > 100)
        for ((key, id) in entries) {
            val fromResource = context.getString(id)
            assertTrue("empty resource for $key (id=$id)", fromResource.isNotEmpty())
            val rendered = Tr.s(key)
            assertTrue("empty render for $key", rendered.isNotEmpty())
        }
    }
}
