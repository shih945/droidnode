package com.droidnode.ui.v2

import android.content.res.ColorStateList
import android.view.View
import android.widget.EditText
import android.widget.RadioButton
import android.widget.RadioGroup
import android.widget.Switch
import android.widget.TextView

import com.droidnode.LogBuffer
import com.droidnode.R
import com.droidnode.brokers.AgentStatus
import com.droidnode.exposers.ForegroundServiceExposer

/**
 * Rendering for the settings screen. The Activity no longer owns the view
 * logic; it keeps binding/listeners and delegates here.
 */
class SettingsRenderer(private val a: SettingsActivity) {

    internal fun render() {
        val maintenance = if (AgentStatus.connected) {
            AgentStatus.maintenance
        } else {
            LocalConfig.drained(a)
        }
        val batteryDrained = AgentStatus.connected && AgentStatus.drained && !maintenance
        StatusPill.render(a, a.pillDot, a.pillText, a.agentOk)

        val kube = LocalConfig.readKubeconfig(a)
        val server = LocalConfig.serverFrom(kube)
        a.kubeStatus.text = if (server != null) Tr.s("已加入") else Tr.s("未配置")
        a.kubeServer.text = server ?: "—"
        a.kubePath.text = LocalConfig.kubeconfigFile(a).absolutePath
        a.kubeAuth.text = LocalConfig.authLabel(kube)

        a.editNodeName.hint = LocalConfig.autoNodeId(a)
        if (!a.editNodeName.hasFocus()) a.editNodeName.setText(LocalConfig.nodeName(a))
        if (!a.editPort.hasFocus()) a.editPort.setText(LocalConfig.kubeletPort(a).toString())
        if (!a.editLogLevel.hasFocus()) a.editLogLevel.setText(LocalConfig.logLevel(a))
        val mirrors = LocalConfig.readRegistryMirrors(a)
        if (!a.editMirrorDocker.hasFocus()) {
            a.editMirrorDocker.setText(mirrors[LocalConfig.MIRROR_DOCKER].orEmpty())
        }
        if (!a.editMirrorK8s.hasFocus()) {
            a.editMirrorK8s.setText(mirrors[LocalConfig.MIRROR_K8S].orEmpty())
        }
        if (!a.editMirrorQuay.hasFocus()) {
            a.editMirrorQuay.setText(mirrors[LocalConfig.MIRROR_QUAY].orEmpty())
        }
        a.cfgRegistry.text = registryStatusText(mirrors)
        a.suppressAppearance = true
        a.appearanceGroup.check(
            when (LocalConfig.appearance(a)) {
                LocalConfig.APPEARANCE_SYSTEM -> R.id.appearanceSystem
                LocalConfig.APPEARANCE_DARK -> R.id.appearanceDark
                else -> R.id.appearanceLight
            },
        )
        a.langGroup.check(
            when (LocalConfig.language(a)) {
                LocalConfig.LANGUAGE_ZH -> R.id.langZh
                LocalConfig.LANGUAGE_EN -> R.id.langEn
                else -> R.id.langSystem
            },
        )
        a.suppressAppearance = false
        a.agentReport.text = agentReportText()
        val running = ForegroundServiceExposer.isAgentRunning
        a.agentPowerSub.text = if (running) {
            Tr.s("运行中，前台服务与 WakeLock 生效")
        } else {
            Tr.s("已停止，节点会从集群掉线")
        }
        a.btnStopAgent.visibility = if (running) View.VISIBLE else View.GONE
        a.btnStartAgent.visibility = if (running) View.GONE else View.VISIBLE

        val ignoring = a.ignoreBattery()
        a.batterySub.text = if (ignoring) {
            Tr.s("已加入，息屏后不会被系统清理")
        } else {
            Tr.s("未加入，息屏后可能被系统清理")
        }

        renderRunControl()
        renderHub()
    }

    /**
     * 首页：状态摘要 + 五个入口的当前值。
     *
     * 副标题就是"这一页现在是什么样"，多数情况不用点进去看。
     */
    internal fun renderHub() {
        val running = ForegroundServiceExposer.isAgentRunning
        val nodeName = LocalConfig.nodeName(a).ifEmpty { LocalConfig.autoNodeId(a) }
        val port = LocalConfig.kubeletPort(a)
        val iface = LocalConfig.nodeIface(a)
        val ifaceLabel = if (iface.isEmpty()) Tr.s("自动选择") else iface
        val server = LocalConfig.serverFrom(LocalConfig.readKubeconfig(a))
        // 和顶栏徽标共用同一个判定，避免"卡片运行中、徽标已停止"
        val status = StatusPill.compute(a.agentOk, a)
        val state = status.label

        val nodeSummary = "$nodeName · :$port · $ifaceLabel"
        a.hubNode.text = nodeSummary
        a.hubControl.text = server ?: Tr.s("未加入，节点无法注册")
        a.hubState.text = state
        a.hubState.setTextColor(a.getColor(status.toneRes))
        a.hubRestart.isEnabled = running
        a.hubRestart.alpha = if (running) 1f else 0.4f
        a.hubRestart.visibility = if (running) View.VISIBLE else View.GONE
        a.hubStop.visibility = if (running) View.VISIBLE else View.GONE
        a.hubStart.visibility = if (running) View.GONE else View.VISIBLE

        a.entrySubs[SettingsActivity.Page.NODE]?.text = nodeSummary
        a.entrySubs[SettingsActivity.Page.CLUSTER]?.text =
            if (server != null) Tr.s("已加入 · %1\$s", server) else Tr.s("未加入，节点无法注册")
        a.entrySubs[SettingsActivity.Page.RUNTIME]?.text =
            if (running) state + " · " + Tr.s("%1\$s 个 Pod", AgentStatus.podsRunning) else state
        a.entrySubs[SettingsActivity.Page.POWER]?.text = listOf(
            if (a.ignoreBattery()) Tr.s("电池已豁免") else Tr.s("电池未豁免"),
            if (LocalConfig.autostart(a)) Tr.s("开机自启") else Tr.s("不自启"),
            if (LocalConfig.keepOnline(a)) Tr.s("锁屏保持") else Tr.s("锁屏省电"),
        ).joinToString(" · ")
        a.entrySubs[SettingsActivity.Page.APP]?.text =
            appearanceLabel() + " · " + languageLabel() + " · " + a.aboutVersion.text
    }

