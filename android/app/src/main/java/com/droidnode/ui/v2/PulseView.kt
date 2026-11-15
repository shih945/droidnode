package com.droidnode.ui.v2

import android.animation.ValueAnimator
import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.DashPathEffect
import android.graphics.Paint
import android.graphics.Path
import android.util.AttributeSet
import android.view.View
import android.view.animation.LinearInterpolator

/**
 * 签名元素：一条心跳线。
 *
 * 三种音量：
 *  - HERO：节点页顶部的实时波形，带一条缓慢移动的信号虚线；
 *  - MINI：分区标题左侧的小刻度；
 *  - FLAT：空状态里的一条平线带一个脉冲。
 *
 * 颜色由调用方按状态设置（运行 / 已排空 / 停止），形状本身不变。
 */
class PulseView @JvmOverloads constructor(
    context: Context,
    attrs: AttributeSet? = null,
) : View(context, attrs) {

    enum class Variant(val designWidth: Float, val designHeight: Float) {
        HERO(240f, 32f),
        MINI(24f, 8f),
        FLAT(240f, 32f),
    }

    private class Seg(val dx: Float, val dy: Float)

    private val hero = arrayOf(
        Seg(40f, 0f), Seg(2f, -9f), Seg(2f, 18f), Seg(2f, -9f),
        Seg(44f, 0f), Seg(2f, -9f), Seg(2f, 18f), Seg(2f, -9f),
        Seg(44f, 0f), Seg(2f, -9f), Seg(2f, 18f), Seg(2f, -9f),
        Seg(94f, 0f),
    )

    private val mini = arrayOf(
        Seg(6f, 0f), Seg(2f, -3f), Seg(3f, 6f), Seg(2f, -3f), Seg(11f, 0f),
    )

    private val flat = arrayOf(
        Seg(96f, 0f), Seg(6f, -10f), Seg(8f, 20f), Seg(6f, -10f), Seg(124f, 0f),
    )

    var variant: Variant = Variant.HERO
        set(value) {
            field = value
            requestLayout()
            invalidate()
        }

    /** 线本身不动，只有信号虚线在走；关掉动画时就是一条静态波形。 */
    var animate: Boolean = true
        set(value) {
            field = value
            if (value) startAnim() else stopAnim()
        }

    private val linePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeJoin = Paint.Join.ROUND
        strokeCap = Paint.Cap.ROUND
    }

    private val echoPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeJoin = Paint.Join.ROUND
        strokeCap = Paint.Cap.ROUND
    }

    private val path = Path()
    private var phase = 0f

    private val animator = ValueAnimator.ofFloat(0f, 48f).apply {
        duration = 2600L
        repeatCount = ValueAnimator.INFINITE
        interpolator = LinearInterpolator()
        addUpdateListener {
            phase = it.animatedValue as Float
            invalidate()
        }
    }

    init {
        linePaint.strokeWidth = Ui.dp(context, 1.5f).toFloat().toFloat()
        echoPaint.strokeWidth = Ui.dp(context, 1.5f).toFloat().toFloat()
        echoPaint.alpha = 70
    }

    fun setTone(color: Int) {
        linePaint.color = color
        echoPaint.color = color
        invalidate()
    }

    fun setToneRes(resId: Int) {
        setTone(context.getColor(resId))
    }

    private fun startAnim() {
        if (animator.isStarted) return
        animator.start()
    }

    private fun stopAnim() {
        animator.cancel()
        phase = 0f
        invalidate()
    }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        if (animate) startAnim()
    }

    override fun onDetachedFromWindow() {
        stopAnim()
        super.onDetachedFromWindow()
    }

    override fun onDraw(canvas: Canvas) {
        super.onDraw(canvas)
        if (width <= 0 || height <= 0) return
        buildPath()
        canvas.drawPath(path, linePaint)
        if (animate && variant == Variant.HERO) {
            echoPaint.pathEffect = DashPathEffect(
                floatArrayOf(Ui.dp(context, 6f).toFloat(), Ui.dp(context, 10f).toFloat()),
                phase,
            )
            canvas.drawPath(path, echoPaint)
        }
    }

    private fun buildPath() {
        val segs = when (variant) {
            Variant.HERO -> hero
            Variant.MINI -> mini
            Variant.FLAT -> flat
        }
        val sx = width / variant.designWidth
        val sy = height / variant.designHeight
        val mid = height / 2f
        path.reset()
        var x = 0f
        var y = mid
        path.moveTo(0f, mid)
        for (s in segs) {
            x += s.dx * sx
            y += s.dy * sy
            path.lineTo(x, y)
        }
    }

    /** 停止时用一条几乎水平的线，避免把「节点挂了」画成心电图骤停。 */
    fun setStopped(stopped: Boolean) {
        if (stopped) {
            linePaint.alpha = 120
            echoPaint.alpha = 0
        } else {
            linePaint.alpha = 255
            echoPaint.alpha = 70
        }
        invalidate()
    }

    companion object {
        @Suppress("unused")
        const val DEFAULT_TONE = Color.WHITE
    }
}
