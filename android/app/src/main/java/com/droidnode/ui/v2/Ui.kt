package com.droidnode.ui.v2

import android.content.Context
import android.content.res.ColorStateList
import android.graphics.Color
import android.view.View
import android.widget.TextView
import com.droidnode.R
import java.util.Locale
import kotlin.math.roundToInt

/**
 * 控制台公共显示逻辑：尺寸换算、字节与时长格式、Pod 阶段的中文映射。
 *
 * 这个包（ui.v2）是独立的新界面，不引用也不修改旧的 MainActivity / DebugActivity /
 * ImagesActivity，切换入口时只改 Manifest。
 */
object Ui {

    fun dp(ctx: Context, value: Float): Int = (value * ctx.resources.displayMetrics.density).roundToInt()

    fun humanBytes(bytes: Long): String {
        if (bytes <= 0L) return "0 B"
        val units = arrayOf("B", "KB", "MB", "GB", "TB")
        var v = bytes.toDouble()
        var i = 0
        while (v >= 1024 && i < units.size - 1) {
            v /= 1024
            i++
        }
        return if (i == 0) "$bytes B" else String.format(Locale.US, "%.1f %s", v, units[i])
    }

    fun uptime(since: Long): String {
        if (since <= 0L) return "—"
        val total = ((System.currentTimeMillis() - since) / 1000L).coerceAtLeast(0L)
        val h = total / 3600
        val m = (total % 3600) / 60
        val s = total % 60
        return when {
            h > 0 -> Tr.s("%1\$s 小时 %2\$s 分", h, m)
            m > 0 -> Tr.s("%1\$s 分 ", m) + s.toString().padStart(2, '0') + Tr.s(" 秒")
            else -> Tr.s("%1\$s 秒", s)
        }
    }

    /** 解析 agent 返回的 RFC3339（秒级或带小数，UTC）时间戳。minSdk 26 有 java.time。 */
    fun parseIso(text: String?): Long? {
        if (text.isNullOrEmpty()) return null
        return try {
            java.time.Instant.parse(text).toEpochMilli()
        } catch (e: Exception) {
            try {
                java.time.OffsetDateTime.parse(text).toInstant().toEpochMilli()
            } catch (e2: Exception) {
                null
            }
        }
    }

    fun fmtAge(startedAtMs: Long?): String {
        if (startedAtMs == null || startedAtMs <= 0L) return "—"
        val s = ((System.currentTimeMillis() - startedAtMs) / 1000L).coerceAtLeast(0L)
        if (s < 60) return Tr.s("%1\$s 秒", s)
        val m = s / 60
        if (m < 60) return Tr.s("%1\$s 分", m)
        val h = m / 60
        if (h < 24) return Tr.s("%1\$s 小时", h)
        return Tr.s("%1\$s 天", h / 24)
    }

    /** 时间戳转「MM-dd HH:mm」，给镜像的拉取时间这类不带年份的场合。 */
    fun fmtStamp(ms: Long?): String {
        if (ms == null || ms <= 0L) return "—"
        return java.time.Instant.ofEpochMilli(ms)
            .atZone(java.time.ZoneId.systemDefault())
            .format(java.time.format.DateTimeFormatter.ofPattern("MM-dd HH:mm"))
    }

    fun phaseLabel(phase: String): String = when {
        phase.startsWith("Running") -> Tr.s("运行中")
        phase.startsWith("Succeeded") -> Tr.s("已完成")
        phase == "Pending" -> Tr.s("等待调度")
        phase == "Drained" -> Tr.s("已排空")
        phase.contains("CrashLoop") -> Tr.s("崩溃重启")
        phase.contains("ImagePull") -> Tr.s("拉取失败")
        phase == "Failed" -> Tr.s("失败")
        phase == "Unknown" -> Tr.s("未知")
        else -> phase
    }

    fun phaseColor(ctx: Context, phase: String): Int = when {
        phase.startsWith("Running") || phase.startsWith("Succeeded") -> ctx.getColor(R.color.ok)
        phase.contains("CrashLoop") || phase.contains("ImagePull") || phase == "Failed" -> ctx.getColor(R.color.bad)
        else -> ctx.getColor(R.color.lamp)
    }

    /** 色点永远配文字，不单独用颜色表达状态。 */
    fun tintDot(dot: View, color: Int) {
        dot.backgroundTintList = ColorStateList.valueOf(color)
    }

    fun stateColor(ctx: Context, phase: String): Int = phaseColor(ctx, phase)

    /** 细进度条：父容器 weightSum = 100，填充块按百分比设置权重。 */
    fun setMeter(fill: View, percent: Float, color: Int) {
        val pct = percent.coerceIn(0f, 100f)
        fill.backgroundTintList = ColorStateList.valueOf(color)
        val lp = fill.layoutParams
        lp.width = 0
        if (lp is android.widget.LinearLayout.LayoutParams) {
            lp.weight = pct
            fill.layoutParams = lp
        }
        fill.visibility = if (pct <= 0.5f) View.INVISIBLE else View.VISIBLE
    }

    fun label(ctx: Context, text: String, sizeSp: Float, color: Int): TextView = TextView(ctx).apply {
        this.text = text
        this.setTextSize(android.util.TypedValue.COMPLEX_UNIT_SP, sizeSp)
        this.setTextColor(color)
    }

    fun parseColorSafe(value: String?, fallback: Int): Int = try {
        if (value.isNullOrEmpty()) fallback else Color.parseColor(value)
    } catch (e: Exception) {
        fallback
    }
}
