package com.droidnode.ui.v2

import com.droidnode.data.AgentApi
import com.droidnode.data.Validation

import android.app.Activity
import com.droidnode.ui.BaseActivity
import android.content.Context
import android.content.Intent
import android.content.res.ColorStateList
import android.net.Uri
import android.os.Bundle
import android.os.PowerManager
import android.provider.Settings
import android.view.View
import android.widget.EditText
import android.widget.RadioButton
import android.widget.RadioGroup
import android.widget.Switch
import android.widget.TextView
import androidx.core.content.ContextCompat
import com.droidnode.LogBuffer
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
import kotlinx.coroutines.withContext

/**
 * 设置页：导入 kubeconfig、连接测试、节点配置（节点名 / kubelet 端口 / 日志级别）、
 * 电池白名单、维护模式。
 *
 * 节点配置写入 SharedPreferences（LocalConfig），由 ForegroundServiceExposer 在
 * 启动 agent 时读成环境变量；保存后走 ACTION_RESTART 让新配置生效。
 */
class SettingsActivity : BaseActivity() {

    override val isTabRoot: Boolean get() = true

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private lateinit var sheet: SheetHost

    private lateinit var pillDot: View
    private lateinit var pillText: TextView
    private lateinit var kubeStatus: TextView
    private lateinit var kubeServer: TextView
    private lateinit var kubePath: TextView
    private lateinit var kubeAuth: TextView
    private lateinit var testHint: TextView
    private lateinit var editNodeName: EditText
    private lateinit var editPort: EditText
    private lateinit var editLogLevel: EditText
    private lateinit var editMirrorDocker: EditText
    private lateinit var editMirrorK8s: EditText
    private lateinit var editMirrorQuay: EditText
    private lateinit var configHint: TextView
    private lateinit var cfgRegistry: TextView
    private lateinit var batterySub: TextView
    private lateinit var maintainSub: TextView
    private lateinit var agentReport: TextView
    private lateinit var agentPowerSub: TextView
    private lateinit var btnAgentPower: TextView
    private lateinit var switchMaintain: Switch
    private lateinit var runState: TextView
    private lateinit var btnRestartAgent: TextView
    private lateinit var aboutVersion: TextView
    private lateinit var appearanceGroup: RadioGroup
    private lateinit var langGroup: RadioGroup
    private lateinit var switchAutostart: Switch
    private lateinit var ifaceGroup: RadioGroup

    /** 程序化重建单选行时，避免触发 onCheckedChange 又去重启 agent。 */
    private var suppressIface = false

