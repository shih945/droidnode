package com.droidnode.ui.v2

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class NodeRepositoryTest {

    private class FakeLoader : NodeRepository.Loader {
        var podList: List<PodRow>? = null
        var stats: NodeStats? = null
        var imageList: List<ImageRow>? = null
        var healthy = false
        var imageCalls = 0

        override fun pods() = podList
        override fun stats() = stats
        override fun images(): List<ImageRow>? {
            imageCalls++
            return imageList
        }
        override fun health() = healthy
    }

    private fun pod(name: String) = PodRow(
        name = name,
        namespace = "default",
        container = "main",
        phase = "Running",
        ready = true,
        restarts = 0,
        startedAt = null,
        podIp = null,
        message = null,
    )

    @Test
    fun firstRefreshPublishesSnapshot() = runBlocking {
        val loader = FakeLoader().apply {
            podList = listOf(pod("a"))
            stats = NodeStats(1, 2, 3, 4, 5)
            healthy = true
        }
        val repo = NodeRepository(loader)
        repo.refresh()

        val state = repo.state.value
        assertEquals(1, state.pods.size)
        assertEquals("a", state.pods[0].name)
        assertTrue(state.health)
        assertEquals(1L, state.ticks)
    }

    @Test
    fun transientPodFetchKeepsPreviousData() = runBlocking {
        val loader = FakeLoader().apply {
            podList = listOf(pod("a"))
            healthy = true
        }
        val repo = NodeRepository(loader)
        repo.refresh()

        // agent 还活着，只是这一次 /pods 没拿到：保留上一份，别把页面刷空
        loader.podList = null
        repo.refresh()

        val state = repo.state.value
        assertEquals(1, state.pods.size)
        assertTrue(state.health)
        assertEquals(2L, state.ticks)
    }

    @Test
    fun deadAgentClearsSnapshot() = runBlocking {
        val loader = FakeLoader().apply {
            podList = listOf(pod("a"))
            stats = NodeStats(1, 2, 3, 4, 5)
            healthy = true
        }
        val repo = NodeRepository(loader)
        repo.refresh()

        // agent 停掉：健康探测失败，此时列表里的数据已经无从校验，必须清空
        loader.podList = null
        loader.stats = null
        loader.healthy = false
        repo.refresh()

        val state = repo.state.value
        assertTrue(state.pods.isEmpty())
        assertEquals(null, state.stats)
        assertFalse(state.health)
        assertEquals(2L, state.ticks)
    }

    @Test
    fun imagesRefreshEveryThirdTickOnly() = runBlocking {
        val loader = FakeLoader().apply {
            imageList = listOf(
                ImageRow("alpine", 1, 2, 3, "sha256:x", "docker.io", "now", "/rootfs"),
            )
            healthy = true
        }
        val repo = NodeRepository(loader)
        repo.refresh()
        repo.refresh()
        assertTrue(repo.state.value.images.isEmpty())
        assertEquals(0, loader.imageCalls)

        repo.refresh()
        assertEquals(1, loader.imageCalls)
        assertEquals(1, repo.state.value.images.size)
    }
}
