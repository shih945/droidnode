package com.droidnode.exposers

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.IBinder
import android.util.Log
import androidx.core.content.ContextCompat
import com.droidnode.brokers.AgentLaunchConfig
import com.droidnode.brokers.AgentStatusClient
import com.droidnode.brokers.AgentStatusSnapshot
import com.droidnode.brokers.BatteryBroker
import com.droidnode.brokers.NetworkBroker
import com.droidnode.brokers.RustProcessBroker
import com.droidnode.brokers.WakeLockBroker
import com.droidnode.brokers.WifiLockBroker
import com.droidnode.services.foundation.AgentLifecycleService
import com.droidnode.services.foundation.ResourceGuardService
import com.droidnode.services.orchestration.NodeReadinessService
import com.droidnode.data.AgentApi
import com.droidnode.ui.v2.LocalConfig
import com.droidnode.ui.v2.NodeActivity
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import java.io.File

private const val TAG = "ForegroundServiceExposer"
private const val NOTIFICATION_ID = 1001
private const val CHANNEL_ID = "droidnode_channel"
/// When the user disabled "stay online while locked", keep the wake lock this
/// long after the last pod disappears before releasing it (avoids flapping
/// during a restart). With the default setting the lock is never released.
private const val IDLE_WAKE_LOCK_RELEASE_MS = 60_000L

/**
 * Android Foreground Service entry point.
 * Responsible only for Android lifecycle concerns: notification, wake lock,
 * broker construction, and launching the readiness loop on a coroutine.
 * All execution logic lives in the Rust agent.
 */
class ForegroundServiceExposer : Service() {

    companion object {
        private const val ACTION_RESTART = "com.droidnode.action.RESTART_AGENT"
        private const val ACTION_STOP = "com.droidnode.action.STOP_AGENT"

        @Volatile var isAgentRunning: Boolean = false
        @Volatile var nodeId: String = ""
        @Volatile var startTime: Long = 0L

        /**
         * 导入 kubeconfig 后要让新配置生效，唯一的办法是用同样的
         * 环境变量重开 agent 进程。走前台服务的 onStartCommand 而不是直接
         * kill，是为了让 readiness 循环、battery/wifi lock 保持同一份实例。
         */
        fun requestRestart(context: Context) {
            // 重启隐含"要它继续跑"，清掉用户之前按下的停止意图
            LocalConfig.setAgentStopped(context, false)
            val intent = Intent(context, ForegroundServiceExposer::class.java)
                .setAction(ACTION_RESTART)
            ContextCompat.startForegroundService(context, intent)
        }

        /** Stop the foreground service; onDestroy also kills the agent child. */
        fun requestStop(context: Context) {
            // 记住"用户主动停止"，否则打开页面 / 开机自启会把它重新拉起来
            LocalConfig.setAgentStopped(context, true)
            val intent = Intent(context, ForegroundServiceExposer::class.java)
                .setAction(ACTION_STOP)
            context.startService(intent)
        }
    }

    private val serviceScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    // Only one readiness loop may run: onStartCommand is called again every time
    // the activity starts, and duplicate loops fight over the kubelet port.
    private var readinessJob: Job? = null
    private lateinit var agentLifecycle: AgentLifecycleService

    private lateinit var wakeLockBroker: WakeLockBroker
    private lateinit var wifiLockBroker: WifiLockBroker
    private lateinit var networkBroker: NetworkBroker
    private lateinit var nodeReadinessService: NodeReadinessService
    private lateinit var statusClient: AgentStatusClient
    /// When the agent last reported zero running pods (0 = unknown/busy).
    private var idleSince = 0L
    /// Avoid a request storm while re-applying a persisted maintenance mode.
    private var maintenanceReapplied = false