    private var suppressSwitch = false
    private var suppressAppearance = false
    private var agentOk = true
    private var statusJob: Job? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_v2_settings)
        sheet = SheetHost(this)

        pillDot = findViewById(R.id.pillDot)
        pillText = findViewById(R.id.pillText)
        kubeStatus = findViewById(R.id.kubeStatus)
        kubeServer = findViewById(R.id.kubeServer)
        kubePath = findViewById(R.id.kubePath)
        kubeAuth = findViewById(R.id.kubeAuth)
        testHint = findViewById(R.id.testHint)
        editNodeName = findViewById(R.id.editNodeName)
        editPort = findViewById(R.id.editPort)
        editLogLevel = findViewById(R.id.editLogLevel)
        editMirrorDocker = findViewById(R.id.editMirrorDocker)
        editMirrorK8s = findViewById(R.id.editMirrorK8s)
        editMirrorQuay = findViewById(R.id.editMirrorQuay)
        configHint = findViewById(R.id.configHint)
        cfgRegistry = findViewById(R.id.cfgRegistry)
        batterySub = findViewById(R.id.batterySub)
        maintainSub = findViewById(R.id.maintainSub)
        agentReport = findViewById(R.id.agentReport)
        agentPowerSub = findViewById(R.id.agentPowerSub)
        btnAgentPower = findViewById(R.id.btnAgentPower)
        switchMaintain = findViewById(R.id.switchMaintain)
        runState = findViewById(R.id.runState)
        btnRestartAgent = findViewById(R.id.btnRestartAgent)
        aboutVersion = findViewById(R.id.aboutVersion)
        appearanceGroup = findViewById(R.id.appearanceGroup)
        langGroup = findViewById(R.id.langGroup)
        switchAutostart = findViewById(R.id.switchAutostart)
        ifaceGroup = findViewById(R.id.ifaceGroup)
        renderInterfaces()

        switchAutostart.isChecked = LocalConfig.autostart(this)
        switchAutostart.setOnCheckedChangeListener { _, checked ->
            LocalConfig.setAutostart(this, checked)
            Sheets.toast(this, if (checked) Tr.s("已开启开机自启") else Tr.s("已关闭开机自启"))
        }
        findViewById<View>(R.id.btnExport).setOnClickListener { exportDiagnostics() }

        appearanceGroup.setOnCheckedChangeListener { _, checkedId ->
            if (suppressAppearance) return@setOnCheckedChangeListener
            val mode = when (checkedId) {
                R.id.appearanceSystem -> LocalConfig.APPEARANCE_SYSTEM
                R.id.appearanceDark -> LocalConfig.APPEARANCE_DARK
                else -> LocalConfig.APPEARANCE_LIGHT
            }
            if (mode == LocalConfig.appearance(this)) return@setOnCheckedChangeListener
            LocalConfig.setAppearance(this, mode)
            recreate()
        }

        langGroup.setOnCheckedChangeListener { _, checkedId ->
            if (suppressAppearance) return@setOnCheckedChangeListener
            val tag = when (checkedId) {
                R.id.langZh -> LocalConfig.LANGUAGE_ZH
                R.id.langEn -> LocalConfig.LANGUAGE_EN
                else -> LocalConfig.LANGUAGE_SYSTEM
            }
            if (tag == LocalConfig.language(this)) return@setOnCheckedChangeListener
            LocalConfig.setLanguage(this, tag)
            recreate()
        }

        BottomNav.wire(this, BottomNav.Screen.SETTINGS)
        for (id in intArrayOf(R.id.tickNode, R.id.tickRun, R.id.tickPower, R.id.tickLook, R.id.tickNet, R.id.tickLang, R.id.tickDiag, R.id.tickAbout)) {
            findViewById<PulseView>(id).apply {
                variant = PulseView.Variant.MINI
                setToneRes(R.color.text_3)
            }
        }

        findViewById<View>(R.id.btnJoin).setOnClickListener {
            startActivity(Intent(this, JoinActivity::class.java))
        }
        findViewById<View>(R.id.btnTest).setOnClickListener { testConnection() }
        findViewById<View>(R.id.btnSaveConfig).setOnClickListener { saveNodeConfig() }
        findViewById<View>(R.id.btnAgentPower).setOnClickListener { toggleAgent() }
        findViewById<View>(R.id.btnRestartAgent).setOnClickListener { askRestart() }
        findViewById<View>(R.id.btnBattery).setOnClickListener { requestBatteryExemption() }
        switchMaintain.setOnCheckedChangeListener { _, checked ->
            if (!suppressSwitch) requestMaintenance(checked)
        }

        val version = try {
            packageManager.getPackageInfo(packageName, 0).versionName
        } catch (e: Exception) {
            "0.1.0"
        }
        aboutVersion.text = "DroidNode $version"

        ensureAgentStarted()
        render()
    }

    /**
     * Settings can be opened directly (recents restore, `am start` in tests) —
     * unlike the launcher entry it must not leave the agent stopped.
     */
    private fun ensureAgentStarted() {
        if (ForegroundServiceExposer.isAgentRunning) return
        try {
            ContextCompat.startForegroundService(
                this,
                Intent(this, ForegroundServiceExposer::class.java),
            )
        } catch (e: Exception) {
            android.util.Log.w("SettingsActivity", Tr.s("启动前台服务失败: %1\$s", e.message))
        }
    }

    /**
     * E17: explicit start/stop. Stopping goes through the service so onDestroy
     * also kills the agent child (no orphan proot holding the kubelet port).
     */
    private fun toggleAgent() {
        if (ForegroundServiceExposer.isAgentRunning) {
            ForegroundServiceExposer.requestStop(this)
            Sheets.toast(this, Tr.s("正在停止 agent…"))
        } else {
            try {
                ContextCompat.startForegroundService(
                    this,
                    Intent(this, ForegroundServiceExposer::class.java),
                )
                Sheets.toast(this, Tr.s("正在启动 agent…"))
            } catch (e: Exception) {
                Sheets.toast(this, Tr.s("启动失败：%1\$s", e.message))
                return
            }
        }
        scope.launch {
            delay(1500L)
            render()
        }
    }

    override fun onResume() {
        super.onResume()
        render()
        // Wi-Fi 开关会改变网卡列表，回到设置页时重新枚举一次
        renderInterfaces()
        // E16: the agent pushes status every 10 s; poll the in-memory snapshot
        // so the "agent 上报" row stays live instead of freezing at onResume.
        statusJob?.cancel()
        statusJob = scope.launch {
            while (isActive) {
                agentReport.text = agentReportText()
                renderRunControl()
                delay(3000L)
            }
        }
        scope.launch {
            val ok = withContext(Dispatchers.IO) { Snapshot.health() }
            agentOk = ok
            render()
        }
    }

    override fun onPause() {
        statusJob?.cancel()
        statusJob = null
        super.onPause()
    }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
    }

    override fun onBackPressed() {
        if (sheet.isShowing) sheet.dismiss() else super.onBackPressed()
    }

    private fun render() {
        val maintenance = if (AgentStatus.connected) {
            AgentStatus.maintenance
        } else {
            LocalConfig.drained(this)
        }
        val batteryDrained = AgentStatus.connected && AgentStatus.drained && !maintenance
        StatusPill.render(this, pillDot, pillText, agentOk, maintenance || batteryDrained)

        val kube = LocalConfig.readKubeconfig(this)
        val server = LocalConfig.serverFrom(kube)
        kubeStatus.text = if (server != null) Tr.s("已加入") else Tr.s("未配置")
        kubeServer.text = server ?: "—"
        kubePath.text = LocalConfig.kubeconfigFile(this).absolutePath
        kubeAuth.text = LocalConfig.authLabel(kube)

        editNodeName.hint = LocalConfig.autoNodeId(this)
        if (!editNodeName.hasFocus()) editNodeName.setText(LocalConfig.nodeName(this))
        if (!editPort.hasFocus()) editPort.setText(LocalConfig.kubeletPort(this).toString())
        if (!editLogLevel.hasFocus()) editLogLevel.setText(LocalConfig.logLevel(this))
        val mirrors = LocalConfig.readRegistryMirrors(this)
        if (!editMirrorDocker.hasFocus()) {
            editMirrorDocker.setText(mirrors[LocalConfig.MIRROR_DOCKER].orEmpty())
        }
        if (!editMirrorK8s.hasFocus()) {
            editMirrorK8s.setText(mirrors[LocalConfig.MIRROR_K8S].orEmpty())
        }
        if (!editMirrorQuay.hasFocus()) {
            editMirrorQuay.setText(mirrors[LocalConfig.MIRROR_QUAY].orEmpty())
        }
        cfgRegistry.text = registryStatusText(mirrors)
        suppressAppearance = true
        appearanceGroup.check(
            when (LocalConfig.appearance(this)) {
                LocalConfig.APPEARANCE_SYSTEM -> R.id.appearanceSystem
                LocalConfig.APPEARANCE_DARK -> R.id.appearanceDark
                else -> R.id.appearanceLight
            },
        )
        langGroup.check(
            when (LocalConfig.language(this)) {
                LocalConfig.LANGUAGE_ZH -> R.id.langZh
                LocalConfig.LANGUAGE_EN -> R.id.langEn
                else -> R.id.langSystem
            },
        )
        suppressAppearance = false
        agentReport.text = agentReportText()
        val running = ForegroundServiceExposer.isAgentRunning
        agentPowerSub.text = if (running) {
            Tr.s("运行中，前台服务与 WakeLock 生效")
        } else {
            Tr.s("已停止，节点会从集群掉线")
        }
        btnAgentPower.text = if (running) Tr.s("停止 agent") else Tr.s("启动 agent")

        val ignoring = ignoreBattery()
        batterySub.text = if (ignoring) {
            Tr.s("已加入，息屏后不会被系统清理")
        } else {
            Tr.s("未加入，息屏后可能被系统清理")
        }

        renderRunControl()
    }

    /**
     * 运行控制区的实时刷新：状态、维护开关、重启/停止按钮。
     * 状态来自 IPC（每 10 秒）或本地 prefs（IPC 未连接时）。
     */
    private fun renderRunControl() {
        val running = ForegroundServiceExposer.isAgentRunning
        val maintenance = if (AgentStatus.connected) {
            AgentStatus.maintenance
        } else {
            LocalConfig.drained(this)
        }
        val batteryDrained = AgentStatus.connected && AgentStatus.drained && !maintenance
        runState.text = when {
            !running -> Tr.s("已停止 · 节点已从集群掉线")
            maintenance -> Tr.s("维护中（已 cordon + 排空）")
            batteryDrained -> Tr.s("低电排空中 · 电量恢复后自动恢复")
            else -> Tr.s("运行中")
        }
        runState.setTextColor(
            getColor(if (running && !maintenance && !batteryDrained) R.color.ok else R.color.lamp),
        )

        suppressSwitch = true
        switchMaintain.isChecked = maintenance
        switchMaintain.isEnabled = running
        suppressSwitch = false
        maintainSub.text = when {
            !running -> Tr.s("agent 已停止；启动后才能开启维护模式")
            maintenance -> Tr.s("已 cordon 并排空：调度器不再投递新 Pod，本机容器已停止")
            else -> Tr.s("开启后先 cordon 节点，再停止本机容器；关闭后恢复调度")
        }
        btnRestartAgent.isEnabled = running
        btnRestartAgent.alpha = if (running) 1f else 0.4f
        agentPowerSub.text = if (running) {
            Tr.s("运行中，前台服务与 WakeLock 生效")
        } else {
            Tr.s("已停止，节点会从集群掉线")
        }
        btnAgentPower.text = if (running) Tr.s("停止 agent") else Tr.s("启动 agent")
    }

    /**
     * E16: structured status pushed by the agent over the Unix socket — no
     * regex parsing of stdout involved.
     */
    private fun agentReportText(): String {
        val at = AgentStatus.updatedAt
        if (at <= 0L) {
            return if (AgentStatus.connected) Tr.s("已连接 · 等待状态") else Tr.s("未连接（agent 未启动或 IPC 不可用）")
        }
        val age = ((System.currentTimeMillis() - at) / 1000L).coerceAtLeast(0L)
        val state = when {
            AgentStatus.drained -> Tr.s("已排空")
            AgentStatus.podsRunning > 0 ->
                Tr.s("%1\$s 个 Pod 运行中", AgentStatus.podsRunning) +
                    if (AgentStatus.restarts > 0) Tr.s(" · 重启 %1\$s 次", AgentStatus.restarts) else ""
            else -> Tr.s("空闲，无运行中容器")
        }
        val ready = if (AgentStatus.ready) "Ready" else "NotReady"
        val link = if (AgentStatus.connected) Tr.s("已连接") else Tr.s("已断开")
        return Tr.s("%1\$s · %2\$s · %3\$s · %4\$s 秒前", ready, state, link, age)
    }

    /**
     * 优先展示 agent 自报的生效值；没有日志（agent 还没起来 / 刚改完没重启）时，
     * 退回到已保存的配置，并明确说还没生效。
     */
    private fun registryStatusText(mirrors: Map<String, String>): String {
        registryFromLogs()?.let { return it }
        val configured = LocalConfig.MIRROR_KEYS.mapNotNull { mirrors[it] }
        return if (configured.isEmpty()) {
            Tr.s("未配置 · 直连上游")
        } else {
            Tr.s("已保存，agent 重启后生效：%1\$s", configured.first())
        }
    }

    /**
     * agent 启动时会打一行 `registry mirrors configured docker=… k8s=… quay=…`
     * （或 `registry mirrors: none`）；拉取时的 `registry mirror applied` 只在实际
     * 发生改写时才有。两者都从日志里取，比界面自己猜更可靠。
     */
    private fun registryFromLogs(): String? {
        val configured = Regex("registry mirrors configured docker=(\\S*) k8s=(\\S*) quay=(\\S*)")
        val applied = Regex("registry mirror applied.*?to=(\\S+)")
        for (line in LogBuffer.snapshot().asReversed()) {
            configured.find(line.text)?.let { m ->
                val parts = listOf(
                    "docker.io" to m.groupValues[1],
                    "registry.k8s.io" to m.groupValues[2],
                    "quay.io" to m.groupValues[3],
                ).filter { it.second.isNotEmpty() }
                if (parts.isNotEmpty()) {
                    return parts.joinToString(" · ") { "${it.first} → ${it.second}" }
                }
            }
            if (line.text.contains("registry mirrors: none")) {
                return Tr.s("未配置 · 直连上游")
            }
            applied.find(line.text)?.let { return Tr.s("已改写 → ") + it.groupValues[1] }
        }
        return null
    }

    /**
     * 「网络」一节：自动 + 每张 IPv4 网卡一行。
     *
     * 选中后写 files/network-preference.env，agent 启动时按网卡名解析地址；
     * 多网卡（Wi-Fi + Wi-Fi Direct/USB 共享）时，自动探测可能选到集群到不了的那张。
     */
    private fun renderInterfaces() {
        val ifaces = LocalConfig.listIpv4Interfaces().sortedBy { it.first }

        suppressIface = true
        ifaceGroup.removeAllViews()
        addIfaceOption("", Tr.s("自动（推荐）· 跳过 Wi-Fi Direct / USB 共享"))
        ifaces.forEach { (name, ip) ->
            val tail = if (LocalConfig.isTetherLike(name)) Tr.s(" · 集群不可达") else ""
            addIfaceOption(name, "$name · $ip$tail")
        }
        suppressIface = false
    }

    private fun addIfaceOption(value: String, label: String) {
        val radio = RadioButton(this)
        radio.id = View.generateViewId()
        radio.tag = value
        radio.text = label
        radio.textSize = 14f
        radio.setTextColor(getColor(R.color.text))
        radio.buttonTintList = ColorStateList.valueOf(getColor(R.color.lamp))
        radio.minHeight = resources.getDimensionPixelSize(R.dimen.tap)
        radio.isChecked = value.equals(LocalConfig.nodeIface(this), ignoreCase = true)
        radio.setOnClickListener {
            if (!suppressIface) onIfacePicked(value)
        }
        ifaceGroup.addView(radio)
    }

    private fun onIfacePicked(value: String) {
        if (value == LocalConfig.nodeIface(this)) return
        if (!LocalConfig.setNodeIface(this, value)) {
            Sheets.toast(this, Tr.s("写入网络偏好失败"))
            return
        }
        val label = if (value.isEmpty()) Tr.s("自动选择") else value
        Sheets.toast(this, Tr.s("已选择 %1\$s", label))
        if (ForegroundServiceExposer.isAgentRunning) restartAgent()
    }

    private fun askRestart() {
        if (!ForegroundServiceExposer.isAgentRunning) {
            Sheets.toast(this, Tr.s("agent 已停止，请先启动"))
            return
        }
        Sheets.confirm(
            this,
            sheet,
            Tr.s("重启 agent"),
            Tr.s("会先停止 agent 进程，再按当前配置重新启动；运行中的容器会被重建。"),
            Tr.s("重启"),
        ) { restartAgent() }
    }

    private fun restartAgent() {
        ForegroundServiceExposer.requestRestart(this)
        Sheets.toast(this, Tr.s("正在重启 agent，稍后可在节点页看状态"))
        render()
    }

    private fun testConnection() {
        testHint.visibility = View.VISIBLE
        testHint.setTextColor(getColor(R.color.text_3))
        testHint.text = Tr.s("正在连接控制面（TLS + 身份校验）…")
        val kube = LocalConfig.readKubeconfig(this)
        scope.launch {
            val result = withContext(Dispatchers.IO) { KubeConnectionTest.test(kube) }
            showTestResult(result)
            agentOk = withContext(Dispatchers.IO) { Snapshot.health() }
            render()
        }
    }

    private fun showTestResult(result: KubeConnectionTest.Result) {
        val (ok, text) = ConnectionMessages.text(result)
        testHint.visibility = View.VISIBLE
        testHint.setTextColor(getColor(if (ok) R.color.ok else R.color.bad))
        testHint.text = if (agentOk) text else Tr.s("%1\$s（agent 当前无响应）", text)
    }

    private fun ignoreBattery(): Boolean = try {
        val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
        pm.isIgnoringBatteryOptimizations(packageName)
    } catch (e: Exception) {
        false
    }

    private fun requestBatteryExemption() {
        if (ignoreBattery()) {
            Sheets.toast(this, Tr.s("已经在电池优化白名单里"))
            return
        }
        try {
            startActivity(
                Intent(
                    Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                    Uri.parse("package:$packageName"),
                ),
            )
        } catch (e: Exception) {
            try {
                startActivity(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS))
            } catch (e2: Exception) {
                Sheets.toast(this, Tr.s("这台设备打不开电池优化设置"))
            }
        }
    }

    /**
     * E14：保存节点名 / kubelet 端口 / 日志级别，以及镜像源（写 files/registry-mirrors.env）。
     * 节点三项走环境变量、镜像源走配置文件，都在 agent 启动时读取，所以保存后必须重启 agent；
     * 重启会重建运行中的容器。
     */
    private fun saveNodeConfig() {
        val name = editNodeName.text.toString().trim()
        if (!Validation.validNodeName(name)) {
            showConfigHint(Tr.s("节点名只能是小写字母、数字、'-'、'.'，且以字母或数字开头结尾"), false)
            return
        }
        val port = Validation.parsePort(editPort.text.toString())
        if (port == null) {
            showConfigHint(Tr.s("端口必须是 1024–65535 的整数（非 root 不能监听 <1024）"), false)
            return
        }
        val level = editLogLevel.text.toString().trim().lowercase()
        if (!Validation.validLogLevel(level, LocalConfig.LOG_LEVELS)) {
            showConfigHint(Tr.s("日志级别只能是 error / warn / info / debug / trace"), false)
            return
        }
        val docker = LocalConfig.normalizeMirrorHost(editMirrorDocker.text.toString())
        val k8s = LocalConfig.normalizeMirrorHost(editMirrorK8s.text.toString())
        val quay = LocalConfig.normalizeMirrorHost(editMirrorQuay.text.toString())
        if (docker == null || k8s == null || quay == null) {
            showConfigHint(Tr.s("镜像源只填主机名（可带 :端口），不要带 http:// 或路径"), false)
            return
        }

        val storedName = LocalConfig.nodeName(this).trim()
        val oldName = LocalConfig.resolveNodeId(this)
        val oldPort = LocalConfig.kubeletPort(this)
        val oldLevel = LocalConfig.logLevel(this)
        val oldMirrors = LocalConfig.readRegistryMirrors(this)
        val nodesChanged = name != storedName || port != oldPort || level != oldLevel
        val mirrorsChanged = docker != oldMirrors[LocalConfig.MIRROR_DOCKER].orEmpty() ||
            k8s != oldMirrors[LocalConfig.MIRROR_K8S].orEmpty() ||
            quay != oldMirrors[LocalConfig.MIRROR_QUAY].orEmpty()
        if (!nodesChanged && !mirrorsChanged) {
            showConfigHint(Tr.s("配置没有变化"), true)
            return
        }

        // 先落盘镜像配置：写失败就不要重启，避免重启后还是旧值。
        if (mirrorsChanged && !LocalConfig.writeRegistryMirrors(this, docker, k8s, quay)) {
            showConfigHint(Tr.s("写入 registry-mirrors.env 失败，检查应用存储"), false)
            return
        }

        if (nodesChanged) {
            LocalConfig.setNodeName(this, name)
            LocalConfig.setKubeletPort(this, port)
            LocalConfig.setLogLevel(this, level)
            // UI 立即切到新端口，避免重启后的几秒里所有请求都打到旧端口。
            AgentApi.port = port
        }
        ForegroundServiceExposer.requestRestart(this)

        val renamed = LocalConfig.resolveNodeId(this) != oldName
        val what = when {
            nodesChanged && mirrorsChanged -> Tr.s("节点配置与镜像源")
            mirrorsChanged -> Tr.s("镜像源")
            else -> Tr.s("节点配置")
        }
        showConfigHint(
            Tr.s("已保存（%1\$s），agent 正在按新配置重启", what) +
                if (renamed) Tr.s("；新节点名会在集群里新建 Node，旧节点不会自动删除") else "",
            true,
        )
        render()
    }

    private fun showConfigHint(message: String, ok: Boolean) {
        configHint.visibility = View.VISIBLE
        configHint.setTextColor(getColor(if (ok) R.color.ok else R.color.bad))
        configHint.text = message
    }

    /**
     * 诊断导出：把版本、节点身份、外观和最近 200 行日志拼成文本，走系统分享。
     * 不含 kubeconfig / token / CA，避免Tr.s("诊断包")变成凭据泄露。
     */
    private fun exportDiagnostics() {
        val kube = LocalConfig.readKubeconfig(this)
        val clock = java.text.SimpleDateFormat("HH:mm:ss", java.util.Locale.US)
        val stamp = java.text.SimpleDateFormat("yyyy-MM-dd HH:mm:ss", java.util.Locale.US)
        val report = buildString {
            appendLine(Tr.s("DroidNode 诊断"))
            appendLine("time: " + stamp.format(java.util.Date()))
            appendLine("version: " + aboutVersion.text)
            appendLine("node: " + LocalConfig.resolveNodeId(this@SettingsActivity))
            appendLine("server: " + (LocalConfig.serverFrom(kube) ?: "-"))
            appendLine("identity: " + LocalConfig.authLabel(kube))
            appendLine("appearance: " + LocalConfig.appearanceLabel(LocalConfig.appearance(this@SettingsActivity)))
            appendLine("kubelet port: " + LocalConfig.kubeletPort(this@SettingsActivity))
            appendLine("--- logs (last 200) ---")
            LogBuffer.snapshot().takeLast(200).forEach {
                appendLine(clock.format(java.util.Date(it.timeMs)) + " " + it.text)
            }
        }
        val send = Intent(Intent.ACTION_SEND).apply {
            type = "text/plain"
            putExtra(Intent.EXTRA_SUBJECT, Tr.s("DroidNode 诊断"))
            putExtra(Intent.EXTRA_TEXT, report)
        }
        try {
            startActivity(Intent.createChooser(send, Tr.s("导出诊断")))
        } catch (e: Exception) {
            Sheets.toast(this, Tr.s("没有可用的分享目标"))
        }
    }

    private fun requestMaintenance(on: Boolean) {
        switchMaintain.isEnabled = false
        scope.launch {
            val response = withContext(Dispatchers.IO) {
                AgentApi.post("/maintenance?on=$on", 8000)
            }
            val ok = response != null && response.code in 200..299
            switchMaintain.isEnabled = true
            if (ok) {
                LocalConfig.setDrained(this@SettingsActivity, on)
                // Optimistic update so the switch/status react immediately;
                // the next IPC sample confirms it.
                AgentStatus.maintenance = on
                AgentStatus.cordoned = on
                Sheets.toast(
                    this@SettingsActivity,
                    if (on) Tr.s("已进入维护模式 · 节点已 cordon 并排空") else Tr.s("已退出维护模式 · 恢复调度"),
                )
            } else {
                Sheets.toast(this@SettingsActivity, Tr.s("agent 无响应，维护模式切换失败"))
                suppressSwitch = true
                switchMaintain.isChecked = !on
                suppressSwitch = false
            }
            render()
        }
    }

    private companion object {
        
    }
}
