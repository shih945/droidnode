package com.droidnode.ui.v2

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith

/** Parsing contract between the agent's /pods JSON and the row model. */
@RunWith(AndroidJUnit4::class)
class SnapshotParseTest {

    @Test
    fun pendingPodKeepsItsWaitingReason() {
        val json = JSONObject(
            """
            {"items":[{
              "metadata":{"name":"slow","namespace":"apps"},
              "status":{
                "phase":"Pending",
                "message":"back-off pulling image \"x\"",
                "containerStatuses":[{
                  "name":"main","ready":false,
                  "state":{"waiting":{"reason":"ErrImagePull","message":"nope"}}
                }]
              }
            }]}
            """.trimIndent(),
        )
        val pods = Snapshot.parsePods(json)
        assertEquals(1, pods.size)
        assertEquals("Pending", pods[0].phase)
        assertEquals("ErrImagePull", pods[0].waitingReason)
        assertEquals("back-off pulling image \"x\"", pods[0].message)
    }

    @Test
    fun imagePullReasonRendersAsFailure() {
        val ctx = ApplicationProvider.getApplicationContext<Context>()
        Tr.bind(ctx)
        assertEquals("拉取失败", Ui.phaseLabel("Pending", "ErrImagePull"))
        assertEquals("崩溃重启", Ui.phaseLabel("Running", "CrashLoopBackOff"))
        assertEquals("等待调度", Ui.phaseLabel("Pending"))
    }

    @Test
    fun runningPodHasNoWaitingReason() {
        val json = JSONObject(
            """
            {"items":[{
              "metadata":{"name":"fast","namespace":"apps"},
              "status":{
                "phase":"Running",
                "containerStatuses":[{
                  "name":"main","ready":true,"restartCount":1,
                  "state":{"running":{"startedAt":"2026-11-15T12:00:00Z"}}
                }]
              }
            }]}
            """.trimIndent(),
        )
        val pods = Snapshot.parsePods(json)
        assertEquals(1, pods.size)
        assertEquals("Running", pods[0].phase)
        assertNull(pods[0].waitingReason)
    }
}
