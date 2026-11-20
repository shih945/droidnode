package com.droidnode.ui.v2

import android.app.Activity
import android.graphics.Typeface
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast
import com.droidnode.R

/**
 * 三种弹层的内容装配：Pod 详情、镜像详情、确认、导入 kubeconfig。
 * 只做「把数据摆进布局」，取数与写文件都留在 Activity 里。
 */
object Sheets {

    fun pod(activity: Activity, host: SheetHost, pod: PodRow, onLogs: () -> Unit) {
        val content = activity.layoutInflater.inflate(R.layout.sheet_pod, null) as LinearLayout
        content.findViewById<TextView>(R.id.sheetTitle).text = Tr.s("Pod 详情")
        val kv = content.findViewById<LinearLayout>(R.id.sheetKv)
        kv.removeAllViews()

        kvRow(activity, kv, Tr.s("名称"), pod.name)
        kvRow(activity, kv, Tr.s("命名空间"), pod.namespace)
        kvRow(activity, kv, Tr.s("状态"), Ui.phaseLabel(pod.phase))
        kvRow(activity, kv, Tr.s("容器"), pod.container)
        kvRow(activity, kv, Tr.s("重启次数"), Tr.s("%1\$s 次", pod.restarts))
        kvRow(activity, kv, Tr.s("启动"), if (pod.startedAt != null) Ui.fmtAge(pod.startedAt) + Tr.s("前") else "—")
        kvRow(activity, kv, "Pod IP", pod.podIp ?: "—")
        kvRow(activity, kv, Tr.s("内存"), if (pod.memBytes > 0) Ui.humanBytes(pod.memBytes) else "—")
        if (!pod.message.isNullOrBlank()) {
            kvRow(activity, kv, Tr.s("最近事件"), pod.message)
        }

        content.findViewById<View>(R.id.sheetClose).setOnClickListener { host.dismiss() }
        content.findViewById<View>(R.id.sheetAction).setOnClickListener {
            host.dismiss()
            onLogs()
        }
        host.show(content, 0.78f)
    }

    fun image(activity: Activity, host: SheetHost, image: ImageRow, onDelete: () -> Unit) {
        val content = activity.layoutInflater.inflate(R.layout.sheet_image, null) as LinearLayout
        val kv = content.findViewById<LinearLayout>(R.id.sheetKv)
        kv.removeAllViews()

        kvRow(activity, kv, Tr.s("镜像"), image.name)
        kvRow(activity, kv, "digest", image.digest)
        kvRow(activity, kv, Tr.s("层数"), Tr.s("%1\$s 层", image.layers))
        kvRow(activity, kv, Tr.s("压缩"), Ui.humanBytes(image.compressed))
        kvRow(activity, kv, Tr.s("解包"), Ui.humanBytes(image.unpacked))
        kvRow(activity, kv, Tr.s("镜像源"), image.registry)
        kvRow(activity, kv, Tr.s("拉取时间"), image.pulledAt)
        kvRow(activity, kv, "rootfs", image.rootfs)

        content.findViewById<TextView>(R.id.sheetHint).text =
            Tr.s("删除会释放 %1\$s；被 Pod 使用时 agent 会拒绝", Ui.humanBytes(image.unpacked))
        content.findViewById<View>(R.id.sheetClose).setOnClickListener { host.dismiss() }
        content.findViewById<View>(R.id.sheetDelete).setOnClickListener {
            host.dismiss()
            onDelete()
        }
        host.show(content, 0.78f)
    }

    fun confirm(
        activity: Activity,
        host: SheetHost,
        title: String,
        message: String,
        confirmLabel: String,
        onConfirm: () -> Unit,
    ) {
        val content = activity.layoutInflater.inflate(R.layout.sheet_confirm, null) as LinearLayout
        content.findViewById<TextView>(R.id.confirmTitle).text = title
        content.findViewById<TextView>(R.id.confirmMessage).text = message
        content.findViewById<TextView>(R.id.confirmOk).text = confirmLabel
        content.findViewById<View>(R.id.confirmCancel).setOnClickListener { host.dismiss() }
        content.findViewById<View>(R.id.confirmOk).setOnClickListener {
            host.dismiss()
            onConfirm()
        }
        host.show(content, null)
    }

