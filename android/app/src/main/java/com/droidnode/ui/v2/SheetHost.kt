package com.droidnode.ui.v2

import android.app.Activity
import android.os.Build
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.FrameLayout
import com.droidnode.R

/**
 * 底部弹层。不引入 Material 组件库，直接在 Activity 的 content 上叠一层：
 * 蒙层 + 贴底的卡片，上下各动画一次。
 *
 * heightFraction 为 null 时按内容高度（确认、导入这类短弹层），
 * 否则固定屏高比例，内容自己滚动（Pod / 镜像详情）。
 */
class SheetHost(private val activity: Activity) {

    private val root: FrameLayout = activity.findViewById(android.R.id.content)
    private var overlay: FrameLayout? = null
    private var panel: FrameLayout? = null
    private var onDismiss: (() -> Unit)? = null

    val isShowing: Boolean get() = overlay != null

    fun show(content: View, heightFraction: Float? = null, onDismiss: (() -> Unit)? = null) {
        removeNow()
        this.onDismiss = onDismiss

        val avail = if (root.height > 0) root.height else activity.resources.displayMetrics.heightPixels
        val o = FrameLayout(activity)
        val scrim = View(activity).apply {
            setBackgroundColor(activity.getColor(R.color.scrim))
            alpha = 0f
            setOnClickListener { dismiss() }
        }
        o.addView(scrim, FrameLayout.LayoutParams(MATCH_PARENT, MATCH_PARENT))

        val p = FrameLayout(activity)
        p.setBackgroundResource(R.drawable.bg_sheet)
        val h = if (heightFraction == null) ViewGroup.LayoutParams.WRAP_CONTENT
        else (avail * heightFraction).toInt().coerceAtLeast(1)
        p.layoutParams = FrameLayout.LayoutParams(MATCH_PARENT, h, Gravity.BOTTOM)
        p.addView(
            content,
            FrameLayout.LayoutParams(
                MATCH_PARENT,
                if (heightFraction == null) ViewGroup.LayoutParams.WRAP_CONTENT else MATCH_PARENT,
            ),
        )
        p.translationY = avail.toFloat()
        o.addView(p)
        root.addView(o)
        overlay = o
        panel = p

        p.post {
            p.animate().translationY(0f).setDuration(200L).start()
            scrim.animate().alpha(1f).setDuration(180L).start()
        }
    }

    fun dismiss() {
        val o = overlay ?: return
        val p = panel
        overlay = null
        panel = null
        val callback = onDismiss
        onDismiss = null

        if (p == null) {
            root.removeView(o)
            callback?.invoke()
            return
        }
        val scrim = o.getChildAt(0)
        scrim?.setOnClickListener(null)
        p.animate()
            .translationY(p.height.toFloat())
            .setDuration(180L)
            .withEndAction {
                root.removeView(o)
                callback?.invoke()
            }
            .start()
        scrim?.animate()?.alpha(0f)?.setDuration(160L)?.start()
    }

    private fun removeNow() {
        overlay?.let { root.removeView(it) }
        overlay = null
        panel = null
        onDismiss = null
    }

    companion object {
        private const val MATCH_PARENT = ViewGroup.LayoutParams.MATCH_PARENT

        @Suppress("unused")
        val MIN_SDK_CHECK = Build.VERSION.SDK_INT
    }
}
