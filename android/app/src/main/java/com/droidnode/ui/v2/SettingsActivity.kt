package com.droidnode.ui.v2

import com.droidnode.data.KubeConnectionTest

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
    private val controller by lazy { SettingsController(this) }
    internal lateinit var sheet: SheetHost

    internal lateinit var pillDot: View
    internal lateinit var pillText: TextView
    internal lateinit var kubeStatus: TextView
    internal lateinit var kubeServer: TextView
    internal lateinit var kubePath: TextView
    internal lateinit var kubeAuth: TextView
    internal lateinit var testHint: TextView
    internal lateinit var editNodeName: EditText
    internal lateinit var editPort: EditText
    internal lateinit var editLogLevel: EditText
    internal lateinit var editMirrorDocker: EditText
    internal lateinit var editMirrorK8s: EditText
    internal lateinit var editMirrorQuay: EditText
    internal lateinit var configHint: TextView
    internal lateinit var cfgRegistry: TextView
    internal lateinit var batterySub: TextView
    internal lateinit var maintainSub: TextView
    internal lateinit var agentReport: TextView
    internal lateinit var agentPowerSub: TextView
    internal lateinit var btnAgentPower: TextView
    internal lateinit var switchMaintain: Switch
    internal lateinit var runState: TextView
    internal lateinit var btnRestartAgent: TextView
    internal lateinit var aboutVersion: TextView
    internal lateinit var appearanceGroup: RadioGroup
    internal lateinit var langGroup: RadioGroup
    internal lateinit var switchAutostart: Switch
    internal lateinit var ifaceGroup: RadioGroup

    /** 程序化重建单选行时，避免触发 onCheckedChange 又去重启 agent。 */
    internal var suppressIface = false

    internal var suppressSwitch = false
    internal var suppressAppearance = false
    internal var agentOk = true
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

    private val renderer by lazy { SettingsRenderer(this) }

    private fun render() = renderer.render()

    private fun renderRunControl() = renderer.renderRunControl()

    private fun agentReportText() = renderer.agentReportText()

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

    /**
     * 运行控制区的实时刷新：状态、维护开关、重启/停止按钮。
     * 状态来自 IPC（每 10 秒）或本地 prefs（IPC 未连接时）。
     */
    /**
     * E16: structured status pushed by the agent over the Unix socket — no
     * regex parsing of stdout involved.
     */
    /**
     * 优先展示 agent 自报的生效值；没有日志（agent 还没起来 / 刚改完没重启）时，
     * 退回到已保存的配置，并明确说还没生效。
     */
    /**
     * agent 启动时会打一行 `registry mirrors configured docker=… k8s=… quay=…`
     * （或 `registry mirrors: none`）；拉取时的 `registry mirror applied` 只在实际
     * 发生改写时才有。两者都从日志里取，比界面自己猜更可靠。
     */
    /**
     * 「网络」一节：自动 + 每张 IPv4 网卡一行。
     *
     * 选中后写 files/network-preference.env，agent 启动时按网卡名解析地址；
     * 多网卡（Wi-Fi + Wi-Fi Direct/USB 共享）时，自动探测可能选到集群到不了的那张。
     */
    private fun renderInterfaces() = renderer.renderInterfaces()

    internal fun onIfacePicked(value: String) {
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

    internal fun ignoreBattery(): Boolean = try {
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
        val result = controller.saveNodeConfig(
            nameInput = editNodeName.text.toString(),
            portInput = editPort.text.toString(),
            levelInput = editLogLevel.text.toString(),
            dockerInput = editMirrorDocker.text.toString(),
            k8sInput = editMirrorK8s.text.toString(),
            quayInput = editMirrorQuay.text.toString(),
        )
        when (result) {
            is SettingsController.SaveResult.Invalid -> showConfigHint(result.message, false)
            SettingsController.SaveResult.Unchanged -> showConfigHint(Tr.s("配置没有变化"), true)
            is SettingsController.SaveResult.Saved -> {
                showConfigHint(result.message, true)
                render()
            }
        }
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
        val report = controller.diagnostics(aboutVersion.text.toString())
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
            val ok = withContext(Dispatchers.IO) { controller.setMaintenance(on) }
            switchMaintain.isEnabled = true
            if (ok) {
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
