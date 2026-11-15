package com.droidnode.ui.v2

import java.io.File

/**
 * 扫 /proc 统计本 uid 下的进程（agent 与全部 proot 容器共享这个 uid）。
 * 只读 /proc，不需要任何权限；取不到就返回空值，界面显示「—」。
 */
object ProcInfo {

    data class Sample(val rssBytes: Long, val processes: Int, val jiffies: Long)

    fun ourProcesses(): Sample {
        val myUid = android.os.Process.myUid()
        var rss = 0L
        var count = 0
        var jiffies = 0L
        try {
            val procDir = File("/proc")
            val entries = procDir.list() ?: return Sample(0L, 0, 0L)
            for (name in entries) {
                if (name.isEmpty() || name[0] !in '0'..'9') continue
                val base = File(procDir, name)
                val status = File(base, "status")
                if (!status.canRead()) continue
                val text = try {
                    status.readText()
                } catch (e: Exception) {
                    continue
                }
                val uidLine = Regex("(?m)^Uid:\\s+(\\d+)").find(text) ?: continue
                if (uidLine.groupValues[1].toIntOrNull() != myUid) continue
                val rssMatch = Regex("(?m)^VmRSS:\\s+(\\d+)").find(text)
                if (rssMatch != null) {
                    rss += (rssMatch.groupValues[1].toLongOrNull() ?: 0L) * 1024L
                    count++
                }
                val stat = File(base, "stat")
                if (stat.canRead()) {
                    try {
                        val s = stat.readText()
                        val close = s.lastIndexOf(')')
                        if (close > 0) {
                            val parts = s.substring(close + 1).trim().split(" ")
                            if (parts.size > 12) {
                                jiffies += (parts[11].toLongOrNull() ?: 0L) + (parts[12].toLongOrNull() ?: 0L)
                            }
                        }
                    } catch (e: Exception) {
                        // 进程可能刚好退出，忽略
                    }
                }
            }
        } catch (e: Exception) {
            return Sample(0L, 0, 0L)
        }
        return Sample(rss, count, jiffies)
    }
}
