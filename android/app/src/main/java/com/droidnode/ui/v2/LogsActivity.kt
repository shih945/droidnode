package com.droidnode.ui.v2

import android.app.Activity
import com.droidnode.ui.BaseActivity
import android.os.Bundle
import android.text.Editable
import android.text.TextWatcher
import android.view.View
import android.view.ViewGroup
import android.widget.BaseAdapter
import android.widget.EditText
import android.widget.ImageView
import android.widget.ListView
import android.widget.TextView
import com.droidnode.LogBuffer
import com.droidnode.LogLine
import com.droidnode.R
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * 日志页：级别、关键字、Pod 三种过滤，暂停后不丢行，恢复时接着看。
 *
 * 数据仍来自 LogBuffer（agent stdout 的 500 行环形缓冲）。按 Pod 过滤目前是在
 * 日志文本里匹配 pod 名；逐容器日志要走 /containerLogs 流式接口，留到下一步。
 */
class LogsActivity : BaseActivity() {

    override val isTabRoot: Boolean get() = true

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val timeFmt = SimpleDateFormat("HH:mm:ss", Locale.US)
    private val adapter = LogAdapter()

    private lateinit var logList: ListView
    private lateinit var logEmpty: TextView
    private lateinit var searchInput: EditText
    private lateinit var btnPause: ImageView
    private lateinit var resumeBar: TextView
    private lateinit var chipAll: TextView
    private lateinit var chipInfo: TextView
    private lateinit var chipWarn: TextView
    private lateinit var chipError: TextView
    private lateinit var chipPod: TextView
    private lateinit var chipClear: TextView
    private lateinit var pillDot: View
    private lateinit var pillText: TextView

