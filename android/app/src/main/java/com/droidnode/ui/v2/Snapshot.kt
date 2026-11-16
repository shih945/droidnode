package com.droidnode.ui.v2

import com.droidnode.data.AgentApi
import org.json.JSONObject
import java.net.URLEncoder

data class PodRow(
    val name: String,
    val namespace: String,
    val container: String,
    val phase: String,
    val ready: Boolean,
    val restarts: Int,
    val startedAt: Long?,
    val podIp: String?,
    val message: String?,
    /** Container waiting reason (ErrImagePull/CrashLoopBackOff/ContainerCreating). */
    val waitingReason: String? = null,
    val memBytes: Long = 0L,
    val cpuNanos: Long = 0L,
)

data class ImageRow(
    val name: String,
    val layers: Int,
    val compressed: Long,
    val unpacked: Long,
    val digest: String,
    val registry: String,
    val pulledAt: String,
    val rootfs: String,
)

data class NodeStats(
    val memAvailable: Long,
    val memTotal: Long,
    val diskAvailable: Long,
    val diskTotal: Long,
    val cpuNanos: Long,
)

/**
 * 把 agent 的三份返回（/pods、/stats/summary、/images）整理成界面直接可用的行。
 * 任何一份失败都返回 null，由调用方决定显示「连不上」还是沿用上一帧。
 */
object Snapshot {

    fun pods(): List<PodRow>? {
        val root = AgentApi.json("/pods", 2000) ?: return null
        return parsePods(root)
    }

    /** Pure parser, kept separate so it can be tested without the network. */
    fun parsePods(root: JSONObject): List<PodRow> {
        val items = root.optJSONArray("items") ?: return emptyList()
        val out = ArrayList<PodRow>(items.length())
        for (i in 0 until items.length()) {
            val o = items.optJSONObject(i) ?: continue
            val meta = o.optJSONObject("metadata")
            val status = o.optJSONObject("status")
            val statuses = status?.optJSONArray("containerStatuses")
            val first = if (statuses != null && statuses.length() > 0) statuses.optJSONObject(0) else null
            val waiting = first?.optJSONObject("state")?.optJSONObject("waiting")
            val running = first?.optJSONObject("state")?.optJSONObject("running")
            val terminated = first?.optJSONObject("state")?.optJSONObject("terminated")
            // 运行中的取 startedAt；已结束的容器只有 terminated，用它算「最后一次活动」。
            val startedAt = Ui.parseIso(running?.optString("startedAt"))
                ?: Ui.parseIso(terminated?.optString("startedAt"))
            out.add(
                PodRow(
                    name = meta?.optString("name") ?: "?",
                    namespace = meta?.optString("namespace") ?: "default",
                    container = first?.optString("name") ?: "—",
                    phase = status?.optString("phase") ?: "Unknown",
                    ready = first?.optBoolean("ready", false) ?: false,
                    restarts = first?.optInt("restartCount", 0) ?: 0,
                    startedAt = startedAt,
                    podIp = status?.optString("podIP")?.takeIf { it.isNotEmpty() && it != "null" },
                    message = status?.optString("message")?.takeIf { it.isNotEmpty() && it != "null" },
                    waitingReason = waiting?.optString("reason")?.takeIf { it.isNotEmpty() && it != "null" },
                ),
            )
        }
        return out
    }

    fun stats(): NodeStats? {
        val root = AgentApi.json("/stats/summary", 3000) ?: return null
        val node = root.optJSONObject("node") ?: return null
        val machine = node.optJSONObject("machine")
        val memory = machine?.optJSONObject("memory")
        val avail = memory?.optLong("availableBytes", 0L) ?: 0L
        val used = memory?.optLong("usageBytes", 0L) ?: 0L
        val fs = node.optJSONObject("fs")
        return NodeStats(
            memAvailable = avail,
            memTotal = avail + used,
            diskAvailable = fs?.optLong("availableBytes", 0L) ?: 0L,
            diskTotal = fs?.optLong("capacityBytes", 0L) ?: 0L,
            cpuNanos = machine?.optJSONObject("cpu")?.optLong("usageCoreNanoSeconds", 0L) ?: 0L,
        )
    }

    /** 逐 Pod 的内存 / CPU：/stats/summary 的 pods[] 按 podRef.name 对回 /pods。 */
    fun podsWithStats(): List<PodRow>? {
        val pods = pods() ?: return null
        val root = AgentApi.json("/stats/summary", 3000)
        val usage = HashMap<String, Pair<Long, Long>>()
        val arr = root?.optJSONArray("pods")
        if (arr != null) {
            for (i in 0 until arr.length()) {
                val o = arr.optJSONObject(i) ?: continue
                val name = o.optJSONObject("podRef")?.optString("name") ?: continue
                val c = o.optJSONArray("containers")?.optJSONObject(0)
                val mem = c?.optJSONObject("memory")?.optLong("rssBytes", 0L) ?: 0L
                val cpu = c?.optJSONObject("cpu")?.optLong("usageCoreNanoSeconds", 0L) ?: 0L
                usage[name] = mem to cpu
            }
        }
        return pods.map { p ->
            val u = usage[p.name]
            if (u == null) p else p.copy(memBytes = u.first, cpuNanos = u.second)
        }
    }

    fun images(): List<ImageRow>? {
        val root = AgentApi.json("/droidnode/images", 5000) ?: return null
        val arr = root.optJSONArray("images") ?: return emptyList()
        val out = ArrayList<ImageRow>(arr.length())
        for (i in 0 until arr.length()) {
            val o = arr.optJSONObject(i) ?: continue
            out.add(
                ImageRow(
                    name = o.optString("name"),
                    layers = o.optJSONArray("layers")?.length() ?: 0,
                    compressed = o.optLong("compressed_size", 0L),
                    unpacked = o.optLong("unpacked_size", 0L),
                    digest = o.optString("manifest_digest"),
                    registry = o.optString("registry"),
                    pulledAt = Ui.parseIso(o.optString("pulled_at"))?.let { Ui.fmtStamp(it) }
                        ?: o.optString("pulled_at"),
                    rootfs = o.optString("rootfs_dir"),
                ),
            )
        }
        return out
    }

    fun health(): Boolean = AgentApi.text("/healthz", 1500)?.trim() == "ok"

    fun drain(on: Boolean): Boolean {
        val res = AgentApi.post(if (on) "/drain" else "/undrain", 4000) ?: return false
        return res.code in 200..299
    }

    fun deleteImage(rootfs: String): AgentApi.Response? {
        val id = URLEncoder.encode(rootfs, "UTF-8").replace("+", "%20")
        return AgentApi.post("/images/delete?id=$id", 20000)
    }
}
