package com.droidnode.ui.v2

import com.droidnode.data.KubeConnectionTest

import com.droidnode.data.AgentApi
import com.droidnode.data.Validation

import android.app.Activity
import com.droidnode.ui.BaseActivity
import android.content.Intent
import android.content.res.ColorStateList
import android.net.Uri
import android.os.Bundle
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

    internal val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    internal val controller by lazy { SettingsController(this) }
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
    internal lateinit var btnStopAgent: TextView
    internal lateinit var btnStartAgent: TextView
    internal lateinit var switchMaintain: Switch
    internal lateinit var runState: TextView
    internal lateinit var btnRestartAgent: TextView
    internal lateinit var aboutVersion: TextView
    internal lateinit var appearanceGroup: RadioGroup
    internal lateinit var langGroup: RadioGroup
    internal lateinit var switchAutostart: Switch
    internal lateinit var switchKeepOnline: Switch
    internal lateinit var ifaceGroup: RadioGroup

    // ── 首页 + 五个二级页（单 Activity 内切页，返回键先回首页） ──────────────
    enum class Page { HUB, NODE, CLUSTER, RUNTIME, POWER, APP }

    internal var page = Page.HUB
    internal lateinit var pill: View
    internal lateinit var settingsTitle: TextView
    internal lateinit var settingsBack: View
    internal lateinit var settingsScroll: android.widget.ScrollView
    internal lateinit var bottomNav: View
    internal lateinit var pageHub: View
    internal lateinit var pageNode: View
    internal lateinit var pageCluster: View
    internal lateinit var pageRuntime: View
    internal lateinit var pagePower: View
    internal lateinit var pageApp: View

    internal lateinit var hubNode: TextView
    internal lateinit var hubControl: TextView
    internal lateinit var hubState: TextView
    internal lateinit var hubRestart: TextView
    internal lateinit var hubStop: TextView
    internal lateinit var hubStart: TextView
    internal lateinit var hubEntries: android.widget.LinearLayout

    /** 二级页入口的副标题；renderHub 每次刷新成当前值。 */
    internal val entrySubs = LinkedHashMap<Page, TextView>()

    internal fun showPage(target: Page) {
        page = target
        val pages = mapOf(
            Page.HUB to pageHub,
            Page.NODE to pageNode,
            Page.CLUSTER to pageCluster,
            Page.RUNTIME to pageRuntime,
            Page.POWER to pagePower,
            Page.APP to pageApp,
        )
        pages.forEach { (p, view) -> view.visibility = if (p == target) View.VISIBLE else View.GONE }

        val secondary = target != Page.HUB
        settingsBack.visibility = if (secondary) View.VISIBLE else View.GONE
        pill.visibility = if (secondary) View.GONE else View.VISIBLE
        // 二级页和原型一致：隐藏底栏，返回首页再换 tab
        bottomNav.visibility = if (secondary) View.GONE else View.VISIBLE
        settingsTitle.setText(
            when (target) {
                Page.HUB -> R.string.v2_settings_01
                Page.NODE -> R.string.v2_settings_48
                Page.CLUSTER -> R.string.v2_settings_49
                Page.RUNTIME -> R.string.v2_settings_50
                Page.POWER -> R.string.v2_settings_28
                Page.APP -> R.string.v2_settings_51
            },
        )
        settingsScroll.scrollTo(0, 0)
        render()
    }

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

        SettingsBinder(this).bind()
    }

    internal fun ensureAgentStarted() = actions.ensureAgentStarted()

    internal fun toggleAgent() = actions.toggleAgent()

    private val renderer by lazy { SettingsRenderer(this) }

    internal fun render() = renderer.render()

    private fun renderRunControl() = renderer.renderRunControl()

    private fun agentReportText() = renderer.agentReportText()

    override fun onResume() {
        super.onResume()
        render()
        // Wi-Fi 开关会改变网卡列表，回到设置页时重新枚举一次
        renderInterfaces()
        // The agent pushes status every 10 s; poll the in-memory snapshot
        // so the "agent 上报" row stays live instead of freezing at onResume.
        statusJob?.cancel()
        statusJob = scope.launch {
            while (isActive) {
                agentReport.text = agentReportText()
                // 徽标和卡片共用 StatusPill.compute，两个输入都必须是最新的
                agentOk = withContext(Dispatchers.IO) { Snapshot.health() }
                StatusPill.render(this@SettingsActivity, pillDot, pillText, agentOk)
                renderRunControl()
                // 首页在可见时才需要刷新；隐藏页刷新也不会显示，但代价是几个字符串拼接
                if (page == Page.HUB) renderer.renderHub()
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
        when {
            sheet.isShowing -> sheet.dismiss()
            // 二级页先回首页；首页再按一次才退出（tab root 语义）
            page != Page.HUB -> showPage(Page.HUB)
            else -> super.onBackPressed()
        }
    }

    internal fun renderInterfaces() = renderer.renderInterfaces()

    private val actions by lazy { SettingsActions(this) }

    internal fun saveNodeConfig() = actions.saveNodeConfig()

    internal fun requestMaintenance(on: Boolean) = actions.requestMaintenance(on)

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

    internal fun askRestart() {
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

    /**
     * 停止比重启后果更重（节点立刻掉线、容器全停），所以也走二次确认。
     * 确认后才真正下发停止请求。
     */
    internal fun askStop() {
        if (!ForegroundServiceExposer.isAgentRunning) {
            Sheets.toast(this, Tr.s("agent 已停止，请先启动"))
            return
        }
        Sheets.confirm(
            this,
            sheet,
            Tr.s("停止 agent"),
            Tr.s("节点会立即从集群掉线，所有容器停止；停止时会先 cordon 再删 Pod。之后可以在这里重新启动。"),
            Tr.s("停止"),
        ) { stopAgent() }
    }

    private fun stopAgent() {
        ForegroundServiceExposer.requestStop(this)
        Sheets.toast(this, Tr.s("正在停止 agent…"))
        scope.launch {
            delay(1500L)
            render()
        }
    }

    private fun restartAgent() {
        ForegroundServiceExposer.requestRestart(this)
        Sheets.toast(this, Tr.s("正在重启 agent，稍后可在节点页看状态"))
        render()
    }

    internal fun testConnection() {
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

    internal fun ignoreBattery(): Boolean = LocalConfig.batteryExempt(this)

    internal fun requestBatteryExemption() {
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
     * 保存节点名 / kubelet 端口 / 日志级别，以及镜像源（写 files/registry-mirrors.env）。
     * 节点三项走环境变量、镜像源走配置文件，都在 agent 启动时读取，所以保存后必须重启 agent；
     * 重启会重建运行中的容器。
     */
    /**
     * 诊断导出：把版本、节点身份、外观和最近 200 行日志拼成文本，走系统分享。
     * 不含 kubeconfig / token / CA，避免Tr.s("诊断包")变成凭据泄露。
     */
    internal fun exportDiagnostics() {
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

    private companion object {
        
    }
}