    private fun appearanceLabel(): String = when (LocalConfig.appearance(a)) {
        LocalConfig.APPEARANCE_SYSTEM -> Tr.s("跟随系统")
        LocalConfig.APPEARANCE_DARK -> Tr.s("深色")
        else -> Tr.s("亮色")
    }

    private fun languageLabel(): String = when (LocalConfig.language(a)) {
        LocalConfig.LANGUAGE_ZH -> Tr.s("简体中文")
        LocalConfig.LANGUAGE_EN -> Tr.s("English")
        else -> Tr.s("系统语言")
    }


    internal fun renderRunControl() {
        val running = ForegroundServiceExposer.isAgentRunning
        val maintenance = if (AgentStatus.connected) {
            AgentStatus.maintenance
        } else {
            LocalConfig.drained(a)
        }
        val batteryDrained = AgentStatus.connected && AgentStatus.drained && !maintenance
        // 同一份判定；这里只把「该做什么」补在后面
        val status = StatusPill.compute(a.agentOk, a)
        a.runState.text = when {
            !running -> Tr.s("已停止 · 节点已从集群掉线")
            !a.agentOk -> Tr.s("无响应 · agent 进程在跑但没有应答")
            maintenance -> Tr.s("维护中（已 cordon + 排空）")
            batteryDrained -> Tr.s("低电排空中 · 电量恢复后自动恢复")
            else -> status.label
        }
        a.runState.setTextColor(a.getColor(status.toneRes))

        a.suppressSwitch = true
        a.switchMaintain.isChecked = maintenance
        a.switchMaintain.isEnabled = running
        a.suppressSwitch = false
        a.maintainSub.text = when {
            !running -> Tr.s("agent 已停止；启动后才能开启维护模式")
            maintenance -> Tr.s("已 cordon 并排空：调度器不再投递新 Pod，本机容器已停止")
            else -> Tr.s("开启后先 cordon 节点，再停止本机容器；关闭后恢复调度")
        }
        a.btnRestartAgent.isEnabled = running
        a.btnRestartAgent.alpha = if (running) 1f else 0.4f
        a.agentPowerSub.text = if (running) {
            Tr.s("运行中，前台服务与 WakeLock 生效")
        } else {
            Tr.s("已停止，节点会从集群掉线")
        }
        a.btnStopAgent.visibility = if (running) View.VISIBLE else View.GONE
        a.btnStartAgent.visibility = if (running) View.GONE else View.VISIBLE
    }


    internal fun agentReportText(): String {
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


    internal fun registryStatusText(mirrors: Map<String, String>): String {
        registryFromLogs()?.let { return it }
        val configured = LocalConfig.MIRROR_KEYS.mapNotNull { mirrors[it] }
        return if (configured.isEmpty()) {
            Tr.s("未配置 · 直连上游")
        } else {
            Tr.s("已保存，agent 重启后生效：%1\$s", configured.first())
        }
    }


    internal fun registryFromLogs(): String? =
        when (val parsed = SettingsText.registryFromLogs(LogBuffer.snapshot().map { it.text })) {
            is SettingsText.Registry.Configured -> {
                val parts = listOf(
                    "docker.io" to parsed.docker,
                    "registry.k8s.io" to parsed.k8s,
                    "quay.io" to parsed.quay,
                ).filter { it.second.isNotEmpty() }
                if (parts.isEmpty()) {
                    null
                } else {
                    parts.joinToString(" · ") { "${it.first} → ${it.second}" }
                }
            }
            SettingsText.Registry.None -> Tr.s("未配置 · 直连上游")
            is SettingsText.Registry.Applied -> Tr.s("已改写 → ") + parsed.target
            null -> null
        }


    internal fun renderInterfaces() {
        val ifaces = LocalConfig.listIpv4Interfaces().sortedBy { it.first }

        a.suppressIface = true
        a.ifaceGroup.removeAllViews()
        addIfaceOption("", Tr.s("自动（推荐）· 跳过 Wi-Fi Direct / USB 共享"))
        ifaces.forEach { (name, ip) ->
            val tail = if (LocalConfig.isTetherLike(name)) Tr.s(" · 集群不可达") else ""
            addIfaceOption(name, "$name · $ip$tail")
        }
        a.suppressIface = false
    }


    private fun addIfaceOption(value: String, label: String) {
        val radio = RadioButton(a)
        radio.id = View.generateViewId()
        radio.tag = value
        radio.text = label
        radio.textSize = 14f
        radio.setTextColor(a.getColor(R.color.text))
        radio.buttonTintList = ColorStateList.valueOf(a.getColor(R.color.lamp))
        radio.minHeight = a.resources.getDimensionPixelSize(R.dimen.tap)
        radio.isChecked = value.equals(LocalConfig.nodeIface(a), ignoreCase = true)
        radio.setOnClickListener {
            if (!a.suppressIface) a.onIfacePicked(value)
        }
        a.ifaceGroup.addView(radio)
    }
}