    fun importKubeconfig(
        activity: Activity,
        host: SheetHost,
        initial: String,
        onPickFile: () -> Unit,
        onSubmit: (String) -> Unit,
    ) {
        val content = activity.layoutInflater.inflate(R.layout.sheet_import, null) as LinearLayout
        val edit = content.findViewById<EditText>(R.id.importText)
        val hint = content.findViewById<TextView>(R.id.importHint)
        if (initial.isNotBlank()) edit.setText(initial)

        content.findViewById<View>(R.id.importClose).setOnClickListener { host.dismiss() }
        content.findViewById<View>(R.id.importCancel).setOnClickListener { host.dismiss() }
        content.findViewById<View>(R.id.importPick).setOnClickListener {
            host.dismiss()
            onPickFile()
        }
        content.findViewById<View>(R.id.importOk).setOnClickListener {
            val text = edit.text?.toString().orEmpty().trim()
            if (text.isEmpty()) {
                hint.setTextColor(activity.getColor(R.color.bad))
                hint.text = Tr.s("请先粘贴 kubeconfig 全文")
                return@setOnClickListener
            }
            if (LocalConfig.serverFrom(text) == null) {
                hint.setTextColor(activity.getColor(R.color.bad))
                hint.text = Tr.s("没找到 server: 行，确认是完整的 kubeconfig")
                return@setOnClickListener
            }
            host.dismiss()
            onSubmit(text)
        }
        host.show(content, null)
    }

    /**
     * 「用 token 加入集群」：只填地址 + token，CA 留空则由调用方从 `/cacerts` 取。
     * 校验只做「非空」，地址规范化在调用方（LocalConfig.normalizeServer）。
     */
    fun joinCluster(
        activity: Activity,
        host: SheetHost,
        onSubmit: (server: String, token: String, caPem: String) -> Unit,
    ) {
        val content = activity.layoutInflater.inflate(R.layout.sheet_join, null) as LinearLayout
        val server = content.findViewById<EditText>(R.id.joinServer)
        val token = content.findViewById<EditText>(R.id.joinToken)
        val ca = content.findViewById<EditText>(R.id.joinCa)
        val hint = content.findViewById<TextView>(R.id.joinHint)

        fun fail(message: String) {
            hint.setTextColor(activity.getColor(R.color.bad))
            hint.text = message
        }

        content.findViewById<View>(R.id.joinClose).setOnClickListener { host.dismiss() }
        content.findViewById<View>(R.id.joinCancel).setOnClickListener { host.dismiss() }
        content.findViewById<View>(R.id.joinOk).setOnClickListener {
            val s = server.text?.toString().orEmpty().trim()
            val t = token.text?.toString().orEmpty().trim()
            val c = ca.text?.toString().orEmpty().trim()
            if (s.isEmpty()) {
                fail(Tr.s("填控制面地址，例如 https://192.0.2.10:6443"))
                return@setOnClickListener
            }
            if (t.isEmpty()) {
                fail(Tr.s("填 token：kubectl -n kube-system create token droidnode-agent --duration=8760h"))
                return@setOnClickListener
            }
            host.dismiss()
            onSubmit(s, t, c)
        }
        host.show(content, null)
    }

    fun toast(activity: Activity, message: String) {
        Toast.makeText(activity, message, Toast.LENGTH_SHORT).show()
    }

    private fun kvRow(activity: Activity, parent: LinearLayout, label: String, value: String) {
        val row = LinearLayout(activity).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.TOP
            setPadding(0, Ui.dp(activity, 8f), 0, Ui.dp(activity, 8f))
        }
        val labelView = TextView(activity).apply {
            text = label
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
            setTextColor(activity.getColor(R.color.text_3))
            layoutParams = LinearLayout.LayoutParams(Ui.dp(activity, 72f), ViewGroup.LayoutParams.WRAP_CONTENT)
        }
        val valueView = TextView(activity).apply {
            text = value
            typeface = Typeface.MONOSPACE
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 13f)
            setTextColor(activity.getColor(R.color.text))
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
        }
        row.addView(labelView)
        row.addView(valueView)
        parent.addView(row)
    }
}