    private var all: List<LogLine> = emptyList()
    private var level = "all"
    private var query = ""
    private var podFilter: String? = null
    private var paused = false
    private var pausedAtMs = 0L
    private var clearedAtMs = 0L
    private var newCount = 0
    private var collectJob: Job? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_v2_logs)

        logList = findViewById(R.id.logList)
        logEmpty = findViewById(R.id.logEmpty)
        searchInput = findViewById(R.id.searchInput)
        btnPause = findViewById(R.id.btnPause)
        resumeBar = findViewById(R.id.resumeBar)
        chipAll = findViewById(R.id.chipAll)
        chipInfo = findViewById(R.id.chipInfo)
        chipWarn = findViewById(R.id.chipWarn)
        chipError = findViewById(R.id.chipError)
        chipPod = findViewById(R.id.chipPod)
        chipClear = findViewById(R.id.chipClear)
        pillDot = findViewById(R.id.pillDot)
        pillText = findViewById(R.id.pillText)

        logList.adapter = adapter
        BottomNav.wire(this, BottomNav.Screen.LOGS)

        podFilter = intent.getStringExtra(EXTRA_POD)?.takeIf { it.isNotBlank() }
        podFilter?.let {
            chipPod.visibility = View.VISIBLE
            chipPod.text = "$it ×"
            chipPod.setOnClickListener {
                podFilter = null
                chipPod.visibility = View.GONE
                applyFilter()
            }
        }

        chipAll.setOnClickListener { selectLevel("all") }
        chipInfo.setOnClickListener { selectLevel("INFO") }
        chipWarn.setOnClickListener { selectLevel("WARN") }
        chipError.setOnClickListener { selectLevel("ERROR") }
        chipClear.setOnClickListener {
            clearedAtMs = System.currentTimeMillis()
            applyFilter()
        }

        btnPause.setOnClickListener { togglePause() }
        resumeBar.setOnClickListener { if (paused) togglePause() }

        searchInput.addTextChangedListener(object : TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) = Unit
            override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) = Unit
            override fun afterTextChanged(s: Editable?) {
                query = s?.toString().orEmpty()
                applyFilter()
            }
        })

        selectLevel("all")
    }

    override fun onStart() {
        super.onStart()
        StatusPill.render(this, pillDot, pillText, true)
        collectJob?.cancel()
        collectJob = scope.launch {
            LogBuffer.flow.collect { lines -> onLogs(lines) }
        }
    }

    override fun onStop() {
        collectJob?.cancel()
        collectJob = null
        super.onStop()
    }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
    }

    private fun onLogs(lines: List<LogLine>) {
        all = lines
        if (paused) {
            newCount = all.count { it.timeMs > pausedAtMs }
            renderResume()
        } else {
            applyFilter()
        }
    }

    private fun selectLevel(next: String) {
        level = next
        chipAll.isActivated = next == "all"
        chipInfo.isActivated = next == "INFO"
        chipWarn.isActivated = next == "WARN"
        chipError.isActivated = next == "ERROR"
        for (chip in listOf(chipAll, chipInfo, chipWarn, chipError)) {
            chip.setTextColor(getColor(if (chip.isActivated) R.color.text else R.color.text_2))
        }
        applyFilter()
    }

    private fun applyFilter() {
        val base = all.filter { it.timeMs >= clearedAtMs }
        val filtered = base.filter { matches(it) }
        adapter.setData(filtered)

        if (filtered.isEmpty()) {
            logEmpty.visibility = View.VISIBLE
            logEmpty.text = when {
                all.isEmpty() -> Tr.s("还没有日志")
                base.isEmpty() -> Tr.s("日志已清空，新的日志会继续追加")
                else -> Tr.s("没有匹配的日志")
            }
        } else {
            logEmpty.visibility = View.GONE
        }
        if (!paused) scrollToBottom()
    }

    private fun matches(line: LogLine): Boolean {
        val text = line.text
        if (level != "all" && levelOf(text) != level) return false
        val pod = podFilter
        if (pod != null && !text.contains(pod)) return false
        if (query.isNotEmpty() && !text.contains(query, ignoreCase = true)) return false
        return true
    }

    private fun togglePause() {
        paused = !paused
        btnPause.setImageResource(if (paused) R.drawable.ic_play else R.drawable.ic_pause)
        btnPause.contentDescription = if (paused) Tr.s("继续") else Tr.s("暂停")
        if (paused) {
            pausedAtMs = System.currentTimeMillis()
            newCount = 0
            renderResume()
        } else {
            resumeBar.visibility = View.GONE
            applyFilter()
            scrollToBottom()
        }
    }

    private fun renderResume() {
        if (paused && newCount > 0) {
            resumeBar.visibility = View.VISIBLE
            resumeBar.text = Tr.s("继续 · %1\$s 条新日志", newCount)
        } else {
            resumeBar.visibility = View.GONE
        }
    }

    private fun scrollToBottom() {
        logList.post { if (adapter.count > 0) logList.setSelection(adapter.count - 1) }
    }

    private fun levelOf(text: String): String = when {
        text.contains("ERROR", ignoreCase = true) || text.contains("FAILED", ignoreCase = true) -> "ERROR"
        text.contains("WARN", ignoreCase = true) -> "WARN"
        else -> "INFO"
    }

    /** agent 每行自带 `时间 级别`，左边已经有一列了，显示时去掉，避免同一行读四遍。 */
    private fun stripAgentPrefix(text: String): String = text.replaceFirst(AGENT_PREFIX, "")

    private val AGENT_PREFIX = Regex("""^\d{4}-\d{2}-\d{2}T[\d:.]+Z\s+(INFO|WARN|ERROR|DEBUG|TRACE)\s+""")

    private fun levelColor(level: String): Int = when (level) {
        "ERROR" -> getColor(R.color.bad)
        "WARN" -> getColor(R.color.lamp)
        else -> getColor(R.color.text_2)
    }

    private inner class LogAdapter : BaseAdapter() {

        private val data = ArrayList<LogLine>()

        fun setData(items: List<LogLine>) {
            data.clear()
            data.addAll(items.takeLast(500))
            notifyDataSetChanged()
        }

        override fun getCount(): Int = data.size

        override fun getItem(position: Int): LogLine = data[position]

        override fun getItemId(position: Int): Long = position.toLong()

        override fun getView(position: Int, convertView: View?, parent: ViewGroup): View {
            val view = convertView ?: layoutInflater.inflate(R.layout.item_log, parent, false)
            val line = data[position]
            val level = levelOf(line.text)
            view.findViewById<TextView>(R.id.logTime).text = timeFmt.format(Date(line.timeMs))
            view.findViewById<TextView>(R.id.logLevel).apply {
                text = level
                setTextColor(levelColor(level))
            }
            view.findViewById<TextView>(R.id.logMsg).text = stripAgentPrefix(line.text)
            return view
        }
    }

    companion object {
        const val EXTRA_POD = "pod"
    }
}
