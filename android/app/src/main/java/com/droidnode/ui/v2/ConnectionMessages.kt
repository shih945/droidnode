package com.droidnode.ui.v2

import com.droidnode.data.KubeConnectionTest

/**
 * 把连接测试结果翻成给人看的一句话。设置页和加入页共用，避免两处文案漂移。
 * 401 与 403 的含义完全不同；401 还要看请求到底带了什么凭据。
 */
object ConnectionMessages {

    fun text(result: KubeConnectionTest.Result): Pair<Boolean, String> = when (result) {
        is KubeConnectionTest.Result.Ok -> true to
            Tr.s("连接成功 · Kubernetes %1\$s · %2\$s ms · 身份：%3\$s", result.version, result.millis, authLabel(result.auth))
        is KubeConnectionTest.Result.AuthFailed -> false to authFailureText(result)
        is KubeConnectionTest.Result.HttpFailed -> false to
            Tr.s("控制面返回 HTTP %1\$s，检查 API 地址与证书", result.code)
        is KubeConnectionTest.Result.TlsFailed -> false to
            Tr.s("TLS 校验失败：%1\$s", result.message)
        is KubeConnectionTest.Result.Unreachable -> false to run {
            val server = result.server ?: Tr.s("控制面")
            Tr.s("连不上 %1\$s：%2\$s", server, result.message)
        }
        is KubeConnectionTest.Result.BadConfig -> false to result.message
    }

    fun authLabel(auth: KubeConnectionTest.Auth): String = when (auth) {
        KubeConnectionTest.Auth.TOKEN -> "token"
        KubeConnectionTest.Auth.CLIENT_CERT -> Tr.s("客户端证书")
        KubeConnectionTest.Auth.NONE -> Tr.s("匿名（无凭据）")
    }

    private fun authFailureText(r: KubeConnectionTest.Result.AuthFailed): String = when {
        r.code == 401 && r.auth == KubeConnectionTest.Auth.NONE ->
            Tr.s("能连上控制面，但请求没有可用凭据（HTTP 401）。") +
                Tr.s("常见原因：导入了控制面的 /etc/rancher/k3s/k3s.yaml（管理员证书），而证书没被读出来。") +
                Tr.s("请改用 token —— 在控制面跑 kubectl -n kube-system create token droidnode-agent --duration=8760h。")
        r.code == 401 && r.auth == KubeConnectionTest.Auth.TOKEN ->
            Tr.s("token 被拒绝（HTTP 401）—— 已过期，或不属于这个集群。") +
                Tr.s("在控制面重新生成：kubectl -n kube-system create token droidnode-agent --duration=8760h")
        r.code == 401 ->
            Tr.s("客户端证书被拒（HTTP 401）—— 证书过期或不属于这个集群；换 token 更快。")
        else ->
            Tr.s("凭据有效但权限不足（HTTP 403）—— 确认 deploy/rbac.yaml 已应用，") +
                Tr.s("且 token 属于 kube-system/droidnode-agent。")
    }
}
