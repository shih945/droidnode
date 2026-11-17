package com.droidnode.brokers

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File

/**
 * Regression for the "join a cluster, then the agent exits with failed to infer
 * configuration" bug: the plaintext kubeconfig path used to be captured once in
 * the service's onCreate, so an ACTION_RESTART after joining kept pointing the
 * agent at the pre-join fallback path. It must be resolved on every spawn.
 */
class RustProcessBrokerTest {

    @get:Rule
    val tmp = TemporaryFolder()

    private val launch = AgentLaunchConfig(nodeId = "node-a", kubeletPort = 10255, logLevel = "info")

    private fun broker(dataDir: File, kubeConfig: () -> File?) = RustProcessBroker(
        binaryPath = File(dataDir, "libnode_agent.so"),
        prootPath = File(dataDir, "libproot.so"),
        dataDir = dataDir,
        cacheDir = File(dataDir, "cache"),
        codeCacheDir = File(dataDir, "code_cache"),
        nativeLibDir = "/native/libs",
        kubeConfig = kubeConfig,
        config = { launch },
    )

    @Test
    fun kubeconfigIsResolvedAgainForEverySpawn() {
        val dataDir = tmp.newFolder("files")
        var joined: File? = null
        val broker = broker(dataDir) { joined }

        // Started before the user joined: no KUBECONFIG at all — pointing it at a
        // path that does not exist is what made the agent report a bogus
        // "kubeconfig.runtime: No such file or directory" right after a join.
        assertFalse(broker.environmentFor(launch).containsKey("KUBECONFIG"))

        val runtime = File(dataDir, "node/config/kubeconfig.runtime")
        runtime.parentFile!!.mkdirs()
        runtime.writeText("apiVersion: v1\n")
        joined = runtime

        assertEquals(runtime.absolutePath, broker.environmentFor(launch)["KUBECONFIG"])
    }

    @Test
    fun pathsFollowTheNodeStorageLayout() {
        val dataDir = tmp.newFolder("files")
        val env = broker(dataDir) { null }.environmentFor(launch)

        assertEquals(File(dataDir, "node").absolutePath, env["DROIDNODE_DATA_DIR"])
        assertEquals(File(dataDir, "node/config").absolutePath, env["DROIDNODE_CONFIG_DIR"])
        assertEquals(File(dataDir, "node/layers").absolutePath, env["DROIDNODE_LAYERS_DIR"])
        assertEquals(File(dataDir, "node/rootfs").absolutePath, env["DROIDNODE_ROOTFS_DIR"])
        assertEquals("10255", env["DROIDNODE_KUBELET_PORT"])
        assertEquals("node-a", env["DROIDNODE_NODE_ID"])
        assertEquals("info", env["RUST_LOG"])
    }

    @Test
    fun runtimeDirectoriesAreCreatedBeforeTheAgentStarts() {
        val dataDir = tmp.newFolder("files")
        broker(dataDir) { null }.environmentFor(launch)

        for (relative in listOf("node/config", "node/layers", "node/rootfs", "code_cache/tmp")) {
            assertTrue("$relative should exist", File(dataDir, relative).isDirectory)
        }
    }
}
