package com.droidnode.ui.v2

import android.content.Context
import com.droidnode.LogBuffer
import com.droidnode.brokers.AgentStatus
import com.droidnode.data.AgentApi
import com.droidnode.data.Validation
import com.droidnode.exposers.ForegroundServiceExposer
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * Non-UI settings operations: validating + persisting the node/mirror config,
 * building the diagnostics report and flipping maintenance mode. The Activity
 * keeps only view binding and feedback.
 */
class SettingsController(private val ctx: Context) {

    sealed class SaveResult {
        data class Invalid(val message: String) : SaveResult()
        object Unchanged : SaveResult()
        data class Saved(val message: String) : SaveResult()
    }

    /**
     * Node name / kubelet port / log level go through environment variables and
     * the mirrors through a file; both are read at agent startup, so a save
     * always requests a restart. Mirrors are written first: if that fails we do
     * not restart into a half-applied config.
     */
    fun saveNodeConfig(
        nameInput: String,
        portInput: String,
        levelInput: String,
        dockerInput: String,
        k8sInput: String,
        quayInput: String,
    ): SaveResult {
        val name = nameInput.trim()
        if (!Validation.validNodeName(name)) {
            return SaveResult.Invalid(
                Tr.s("节点名只能是小写字母、数字、'-'、'.'，且以字母或数字开头结尾"),
            )
        }
        val port = Validation.parsePort(portInput)
            ?: return SaveResult.Invalid(
                Tr.s("端口必须是 1024–65535 的整数（非 root 不能监听 <1024）"),
            )
        val level = levelInput.trim().lowercase()
        if (!Validation.validLogLevel(level, LocalConfig.LOG_LEVELS)) {
            return SaveResult.Invalid(Tr.s("日志级别只能是 error / warn / info / debug / trace"))
        }
        val docker = LocalConfig.normalizeMirrorHost(dockerInput)
        val k8s = LocalConfig.normalizeMirrorHost(k8sInput)
        val quay = LocalConfig.normalizeMirrorHost(quayInput)
        if (docker == null || k8s == null || quay == null) {
            return SaveResult.Invalid(Tr.s("镜像源只填主机名（可带 :端口），不要带 http:// 或路径"))
        }

        val storedName = LocalConfig.nodeName(ctx).trim()
        val oldName = LocalConfig.resolveNodeId(ctx)
        val oldPort = LocalConfig.kubeletPort(ctx)
        val oldLevel = LocalConfig.logLevel(ctx)
        val oldMirrors = LocalConfig.readRegistryMirrors(ctx)
        val nodesChanged = name != storedName || port != oldPort || level != oldLevel
        val mirrorsChanged = docker != oldMirrors[LocalConfig.MIRROR_DOCKER].orEmpty() ||
            k8s != oldMirrors[LocalConfig.MIRROR_K8S].orEmpty() ||
            quay != oldMirrors[LocalConfig.MIRROR_QUAY].orEmpty()
        if (!nodesChanged && !mirrorsChanged) return SaveResult.Unchanged

        if (mirrorsChanged && !LocalConfig.writeRegistryMirrors(ctx, docker, k8s, quay)) {
            return SaveResult.Invalid(Tr.s("写入 registry-mirrors.env 失败，检查应用存储"))
        }

        if (nodesChanged) {
            LocalConfig.setNodeName(ctx, name)
            LocalConfig.setKubeletPort(ctx, port)
            LocalConfig.setLogLevel(ctx, level)
            // Point the UI at the new port immediately: the restart takes a moment.
            AgentApi.port = port
        }
        ForegroundServiceExposer.requestRestart(ctx)

        val renamed = LocalConfig.resolveNodeId(ctx) != oldName
        val what = when {
            nodesChanged && mirrorsChanged -> Tr.s("节点配置与镜像源")
            mirrorsChanged -> Tr.s("镜像源")
            else -> Tr.s("节点配置")
        }
        return SaveResult.Saved(
            Tr.s("已保存（%1\$s），agent 正在按新配置重启", what) +
                if (renamed) Tr.s("；新节点名会在集群里新建 Node，旧节点不会自动删除") else "",
        )
    }

    /** Diagnostics text; deliberately excludes kubeconfig/token/CA. */
    fun diagnostics(version: String): String {
        val kube = LocalConfig.readKubeconfig(ctx)
        val clock = SimpleDateFormat("HH:mm:ss", Locale.US)
        val stamp = SimpleDateFormat("yyyy-MM-dd HH:mm:ss", Locale.US)
        return buildString {
            appendLine(Tr.s("DroidNode 诊断"))
            appendLine("time: " + stamp.format(Date()))
            appendLine("version: " + version)
            appendLine("node: " + LocalConfig.resolveNodeId(ctx))
            appendLine("server: " + (LocalConfig.serverFrom(kube) ?: "-"))
            appendLine("identity: " + LocalConfig.authLabel(kube))
            appendLine("appearance: " + LocalConfig.appearanceLabel(LocalConfig.appearance(ctx)))
            appendLine("kubelet port: " + LocalConfig.kubeletPort(ctx))
            appendLine("--- logs (last 200) ---")
            LogBuffer.snapshot().takeLast(200).forEach {
                appendLine(clock.format(Date(it.timeMs)) + " " + it.text)
            }
        }
    }

    /** Returns true when the agent accepted the switch. */
    fun setMaintenance(on: Boolean): Boolean {
        val response = AgentApi.post("/maintenance?on=$on", 8000)
        val ok = response != null && response.code in 200..299
        if (ok) {
            LocalConfig.setDrained(ctx, on)
            // Optimistic update; the next IPC sample confirms it.
            AgentStatus.maintenance = on
            AgentStatus.cordoned = on
        }
        return ok
    }
}
