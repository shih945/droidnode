package com.droidnode.ui.v2

import android.app.Activity
import android.app.ActivityOptions
import android.content.Intent
import android.content.res.ColorStateList
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import com.droidnode.R

/**
 * 底栏只有五个目的地，切换时结束当前页，避免退栈里堆一串控制台。
 * 图标与文字一起变，不只靠颜色。
 */
object BottomNav {

    enum class Screen(val itemId: Int, val target: Class<out Activity>) {
        NODE(R.id.navNode, NodeActivity::class.java),
        PODS(R.id.navPods, PodsActivity::class.java),
        IMAGES(R.id.navImages, ImagesActivity::class.java),
        LOGS(R.id.navLogs, LogsActivity::class.java),
        SETTINGS(R.id.navSettings, SettingsActivity::class.java),
    }

    fun wire(activity: Activity, current: Screen) {
        for (screen in Screen.values()) {
            val item = activity.findViewById<LinearLayout>(screen.itemId) ?: continue
            val active = screen == current
            item.isActivated = active
            val icon = item.getChildAt(0) as? ImageView
            val label = item.getChildAt(1) as? TextView
            icon?.imageTintList =
                ColorStateList.valueOf(activity.getColor(if (active) R.color.lamp else R.color.text_3))
            label?.setTextColor(activity.getColor(if (active) R.color.text else R.color.text_3))
            item.setOnClickListener { if (!active) open(activity, screen) }
        }
    }

    /**
     * 切换到底栏的另一个目的地。
     *
     * 底栏切换不应该有页面转场：原生的 BottomNavigationView 是同一个 Activity 里换
     * Fragment，天然没有动画、状态保留。这里是 5 个独立 Activity，所以：
     *  - FLAG_ACTIVITY_REORDER_TO_FRONT 复用已存在的页面（滚动位置、输入不丢）；
     *  - FLAG_ACTIVITY_NO_ANIMATION + overridePendingTransition(0,0) 关掉转场；
     *  - 不 finish 当前页，返回键由 BaseActivity 统一处理（从任意 tab 退出）。
     */
    fun open(activity: Activity, screen: Screen) {
        val intent = Intent(activity, screen.target).apply {
            addFlags(Intent.FLAG_ACTIVITY_REORDER_TO_FRONT)
            addFlags(Intent.FLAG_ACTIVITY_NO_ANIMATION)
        }
        // 0/0 = 不要进入/退出动画。ActivityOptions 在所有版本都有效，
        // 不像 overridePendingTransition 在 API 34 已废弃。
        val options = ActivityOptions.makeCustomAnimation(activity, 0, 0).toBundle()
        activity.startActivity(intent, options)
    }
}