    override fun onCreate() {
        super.onCreate()
        Log.i(TAG, "service creating")

        createNotificationChannel()

        // ─── Brokers ────────────────────────────────────────────────────────

        wakeLockBroker = WakeLockBroker(this)

        // Keeps the Wi-Fi radio awake when the screen turns off; without it the
        // node flips to NotReady after a few minutes of sleep.
        wifiLockBroker = WifiLockBroker(this)

        networkBroker = NetworkBroker(this).also { it.register() }

        val batteryBroker = BatteryBroker(this)

        // Binaries are packaged as .so files in jniLibs/arm64-v8a/ so the
        // package manager extracts them to nativeLibraryDir with execute permission.
        // filesDir is mounted noexec on API 29+ so we cannot execute from there.
        val nativeDir = applicationInfo.nativeLibraryDir
        val agentBinary = File(nativeDir, "libnode_agent.so")
        val prootBinary = File(nativeDir, "libproot.so")

        if (!agentBinary.exists()) {
            Log.e(TAG, "node-agent binary not found at ${agentBinary.absolutePath} — APK may be missing jniLibs")
        }

        val dataDir = filesDir
        // The framework creates files/ lazily; make sure it exists before anyone
        // tries to `run-as ... > files/kubeconfig` right after install.
        dataDir.mkdirs()
        // Node name / kubelet port / log level are configurable; empty name
        // falls back to the historical "droidnode-<ANDROID_ID>".
        val nodeId = LocalConfig.resolveNodeId(this)
        // The plaintext kubeconfig only exists while the agent runs. It is
        // materialized inside the broker on every spawn (not here, once), because
        // the user can join a cluster while the service is already up: an
        // ACTION_RESTART must pick the new kubeconfig up instead of reusing the
        // path that was resolved when the service started.
        LocalConfig.deleteRuntimeKubeconfig(this)

        val processBroker = RustProcessBroker(
            binaryPath = agentBinary,
            prootPath = prootBinary,
            dataDir = dataDir,
            cacheDir = cacheDir,
            codeCacheDir = codeCacheDir,
            nativeLibDir = nativeDir,
            kubeConfig = { LocalConfig.materializeKubeconfig(this) },
            // Read on every spawn so "save settings + restart" really applies.
            config = {
                AgentLaunchConfig(
                    nodeId = LocalConfig.resolveNodeId(this),
                    kubeletPort = LocalConfig.kubeletPort(this),
                    logLevel = LocalConfig.logLevel(this),
                )
            },
        )

        // Expose node ID globally for the status UI
        ForegroundServiceExposer.nodeId = nodeId

        // ─── Services ───────────────────────────────────────────────────────

        val agentLifecycle = AgentLifecycleService(processBroker) { running ->
            isAgentRunning = running
            if (running) startTime = System.currentTimeMillis()
        }
        this.agentLifecycle = agentLifecycle
        val resourceGuard = ResourceGuardService(batteryBroker, networkBroker)
        nodeReadinessService = NodeReadinessService(agentLifecycle, resourceGuard) {
            LocalConfig.kubeletPort(this)
        }

        // Push battery/network/storage over the Unix socket and keep
        // the structured status the agent sends back (it also drives the
        // power/status display).
        statusClient = AgentStatusClient(
            context = this,
            batteryBroker = batteryBroker,
            networkBroker = networkBroker,
            scope = serviceScope,
            onStatus = ::handleAgentStatus,
        )
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        Log.i(TAG, "service starting")
        if (intent?.action == ACTION_STOP) {
            Log.i(TAG, "stop requested — letting the agent run its shutdown sequence")
            // 停止要等 agent 收尾（最多 12 s），不能在主线程做；收尾完再 stopSelf。
            // 这段时间仍是前台服务，通知不会消失。
            serviceScope.launch {
                try {
                    agentLifecycle.stop()
                } catch (e: Exception) {
                    Log.w(TAG, "agent stop failed: ${e.message}")
                }
                stopSelf()
            }
            return START_NOT_STICKY
        }
        startForeground(NOTIFICATION_ID, buildNotification())
        wakeLockBroker.acquire()
        wifiLockBroker.acquire()
        statusClient.start()

        if (intent?.action == ACTION_RESTART) {
            Log.i(TAG, "restart requested — respawning agent with current settings")
            // The settings page may have renamed the node; keep the UI label in sync.
            nodeId = LocalConfig.resolveNodeId(this)
            serviceScope.launch {
                try {
                    agentLifecycle.restart()
                } catch (e: Exception) {
                    Log.e(TAG, "agent restart failed: ${e.message}")
                }
            }
        }

        if (readinessJob?.isActive != true) {
            readinessJob = serviceScope.launch {
                nodeReadinessService.runLoop()
            }
        }

        return START_STICKY
    }

