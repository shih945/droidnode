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

    internal fun ignoreBattery(): Boolean = try {
        val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
        pm.isIgnoringBatteryOptimizations(packageName)
    } catch (e: Exception) {
        false
    }

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
     * E14：保存节点名 / kubelet 端口 / 日志级别，以及镜像源（写 files/registry-mirrors.env）。
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
