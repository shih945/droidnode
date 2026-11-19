package com.droidnode.ui.v2

import android.app.Activity
import android.content.Context
import android.view.View
import android.widget.TextView
import com.droidnode.R
import com.droidnode.brokers.AgentStatus
import com.droidnode.exposers.ForegroundServiceExposer

/**
 * agent 状态的唯一判定入口。
 *
 * 顶栏徽标、设置首页的状态卡、各页面标题右侧的胶囊都从这里取 [State]，
 * 所以同一时刻任何地方看到的文字和色点都一致——之前卡片自己算一套
 * （运行中 / 维护中 / 已停止）、徽标算另一套（多了「无响应」），
 * 刚启动的那几秒会出现"卡片运行中、徽标已停止"。
 */
object StatusPill {

    data class State(val toneRes: Int, val label: String)

    /**
     * 状态优先级：没在跑 > 在跑但不应答 > 维护 > 低电排空 > 正常。
     *
     * [agentOk] 是 kubelet 健康探测的结果；服务刚启动的前几秒可能还没应答，
     * 这时显示「无响应」而不是「运行中」，避免界面比实际更乐观。
     */
    fun compute(agentOk: Boolean, context: Context): State {
        val running = ForegroundServiceExposer.isAgentRunning
        val maintenance = if (AgentStatus.connected) {
            AgentStatus.maintenance
        } else {
            LocalConfig.drained(context)
        }
        val batteryDrained = AgentStatus.connected && AgentStatus.drained && !maintenance
        return when {
            !running -> State(R.color.bad, Tr.s("已停止"))
            !agentOk -> State(R.color.bad, Tr.s("无响应"))
            maintenance -> State(R.color.lamp, Tr.s("维护中"))
            batteryDrained -> State(R.color.lamp, Tr.s("已排空"))
            else -> State(R.color.ok, Tr.s("运行中"))
        }
    }

    fun render(activity: Activity, dot: View, text: TextView, agentOk: Boolean) {
        val state = compute(agentOk, activity)
        Ui.tintDot(dot, activity.getColor(state.toneRes))
        text.text = state.label
    }
}