    override fun onDestroy() {
        super.onDestroy()
        Log.i(TAG, "service destroyed")
        statusClient.stop()
        LocalConfig.deleteRuntimeKubeconfig(this)
        // The agent is a child process: without this, `am stopservice` (or the
        // system recreating the service) leaves it running and the next start
        // fights over the kubelet port. Process kill (force-stop) skips this,
        // which is why cleanup_orphan_proot() still exists.
        //
        // 只有 1.5 s 宽限：这里是系统在主线程拆卸服务，等 12 s 会 ANR。正常的
        // "停止 agent" 走 ACTION_STOP，已经在协程里给了完整的 12 s。
        try {
            runBlocking { agentLifecycle.stop(graceMillis = 1_500L) }
        } catch (e: Exception) {
            Log.w(TAG, "could not stop agent on destroy: ${e.message}")
        }
        serviceScope.cancel()
        networkBroker.unregister()
        wifiLockBroker.release()
        wakeLockBroker.release()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    /**
     * The partial wake lock is only needed while containers actually run.
     * If the IPC socket never connects the lock stays acquired (previous
     * behaviour), so a broken socket cannot silently make the node sleepy.
     */
    private fun handleAgentStatus(status: AgentStatusSnapshot) {
        // Maintenance mode survives an agent/service restart. The prefs flag
        // is the user's intent; once the agent answers, re-apply cordon+drain.
        if (LocalConfig.drained(this) && !status.maintenance && !maintenanceReapplied) {
            maintenanceReapplied = true
            serviceScope.launch(Dispatchers.IO) {
                val resp = AgentApi.post("/maintenance?on=true", 8000)
                if (resp == null || resp.code !in 200..299) {
                    delay(5000)
                    maintenanceReapplied = false
                }
            }
        }
        if (status.maintenance) maintenanceReapplied = true

        if (status.podsRunning > 0) {
            idleSince = 0L
            wakeLockBroker.acquire()
            return
        }
        // Nothing is running. Keep the CPU awake anyway unless the user opted
        // into power saving: a released wake lock lets the device suspend, the
        // agent stops heartbeating, and the control plane marks the node
        // NotReady after ~40 s - which also evicts its pods.
        if (LocalConfig.keepOnline(this)) {
            idleSince = 0L
            wakeLockBroker.acquire()
            return
        }
        val now = System.currentTimeMillis()
        if (idleSince == 0L) idleSince = now
        if (now - idleSince >= IDLE_WAKE_LOCK_RELEASE_MS) {
            wakeLockBroker.release()
        }
    }

    // ─── Notification ────────────────────────────────────────────────────────

    private fun createNotificationChannel() {
        val channel = NotificationChannel(
            CHANNEL_ID,
            "DroidNode Agent",
            NotificationManager.IMPORTANCE_LOW
        ).apply {
            description = "DroidNode compute agent is running"
            setShowBadge(false)
        }
        getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
    }

    private fun buildNotification(): Notification {
        // The v2 console is the only UI now; tapping the notification opens it.

        val tapIntent = Intent(this, NodeActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP
        }
        val tapPending = PendingIntent.getActivity(
            this, 0, tapIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        return Notification.Builder(this, CHANNEL_ID)
            .setContentTitle("DroidNode")
            .setContentText("Compute node active — tap to open console")
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setContentIntent(tapPending)
            .setOngoing(true)
            .build()
    }
}
