package com.droidnode.ui.v2

import android.app.Activity
import android.view.View
import android.widget.TextView
import com.droidnode.R
import com.droidnode.exposers.ForegroundServiceExposer

/**
 * 顶栏状态徽标：所有页面共用一套判定，避免同一时刻不同页显示不同状态。
 * 色点 + 文字，颜色只是辅助。
 */
object StatusPill {

    data class State(val toneRes: Int, val label: String)

    fun compute(agentOk: Boolean, drained: Boolean): State {
        val running = ForegroundServiceExposer.isAgentRunning
        return when {
            !running -> State(R.color.bad, Tr.s("已停止"))
            !agentOk -> State(R.color.bad, Tr.s("无响应"))
            drained -> State(R.color.lamp, Tr.s("已排空"))
            else -> State(R.color.ok, Tr.s("运行中"))
        }
    }

    fun render(activity: Activity, dot: View, text: TextView, agentOk: Boolean, drained: Boolean) {
        val state = compute(agentOk, drained)
        Ui.tintDot(dot, activity.getColor(state.toneRes))
        text.text = state.label
    }
}
