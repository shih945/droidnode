package com.droidnode.brokers

import android.util.Log
import java.io.File

private const val TAG = "RustProcessBroker"

/** Identity/knobs that can change from the settings page (E14). */
data class AgentLaunchConfig(
    val nodeId: String,
    val kubeletPort: Int,
    val logLevel: String,
)

/**
 * Broker: spawns, monitors, and kills the Rust node-agent subprocess.
 * No retry logic, no restart logic — that belongs in AgentLifecycleService.
 *
 * [binaryPath]  — path to libnode_agent.so in nativeLibraryDir (executable on all API levels)
 * [prootPath]   — path to libproot.so in nativeLibraryDir
 * [dataDir]     — app's filesDir; holds kubeconfig, layers, rootfs, CA key
 * [cacheDir]    — app's cacheDir; used as TMPDIR for proot child processes
 * [config]      — read on every spawn, so a settings change + agent restart takes effect
 */
class RustProcessBroker(
    private val binaryPath: File,
    private val prootPath: File,
    private val dataDir: File,
    private val cacheDir: File,
    private val codeCacheDir: File,
    private val nativeLibDir: String,
    private val kubeConfigPath: File,
    private val config: () -> AgentLaunchConfig,
) {
    private var process: Process? = null

    /** Spawn the Rust binary and return the Process handle. Throws on failure. */
    fun spawn(): Process {
        if (!binaryPath.exists()) error("node-agent binary not found at ${binaryPath.absolutePath}")
        // Read on every spawn: node name / port / log level are editable at runtime.
        val launch = config()

        val layersDir = File(dataDir, "layers").also { it.mkdirs() }
        // Unpacked rootfs lives under files/, NOT code_cache/: Android treats the
        // cache dir as disposable and silently wipes it, which made every image
        // disappear from the image list and forced a re-unpack after a while.
        val rootfsDir = File(dataDir, "rootfs").also { it.mkdirs() }
        val tmpDir = File(codeCacheDir, "tmp").also { it.mkdirs() }

        val pb = ProcessBuilder(binaryPath.absolutePath)
            .directory(dataDir)
            .redirectErrorStream(true)   // merge stderr into stdout so we capture all output
            .apply {
                environment().apply {
                    // Identity (E14: editable in settings)
                    put("DROIDNODE_NODE_ID", launch.nodeId)
                    // Paths
                    put("DROIDNODE_PROOT_PATH", prootPath.absolutePath)
                    put("DROIDNODE_LAYERS_DIR", layersDir.absolutePath)
                    put("DROIDNODE_ROOTFS_DIR", rootfsDir.absolutePath)
                    put("DROIDNODE_DATA_DIR", dataDir.absolutePath)
                    // Network
                    put("DROIDNODE_KUBELET_PORT", launch.kubeletPort.toString())
                    put("KUBECONFIG", kubeConfigPath.absolutePath)
                    // Runtime dirs — Rust stdlib and proot child processes need these
                    put("HOME", dataDir.absolutePath)
                    put("TMPDIR", tmpDir.absolutePath)
                    // proot is patched to look for libtalloc2.so via $ORIGIN RPATH,
                    // but set LD_LIBRARY_PATH as a fallback for the dynamic linker.
                    put("LD_LIBRARY_PATH", nativeLibDir)
                    // Logging (E14: configurable from the settings page)
                    put("RUST_LOG", launch.logLevel)
                }
            }

        val proc = pb.start()
        process = proc
        Log.i(TAG, "Rust agent started")
        return proc
    }

    /** Kill the current process immediately. Safe to call if not running. */
    fun kill() {
        process?.let {
            it.destroyForcibly()
            Log.i(TAG, "Rust agent killed")
        }
        process = null
    }

    /** Returns the exit value if the process has terminated, null if still running. */
    fun exitCode(): Int? {
        val p = process ?: return null
        return try {
            p.exitValue()
        } catch (_: IllegalThreadStateException) {
            null
        }
    }

    /** True if the agent process is alive. */
    fun isAlive(): Boolean = process?.isAlive == true
}
