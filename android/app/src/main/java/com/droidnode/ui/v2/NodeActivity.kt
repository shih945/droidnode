package com.droidnode.ui.v2

import android.Manifest
import android.app.Activity
import com.droidnode.ui.BaseActivity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.os.BatteryManager
import android.os.Build
import android.os.Bundle
import androidx.core.content.ContextCompat
import android.view.LayoutInflater
import android.view.View
import android.widget.LinearLayout
import android.widget.TextView
import com.droidnode.R
import com.droidnode.brokers.AgentStatus
import com.droidnode.exposers.ForegroundServiceExposer
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.withContext
import java.net.Inet4Address
import java.net.NetworkInterface
import java.util.Locale
import kotlin.math.roundToInt

/**
 * 节点页：这台手机现在能不能接 Pod、还剩多少资源。
 *
 * 数据来自 agent 的三个只读接口（/pods、/stats/summary、/healthz）与 Android 系统接口。
 * 维护 / 重启 / 停止等会改变节点状态的操作都在设置页；这里只显示「维护中」提示。
 */
class NodeActivity : BaseActivity() {

    override val isTabRoot: Boolean get() = true

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private lateinit var sheet: SheetHost

    private lateinit var pillDot: View
    private lateinit var pillText: TextView
    private lateinit var heroDot: View
    private lateinit var heroState: TextView
    private lateinit var heroSub: TextView
    private lateinit var heroPulse: PulseView
    private lateinit var tvNodeName: TextView
    private lateinit var tvServer: TextView
    private lateinit var tvKubelet: TextView
    private lateinit var tvIp: TextView

    private lateinit var maintenanceNotice: View
    private lateinit var maintenanceNoticeSub: TextView
    private lateinit var batteryNotice: View

    private lateinit var memValue: TextView
    private lateinit var memFill: View
    private lateinit var memNote: TextView
    private lateinit var diskValue: TextView
    private lateinit var diskFill: View
    private lateinit var diskNote: TextView
    private lateinit var appValue: TextView
    private lateinit var cpuValue: TextView
    private lateinit var batteryValue: TextView

    private lateinit var podList: LinearLayout
    private lateinit var podEmpty: TextView
    private lateinit var linkPods: TextView

    private var pods: List<PodRow> = emptyList()
    private var stats: NodeStats? = null
    private var proc: ProcInfo.Sample? = null
    private var agentOk = true

