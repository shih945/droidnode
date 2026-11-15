package com.droidnode.ui.v2

import android.app.Activity
import android.view.View
import android.widget.LinearLayout
import android.widget.TextView
import com.droidnode.R

/** 列表行的装配，节点页和 Pod / 镜像页共用同一套行。 */
object Rows {

    fun pod(activity: Activity, parent: LinearLayout, pod: PodRow, onClick: () -> Unit): View {
        val row = activity.layoutInflater.inflate(R.layout.item_pod, parent, false)
        Ui.tintDot(row.findViewById(R.id.podDot), Ui.phaseColor(activity, pod.phase))
        row.findViewById<TextView>(R.id.podName).text = pod.name
        row.findViewById<TextView>(R.id.podSub).text = "${pod.namespace} · ${pod.container}"
        row.findViewById<TextView>(R.id.podState).text = Ui.phaseLabel(pod.phase)
        row.findViewById<TextView>(R.id.podMeta).text =
            if (pod.restarts > 0) {
                Tr.s("重启 %1\$s 次 · %2\$s", pod.restarts, Ui.fmtAge(pod.startedAt))
            } else {
                Ui.fmtAge(pod.startedAt)
            }
        row.setOnClickListener { onClick() }
        return row
    }

    fun image(activity: Activity, parent: LinearLayout, image: ImageRow, onClick: () -> Unit): View {
        val row = activity.layoutInflater.inflate(R.layout.item_image, parent, false)
        row.findViewById<TextView>(R.id.imageName).text = image.name
        row.findViewById<TextView>(R.id.imageSub).text =
            "${Ui.humanBytes(image.compressed)} → ${Ui.humanBytes(image.unpacked)}"
        row.findViewById<TextView>(R.id.imageSide).text = Tr.s("%1\$s 层", image.layers)
        row.setOnClickListener { onClick() }
        return row
    }
}
