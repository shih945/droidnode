package com.droidnode.ui.v2

import android.app.Activity
import com.droidnode.ui.BaseActivity
import android.content.Intent
import android.os.Bundle
import android.view.View
import android.widget.LinearLayout
import android.widget.TextView
import com.droidnode.R
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.withContext

/**
 * Pod 页：这台机器上跑了什么，哪个在反复重启。
 * 过滤只在本地做，不额外请求；数据仍是 /pods + /stats/summary。
 */
class PodsActivity : BaseActivity() {

    override val isTabRoot: Boolean get() = true

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private lateinit var sheet: SheetHost

    private lateinit var podList: LinearLayout
    private lateinit var podEmpty: TextView
    private lateinit var chipAll: TextView
    private lateinit var chipRunning: TextView
    private lateinit var chipBad: TextView
    private lateinit var pillDot: View
    private lateinit var pillText: TextView

    private var pods: List<PodRow> = emptyList()
    private var filter = FILTER_ALL
    private var agentOk = true
    private var pollJob: Job? = null
    private var observeJob: Job? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_v2_pods)
        sheet = SheetHost(this)

        podList = findViewById(R.id.podList)
        podEmpty = findViewById(R.id.podEmpty)
        chipAll = findViewById(R.id.chipAll)
        chipRunning = findViewById(R.id.chipRunning)
        chipBad = findViewById(R.id.chipBad)
        pillDot = findViewById(R.id.pillDot)
        pillText = findViewById(R.id.pillText)

        BottomNav.wire(this, BottomNav.Screen.PODS)

        chipAll.setOnClickListener { select(FILTER_ALL) }
        chipRunning.setOnClickListener { select(FILTER_RUNNING) }
        chipBad.setOnClickListener { select(FILTER_BAD) }

        render()
    }

    override fun onStart() {
        super.onStart()
        Repository.acquire(scope)
        observeJob = scope.launch {
            Repository.state.collectLatest { onSnapshot(it) }
        }
    }

    override fun onStop() {
        observeJob?.cancel()
        observeJob = null
        pollJob?.cancel()
        pollJob = null
        Repository.release()
        super.onStop()
    }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
    }

    override fun onBackPressed() {
        if (sheet.isShowing) sheet.dismiss() else super.onBackPressed()
    }

    private fun select(next: String) {
        filter = next
        render()
    }

    /** Render one repository snapshot; the shared loop owns the HTTP calls. */
    private suspend fun onSnapshot(snapshot: NodeRepository.State) {
        agentOk = snapshot.health
        if (snapshot.ticks > 0L) pods = snapshot.pods
        render()
    }

    private fun render() {
        StatusPill.render(this, pillDot, pillText, agentOk, LocalConfig.drained(this))

        val running = pods.count { PodFilters.isRunning(it) }
        val bad = pods.count { PodFilters.isBad(it) }
        setChip(chipAll, Tr.s("全部 %1\$s", pods.size), filter == FILTER_ALL)
        setChip(chipRunning, Tr.s("运行中 %1\$s", running), filter == FILTER_RUNNING)
        setChip(chipBad, Tr.s("异常 %1\$s", bad), filter == FILTER_BAD)

        val visible = pods.filter {
            when (filter) {
                FILTER_RUNNING -> PodFilters.isRunning(it)
                FILTER_BAD -> PodFilters.isBad(it)
                else -> true
            }
        }.sortedWith(compareBy<PodRow> { PodFilters.severity(it) }.thenBy { it.name })

        podList.removeAllViews()
        if (visible.isEmpty()) {
            podEmpty.visibility = View.VISIBLE
            podEmpty.text = if (pods.isEmpty()) Tr.s("还没有 Pod 调度到本机") else Tr.s("这个筛选下没有 Pod")
            return
        }
        podEmpty.visibility = View.GONE
        for (pod in visible) {
            podList.addView(
                Rows.pod(this, podList, pod) {
                    Sheets.pod(this, sheet, pod) {
                        startActivity(
                            Intent(this, LogsActivity::class.java)
                                .putExtra(LogsActivity.EXTRA_POD, pod.name),
                        )
                    }
                },
            )
        }
    }

    private fun setChip(chip: TextView, text: String, active: Boolean) {
        chip.text = text
        chip.isActivated = active
        chip.setTextColor(getColor(if (active) R.color.text else R.color.text_2))
    }

    private companion object {
        const val FILTER_ALL = "all"
        const val FILTER_RUNNING = "running"
        const val FILTER_BAD = "bad"
    }
}