    private var lastJiffies = -1L
    private var lastCpuAt = 0L
    private var cpuText = "—"
    private var pollJob: Job? = null
    private var observeJob: Job? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_v2_node)
        sheet = SheetHost(this)

        pillDot = findViewById(R.id.pillDot)
        pillText = findViewById(R.id.pillText)
        heroDot = findViewById(R.id.heroDot)
        heroState = findViewById(R.id.heroState)
        heroSub = findViewById(R.id.heroSub)
        heroPulse = findViewById(R.id.heroPulse)
        tvNodeName = findViewById(R.id.tvNodeName)
        tvServer = findViewById(R.id.tvServer)
        tvKubelet = findViewById(R.id.tvKubelet)
        tvIp = findViewById(R.id.tvIp)
        maintenanceNotice = findViewById(R.id.maintenanceNotice)
        maintenanceNoticeSub = findViewById(R.id.maintenanceNoticeSub)
        batteryNotice = findViewById(R.id.batteryNotice)
        memValue = findViewById(R.id.memValue)
        memFill = findViewById(R.id.memFill)
        memNote = findViewById(R.id.memNote)
        diskValue = findViewById(R.id.diskValue)
        diskFill = findViewById(R.id.diskFill)
        diskNote = findViewById(R.id.diskNote)
        appValue = findViewById(R.id.appValue)
        cpuValue = findViewById(R.id.cpuValue)
        batteryValue = findViewById(R.id.batteryValue)
        podList = findViewById(R.id.podList)
        podEmpty = findViewById(R.id.podEmpty)
        linkPods = findViewById(R.id.linkPods)

        heroPulse.variant = PulseView.Variant.HERO
        for (id in intArrayOf(R.id.tickRes, R.id.tickPods)) {
            findViewById<PulseView>(id).apply {
                variant = PulseView.Variant.MINI
                setToneRes(R.color.text_3)
            }
        }

        BottomNav.wire(this, BottomNav.Screen.NODE)

        findViewById<View>(R.id.btnCopy).setOnClickListener { copyNodeName() }
        linkPods.setOnClickListener { BottomNav.open(this, BottomNav.Screen.PODS) }
        maintenanceNotice.setOnClickListener {
            BottomNav.open(this, BottomNav.Screen.SETTINGS)
        }

        ensureNotificationPermission()
        ensureAgentStarted()
        render()
    }

    /**
     * TargetSdk 33+ 起通知需要运行时授权，否则前台服务通知不显示
     * （服务本身还能跑，但用户看不到节点在运行）。当前 targetSdk 仍是 28，
     * 系统会自动授予；这段只在将来升到 33+ 后真正弹框。
     */
    private fun ensureNotificationPermission() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
        if (checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED
        ) {
            return
        }
        requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), REQ_NOTIFICATIONS)
    }

    /**
     * 新版界面成了默认入口后，这里必须自己拉起前台服务：
     * 以前只有旧 MainActivity 会 startForegroundService，切了入口就没人启动了。
     */
    private fun ensureAgentStarted() {
        if (ForegroundServiceExposer.isAgentRunning) return
        try {
            val intent = Intent(this, ForegroundServiceExposer::class.java)
            ContextCompat.startForegroundService(this, intent)
        } catch (e: Exception) {
            android.util.Log.w("NodeActivity", Tr.s("启动前台服务失败: %1\$s", e.message))
        }
    }

    override fun onStart() {
        super.onStart()
        Repository.acquire(scope)
        observeJob = scope.launch {
            Repository.state.collectLatest { onSnapshot(it) }
        }
    }

    override fun onStop() {
        observeJob?.cancel()
        observeJob = null
        pollJob?.cancel()
        pollJob = null
        Repository.release()
        super.onStop()
    }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
    }

    override fun onBackPressed() {
        if (sheet.isShowing) sheet.dismiss() else super.onBackPressed()
    }

    /** Render one repository snapshot; the shared loop owns the HTTP calls. */
    private suspend fun onSnapshot(snapshot: NodeRepository.State) {
        agentOk = snapshot.health
        if (snapshot.ticks > 0L) {
            pods = snapshot.pods
            snapshot.stats?.let { stats = it }
        }
        proc = withContext(Dispatchers.IO) { ProcInfo.ourProcesses() }
        render()
    }

    private fun render() {
        // Prefer the port the agent reports; the saved value can lag behind a restart.
        val kubeletPort = if (AgentStatus.connected && AgentStatus.kubeletPort > 0) {
            AgentStatus.kubeletPort
        } else {
            LocalConfig.kubeletPort(this)
        }
        tvKubelet.text = "0.0.0.0:$kubeletPort"
        val running = ForegroundServiceExposer.isAgentRunning
        // Maintenance (operator) and battery drain are different states: only
        // maintenance shows the "维护中" notice and is persisted as user intent.
        val maintenance = if (AgentStatus.connected) {
            AgentStatus.maintenance
        } else {
            LocalConfig.drained(this)
        }
        val batteryDrained = AgentStatus.connected && AgentStatus.drained && !maintenance
        val drained = maintenance || batteryDrained ||
            pods.any { it.message?.contains("drained") == true }

        val status = StatusPill.compute(agentOk, drained)
        val stateText = when (status.label) {
            Tr.s("运行中") -> Tr.s("节点运行中")
            Tr.s("已排空") -> Tr.s("节点已排空")
            Tr.s("已停止") -> Tr.s("节点已停止")
            else -> Tr.s("agent 无响应")
        }

        Ui.tintDot(heroDot, getColor(status.toneRes))
        heroState.text = stateText
        heroPulse.setTone(getColor(status.toneRes))
        heroPulse.setStopped(!running || !agentOk)
        StatusPill.render(this, pillDot, pillText, agentOk, drained)

        val startTime = ForegroundServiceExposer.startTime
        val ready = pods.count { it.phase.startsWith("Running") }
        heroSub.text = when {
            !running -> Tr.s("前台服务未在运行")
            startTime <= 0L -> Tr.s("已启动 · 0 / %1\$s 个 Pod 就绪", pods.size)
            maintenance -> Tr.s("已运行 %1\$s · 维护中，不再接收新的 Pod", Ui.uptime(startTime))
            batteryDrained -> Tr.s("已运行 %1\$s · 电池低，已排空", Ui.uptime(startTime))
            else -> Tr.s("已运行 %1\$s · %2\$s / %3\$s 个 Pod 就绪", Ui.uptime(startTime), ready, pods.size)
        }

        tvNodeName.text = ForegroundServiceExposer.nodeId.ifEmpty { "—" }
        tvServer.text = LocalConfig.serverFrom(LocalConfig.readKubeconfig(this)) ?: Tr.s("未配置 kubeconfig")
        tvIp.text = localIp()

        renderResources()
        renderMaintenanceNotice(maintenance)
        renderBatteryNotice()
        renderPods()
    }

    private fun renderResources() {
        stats?.let { s ->
            val memUsed = if (s.memTotal > 0) (s.memTotal - s.memAvailable) * 100.0 / s.memTotal else 0.0
            memValue.text = Tr.s("可用 %1\$s / %2\$s", Ui.humanBytes(s.memAvailable), Ui.humanBytes(s.memTotal))
            memNote.text = Tr.s("已用 %1\$s%%", memUsed.roundToInt())
            Ui.setMeter(
                memFill,
                memUsed.toFloat(),
                getColor(if (memUsed > 85) R.color.bad else if (memUsed > 70) R.color.lamp else R.color.ok),
            )

            val diskUsed = if (s.diskTotal > 0) (s.diskTotal - s.diskAvailable) * 100.0 / s.diskTotal else 0.0
            diskValue.text = Tr.s("可用 %1\$s / %2\$s", Ui.humanBytes(s.diskAvailable), Ui.humanBytes(s.diskTotal))
            diskNote.text = Tr.s("已用 %1\$s%%", diskUsed.roundToInt())
            Ui.setMeter(
                diskFill,
                diskUsed.toFloat(),
                getColor(if (diskUsed > 85) R.color.bad else R.color.ok),
            )
        }

        proc?.let { p ->
            appValue.text = Tr.s("%1\$s 个进程 · %2\$s", p.processes, Ui.humanBytes(p.rssBytes))
            val now = System.currentTimeMillis()
            if (lastJiffies >= 0 && now > lastCpuAt) {
                val delta = (p.jiffies - lastJiffies).coerceAtLeast(0L)
                val pct = (delta / 100.0) / ((now - lastCpuAt) / 1000.0) * 100.0
                cpuText = getString(
                    R.string.kt_cpu,
                    Runtime.getRuntime().availableProcessors(),
                    pct,
                )
            }
            lastJiffies = p.jiffies
            lastCpuAt = now
            cpuValue.text = cpuText
        }

        val battery = registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        if (battery != null) {
            val level = battery.getIntExtra(BatteryManager.EXTRA_LEVEL, -1)
            val scale = battery.getIntExtra(BatteryManager.EXTRA_SCALE, 100)
            val status = battery.getIntExtra(BatteryManager.EXTRA_STATUS, -1)
            val temp = battery.getIntExtra(BatteryManager.EXTRA_TEMPERATURE, 0)
            val pct = if (level >= 0 && scale > 0) level * 100 / scale else -1
            val charging = status == BatteryManager.BATTERY_STATUS_CHARGING ||
                status == BatteryManager.BATTERY_STATUS_FULL
            batteryValue.text = String.format(
                Locale.US,
                "%d%% · %s · %.1f°C",
                pct,
                if (charging) Tr.s("充电中") else Tr.s("放电中"),
                temp / 10.0,
            )
        }
    }

    /** 维护状态在节点页只读展示；开关/重启/停止都在设置 → 运行控制。 */
    private fun renderMaintenanceNotice(maintenance: Boolean) {
        maintenanceNotice.visibility = if (maintenance) View.VISIBLE else View.GONE
        maintenanceNoticeSub.text = when {
            AgentStatus.cordoned -> Tr.s("调度已停止，容器已排空")
            maintenance -> Tr.s("容器已排空（cordon 状态未确认）")
            else -> ""
        }
    }

    /**
     * Battery-optimization exemption is the one thing the user must grant for the
     * node to survive a locked screen: without it the system ignores our wake
     * lock and freezes the agent in Doze, so the heartbeat stops.
     */
    private fun renderBatteryNotice() {
        batteryNotice.visibility = if (LocalConfig.batteryExempt(this)) View.GONE else View.VISIBLE
    }

    private fun renderPods() {
        val sorted = pods.sortedWith(
            compareBy<PodRow> { severity(it) }.thenBy { it.name },
        )
        podList.removeAllViews()
        if (sorted.isEmpty()) {
            podEmpty.visibility = View.VISIBLE
            linkPods.text = Tr.s("全部")
            return
        }
        podEmpty.visibility = View.GONE
        linkPods.text = Tr.s("全部 %1\$s", sorted.size)
        for (pod in sorted.take(3)) {
            podList.addView(
                Rows.pod(this, podList, pod) {
                    Sheets.pod(this, sheet, pod) { openLogsFor(pod.name) }
                },
            )
        }
    }

    private fun severity(pod: PodRow): Int = when {
        pod.phase.startsWith("Running") || pod.phase.startsWith("Succeeded") -> 2
        pod.phase == "Pending" -> 1
        else -> 0
    }

    private fun openLogsFor(podName: String) {
        startActivity(
            Intent(this, LogsActivity::class.java).putExtra(LogsActivity.EXTRA_POD, podName),
        )
    }

    private fun copyNodeName() {
        val name = ForegroundServiceExposer.nodeId
        if (name.isEmpty()) {
            Sheets.toast(this, Tr.s("节点名还没生成"))
            return
        }
        val cm = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        cm.setPrimaryClip(ClipData.newPlainText("node", name))
        Sheets.toast(this, Tr.s("已复制节点名"))
    }

    private fun localIp(): String {
        return try {
            for (nif in NetworkInterface.getNetworkInterfaces()) {
                if (!nif.isUp || nif.isLoopback) continue
                for (addr in nif.inetAddresses) {
                    if (addr is Inet4Address && !addr.isLoopbackAddress) {
                        return addr.hostAddress ?: continue
                    }
                }
            }
            "—"
        } catch (e: Exception) {
            "—"
        }
    }

    private companion object {
        const val REQ_NOTIFICATIONS = 2001
    }
}
