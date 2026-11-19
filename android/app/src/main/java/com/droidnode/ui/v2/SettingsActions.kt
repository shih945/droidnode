package com.droidnode.ui.v2

import android.content.Intent
import android.view.View
import androidx.core.content.ContextCompat
import com.droidnode.exposers.ForegroundServiceExposer
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import com.droidnode.R

/** Save/maintenance actions for the settings screen. */
class SettingsActions(private val a: SettingsActivity) {

    internal fun saveNodeConfig() {
        val result = a.controller.saveNodeConfig(
            nameInput = a.editNodeName.text.toString(),
            portInput = a.editPort.text.toString(),
            levelInput = a.editLogLevel.text.toString(),
            dockerInput = a.editMirrorDocker.text.toString(),
            k8sInput = a.editMirrorK8s.text.toString(),
            quayInput = a.editMirrorQuay.text.toString(),
        )
        when (result) {
            is SettingsController.SaveResult.Invalid -> showConfigHint(result.message, false)
            SettingsController.SaveResult.Unchanged -> showConfigHint(Tr.s("配置没有变化"), true)
            is SettingsController.SaveResult.Saved -> {
                showConfigHint(result.message, true)
                a.render()
            }
        }
    }


    private fun showConfigHint(message: String, ok: Boolean) {
        a.configHint.visibility = View.VISIBLE
        a.configHint.setTextColor(a.getColor(if (ok) R.color.ok else R.color.bad))
        a.configHint.text = message
    }


    internal fun requestMaintenance(on: Boolean) {
        a.switchMaintain.isEnabled = false
        a.scope.launch {
            val ok = withContext(Dispatchers.IO) { a.controller.setMaintenance(on) }
            a.switchMaintain.isEnabled = true
            if (ok) {
                Sheets.toast(
                    a,
                    if (on) Tr.s("已进入维护模式 · 节点已 cordon 并排空") else Tr.s("已退出维护模式 · 恢复调度"),
                )
            } else {
                Sheets.toast(a, Tr.s("agent 无响应，维护模式切换失败"))
                a.suppressSwitch = true
                a.switchMaintain.isChecked = !on
                a.suppressSwitch = false
            }
            a.render()
        }
    }



    /**
     * Settings can be opened directly (recents restore, `am start` in tests) —
     * 但用户如果按过「停止 agent」，这里不能把它重新拉起来：停止是持久意图。
     */
    internal fun ensureAgentStarted() {
        if (ForegroundServiceExposer.isAgentRunning) return
        if (LocalConfig.agentStopped(a)) return
        startAgentService()
    }

    private fun startAgentService() {
        try {
            ContextCompat.startForegroundService(
                a,
                Intent(a, ForegroundServiceExposer::class.java),
            )
        } catch (e: Exception) {
            android.util.Log.w("SettingsActivity", Tr.s("启动前台服务失败: %1\$s", e.message))
        }
    }

    /**
     * Explicit start/stop. Stopping goes through the service so onDestroy
     * also kills the agent child (no orphan proot holding the kubelet port).
     */
    internal fun toggleAgent() {
        if (ForegroundServiceExposer.isAgentRunning) {
            ForegroundServiceExposer.requestStop(a)
            Sheets.toast(a, Tr.s("正在停止 agent…"))
        } else {
            // 显式启动：清掉"用户停止"意图，之后打开页面/开机才会正常自动拉起
            LocalConfig.setAgentStopped(a, false)
            try {
                ContextCompat.startForegroundService(
                    a,
                    Intent(a, ForegroundServiceExposer::class.java),
                )
                Sheets.toast(a, Tr.s("正在启动 agent…"))
            } catch (e: Exception) {
                Sheets.toast(a, Tr.s("启动失败：%1\$s", e.message))
                return
            }
        }
        a.scope.launch {
            delay(1500L)
            a.render()
        }
    }


}
