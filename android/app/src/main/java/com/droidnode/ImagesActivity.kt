package com.droidnode

import android.app.Activity
import android.graphics.Color
import android.graphics.Typeface
import android.os.Bundle
import android.view.ViewGroup
import android.widget.Button
import com.droidnode.ui.BaseActivity
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import com.droidnode.ui.v2.AgentApi
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.net.URL
import java.net.URLEncoder
import java.security.SecureRandom
import java.security.cert.X509Certificate
import javax.net.ssl.HttpsURLConnection
import javax.net.ssl.SSLContext
import javax.net.ssl.TrustManager
import javax.net.ssl.X509TrustManager

/**
 * On-device image management.
 *
 * Talks to the agent's local API (loopback only):
 *   GET  /images              -> { images: [{name, manifest_digest, compressed_size,
 *                                            unpacked_size, layers, rootfs_dir, pulled_at}], total_size }
 *   POST /images/delete?id=.. -> removes the rootfs and GCs unreferenced layers
 */
class ImagesActivity : BaseActivity() {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private lateinit var summary: TextView
    private lateinit var list: LinearLayout

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Color.parseColor("#101418"))
            setPadding(40, 64, 40, 40)
        }

        val title = TextView(this).apply {
            text = "镜像"
            textSize = 24f
            setTextColor(Color.WHITE)
            setTypeface(typeface, Typeface.BOLD)
        }

        summary = TextView(this).apply {
            textSize = 13f
            setTextColor(Color.parseColor("#9AA0A6"))
            setPadding(0, 12, 0, 20)
        }

        list = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }

        val scroll = ScrollView(this).apply { addView(list) }

        val refresh = Button(this).apply {
            text = "刷新列表"
            setOnClickListener { load() }
        }

        root.addView(title)
        root.addView(summary)
        root.addView(scroll, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
        root.addView(refresh)

        setContentView(root)
        load()
    }

    // ── data ────────────────────────────────────────────────────────────────

    private fun load() {
        scope.launch {
            val body = withContext(Dispatchers.IO) { http("GET", "https://127.0.0.1:${AgentApi.port}/images") }
            if (body == null) {
                summary.text = "无法连接 agent（端口 ${AgentApi.port}）。确认节点正在运行。"
                return@launch
            }
            val arr = JSONObject(body).optJSONArray("images")
            val total = JSONObject(body).optLong("total_size", 0)
            summary.text = "${arr?.length() ?: 0} 个镜像 · 共 ${human(total)}"
            render(arr)
        }
    }

    private fun render(arr: org.json.JSONArray?) {
        list.removeAllViews()
        if (arr == null || arr.length() == 0) {
            list.addView(TextView(this).apply {
                text = "还没有镜像。部署一个 Pod 后镜像会出现在这里。"
                setTextColor(Color.parseColor("#9AA0A6"))
                textSize = 14f
            })
            return
        }
        for (i in 0 until arr.length()) {
            val o = arr.getJSONObject(i)
            list.addView(row(o))
        }
    }

    private fun row(o: JSONObject): LinearLayout {
        val name = o.optString("name", "?")
        val digest = o.optString("manifest_digest", "")
        val compressed = o.optLong("compressed_size", 0)
        val unpacked = o.optLong("unpacked_size", 0)
        val layers = o.optJSONArray("layers")?.length() ?: 0
        val rootfs = o.optString("rootfs_dir", "")

        val card = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(28, 24, 28, 24)
            setBackgroundColor(Color.parseColor("#1B2126"))
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
            ).apply { bottomMargin = 20 }
        }

        card.addView(TextView(this).apply {
            text = name
            textSize = 16f
            setTextColor(Color.WHITE)
            setTypeface(typeface, Typeface.BOLD)
        })

        card.addView(TextView(this).apply {
            text = if (digest.isEmpty()) "digest: —" else "digest: ${digest.take(19)}…"
            textSize = 12f
            setTextColor(Color.parseColor("#9AA0A6"))
            setPadding(0, 8, 0, 0)
        })

        card.addView(TextView(this).apply {
            text = "层 $layers · 压缩 ${human(compressed)} · 解包 ${human(unpacked)}"
            textSize = 12f
            setTextColor(Color.parseColor("#9AA0A6"))
            setPadding(0, 4, 0, 0)
        })

        val del = Button(this).apply {
            text = "删除"
            setOnClickListener {
                isEnabled = false
                delete(rootfs) { ok, msg ->
                    if (ok) {
                        Toast.makeText(this@ImagesActivity, "已删除 $name", Toast.LENGTH_SHORT).show()
                        load()
                    } else {
                        isEnabled = true
                        Toast.makeText(this@ImagesActivity, "删除失败: $msg", Toast.LENGTH_LONG).show()
                    }
                }
            }
        }
        card.addView(del)
        return card
    }

    private fun delete(rootfsDir: String, done: (Boolean, String) -> Unit) {
        scope.launch {
            val url = "https://127.0.0.1:${AgentApi.port}/images/delete?id=" +
                URLEncoder.encode(rootfsDir, "UTF-8")
            val res = withContext(Dispatchers.IO) { http("POST", url) }
            if (res == null) {
                done(false, "无响应")
            } else if (res.contains("\"freed_bytes\"")) {
                done(true, res)
            } else {
                done(false, res)
            }
        }
    }

    // ── helpers ─────────────────────────────────────────────────────────────

    private fun http(method: String, urlStr: String): String? = try {
        val ctx = SSLContext.getInstance("TLS")
        ctx.init(null, arrayOf<TrustManager>(object : X509TrustManager {
            override fun checkClientTrusted(chain: Array<X509Certificate>, authType: String) {}
            override fun checkServerTrusted(chain: Array<X509Certificate>, authType: String) {}
            override fun getAcceptedIssuers(): Array<X509Certificate> = arrayOf()
        }), SecureRandom())

        val conn = URL(urlStr).openConnection() as HttpsURLConnection
        conn.sslSocketFactory = ctx.socketFactory
        conn.setHostnameVerifier { _, _ -> true }
        conn.connectTimeout = 3000
        conn.readTimeout = 20000      // deletion walks the tree, allow time
        conn.requestMethod = method
        if (method == "POST") {
            conn.doOutput = true
            conn.outputStream.use { it.write(ByteArray(0)) }
        }
        val code = conn.responseCode
        val body = (if (code in 200..299) conn.inputStream else conn.errorStream)
            ?.bufferedReader()?.use { it.readText() }
        conn.disconnect()
        body
    } catch (e: Exception) {
        null
    }

    private fun human(bytes: Long): String {
        if (bytes <= 0) return "0 B"
        val units = arrayOf("B", "KB", "MB", "GB", "TB")
        var v = bytes.toDouble()
        var i = 0
        while (v >= 1024 && i < units.size - 1) {
            v /= 1024
            i++
        }
        return if (i == 0) "${bytes} B" else String.format("%.1f %s", v, units[i])
    }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
    }
}
