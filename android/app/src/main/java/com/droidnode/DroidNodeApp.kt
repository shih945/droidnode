package com.droidnode

import android.app.Application
import com.droidnode.data.AgentApi
import com.droidnode.data.NodeStorage

/**
 * 进程级初始化：把用户配置的 kubelet 端口交给所有 UI 客户端。
 *
 * 以前 10255 散落在 AgentApi / ImagesActivity / NodeReadinessService 里；
 * E14 允许改端口后，必须有一个进程启动时统一读取的地方，否则界面会去敲旧端口。
 */
class DroidNodeApp : Application() {

    override fun onCreate() {
        super.onCreate()
        NodeStorage.ensure(this)
        AgentApi.configure(this)
    }
}
