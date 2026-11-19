package com.droidnode.ui.v2

import android.app.Activity
import com.droidnode.ui.BaseActivity
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
 * 镜像页：磁盘被谁占了，删掉不再使用的。
 *
 * 删除走 agent 的 /images/delete，409 表示镜像正在被某台 Pod 使用 —— 直接把
 * 这一句翻成人话，不再把 JSON 原样弹出来。
 */
class ImagesActivity : BaseActivity() {

    override val isTabRoot: Boolean get() = true

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private lateinit var sheet: SheetHost

    private lateinit var imageList: LinearLayout
    private lateinit var imageEmpty: TextView
    private lateinit var imageCount: TextView
    private lateinit var imageTotal: TextView
    private lateinit var imageRegistry: TextView
    private lateinit var pillDot: View
    private lateinit var pillText: TextView

    private var images: List<ImageRow> = emptyList()
    private var agentOk = true
    private var pollJob: Job? = null
    private var observeJob: Job? = null
    private var deleting = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_v2_images)
        sheet = SheetHost(this)

        imageList = findViewById(R.id.imageList)
        imageEmpty = findViewById(R.id.imageEmpty)
        imageCount = findViewById(R.id.imageCount)
        imageTotal = findViewById(R.id.imageTotal)
        imageRegistry = findViewById(R.id.imageRegistry)
        pillDot = findViewById(R.id.pillDot)
        pillText = findViewById(R.id.pillText)

        BottomNav.wire(this, BottomNav.Screen.IMAGES)
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

    /** Render one repository snapshot; the shared loop owns the HTTP calls. */
    private suspend fun onSnapshot(snapshot: NodeRepository.State) {
        agentOk = snapshot.health
        if (snapshot.ticks > 0L) images = snapshot.images
        render()
    }

    private fun render() {
        StatusPill.render(this, pillDot, pillText, agentOk)

        // agent 的 /images 会把同一个 rootfs 目录重复列出（现在观测到同一镜像 4 条），
        // 按 rootfs 去重后再统计，否则数量和占用都会翻倍。
        val unique = images.distinctBy { it.rootfs }
        val total = unique.sumOf { it.compressed + it.unpacked }
        imageCount.text = Tr.s("%1\$s 个镜像", unique.size)
        imageTotal.text = Tr.s("共 %1\$s", Ui.humanBytes(total))
        imageRegistry.text = unique.firstOrNull()?.registry?.takeIf { it.isNotEmpty() } ?: "—"

        imageList.removeAllViews()
        if (unique.isEmpty()) {
            imageEmpty.visibility = View.VISIBLE
            return
        }
        imageEmpty.visibility = View.GONE
        for (image in unique.sortedBy { it.name }) {
            imageList.addView(
                Rows.image(this, imageList, image) {
                    Sheets.image(this, sheet, image) { confirmDelete(image) }
                },
            )
        }
    }

    private fun confirmDelete(image: ImageRow) {
        Sheets.confirm(
            this,
            sheet,
            Tr.s("删除镜像"),
            Tr.s("删除 %1\$s 会释放 %2\$s，下次部署需要重新拉取。", image.name, Ui.humanBytes(image.unpacked)),
            Tr.s("删除"),
        ) { delete(image) }
    }

    private fun delete(image: ImageRow) {
        if (deleting) return
        deleting = true
        scope.launch {
            val res = withContext(Dispatchers.IO) { Snapshot.deleteImage(image.rootfs) }
            deleting = false
            when {
                res == null -> Sheets.toast(this@ImagesActivity, Tr.s("agent 无响应，删除失败"))
                res.code == 409 -> Sheets.toast(this@ImagesActivity, Tr.s("这个镜像正被 Pod 使用，不能删除"))
                res.code in 200..299 -> {
                    Sheets.toast(this@ImagesActivity, Tr.s("已删除 %1\$s · 释放 %2\$s", image.name, Ui.humanBytes(image.unpacked)))
                    Repository.refresh()
                }
                else -> Sheets.toast(this@ImagesActivity, Tr.s("删除失败（HTTP %1\$s）", res.code))
            }
        }
    }
}
