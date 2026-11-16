# DroidNode Agent HTTP API

> 版本：`0.1.43`　基础地址：`https://<node-ip>:<DROIDNODE_KUBELET_PORT>`

Rust agent 在手机上暴露的 kubelet 兼容只读 API（默认 10255 端口，TLS）。控制面、kubectl 和本机 app 都通过它读取 Pod/镜像/日志/指标，exec/attach/port-forward 走 SPDY。

## 认证

TLS 客户端证书（由集群 client CA 签发）。/pods、/images、/logs、/metrics 等只读接口允许匿名访问（kubelet 只读端口的惯例）；/exec、/attach、/portForward 必须带客户端证书，否则 403。

## 端点一览

| 方法 | 路径 | 认证 | 传输 | 说明 |
|---|---|---|---|---|
| `GET` | `/healthz` | none | http | 存活检查 |
| `GET` | `/pods` | none | http | 列出本节点的 Pod |
| `GET` | `/images` | none | http | 列出已解包的镜像 |
| `POST` | `/images/delete` | none | http | 删除镜像 |
| `GET` | `/containerLogs/{namespace}/{pod_id}/{container_name}` | none | http | 读取容器日志 |
| `GET` | `/metrics` | none | http | Prometheus 指标 |
| `GET` | `/metrics/resource` | none | http | 节点资源指标 |
| `GET` | `/stats/summary` | none | http | 节点与 Pod 统计 |
| `POST` | `/drain` | none | http | 排空节点 |
| `POST` | `/undrain` | none | http | 恢复节点 |
| `POST` | `/maintenance` | none | http | 切换维护模式 |
| `GET` | `/exec/{namespace}/{pod}/{container}` | client-cert | spdy | 在容器中执行命令（SPDY） |
| `POST` | `/exec/{namespace}/{pod}/{container}` | client-cert | spdy | 在容器中执行命令（SPDY，POST） |
| `GET` | `/attach/{namespace}/{pod}/{container}` | client-cert | spdy | 附加到容器（SPDY） |
| `POST` | `/attach/{namespace}/{pod}/{container}` | client-cert | spdy | 附加到容器（SPDY，POST） |
| `GET` | `/portForward/{namespace}/{pod}` | client-cert | spdy | 端口转发（SPDY） |
| `POST` | `/portForward/{namespace}/{pod}` | client-cert | spdy | 端口转发（SPDY，POST） |

## 端点详情

### `GET /healthz`

存活检查

进程存活即返回 200，不做依赖检查。

- 认证：`none`
- 传输：`http`

| 状态码 | 说明 | 响应 |
|---|---|---|
| `200` | ok | text/plain `ok` |

```bash
curl -k "https://<node-ip>:<DROIDNODE_KUBELET_PORT>/healthz"
```

### `GET /pods`

列出本节点的 Pod

返回标准 v1.PodList；status.phase 为 Pending/Running/Failed 等，containerStatuses[].state.waiting.reason 会带 ErrImagePull/ImagePullBackOff/CrashLoopBackOff。

- 认证：`none`
- 传输：`http`

| 状态码 | 说明 | 响应 |
|---|---|---|
| `200` | PodList | application/json `{"kind": "PodList", "apiVersion": "v1", "metadata": {}, "items": [{"apiVersion": "v1", "kind": "Pod", "metadata": {"name": "chatapp-backend-abc", "namespace": "chatapp-vertx", "uid": "…"}, "spec": {"nodeName": "dn-k10"}, "status": {"phase": "Running", "podIP": "192.168.2.5", "containerStatuses": [{"name": "chatapp-backend", "ready": true, "started": true, "restartCount": 0, "state": {"running": {"startedAt": "2026-11-15T12:00:00Z"}}}]}}]}` |
| `500` | 内部错误 | application/json `{"error": "…"}` |

```bash
curl -k "https://<node-ip>:<DROIDNODE_KUBELET_PORT>/pods"
```

### `GET /images`

列出已解包的镜像

按 rootfs 目录去重后返回镜像、总大小和数量。name 形如 registry/repo:tag。

- 认证：`none`
- 传输：`http`

| 状态码 | 说明 | 响应 |
|---|---|---|
| `200` | 镜像列表 | application/json `{"images": [{"name": "192.168.2.7:5000/chatapp/backend:arm64", "layers": 3, "compressed": 172500000, "unpacked": 312300000, "digest": "sha256:…", "registry": "192.168.2.7:5000", "pulled_at": "2026-11-15T12:00:00Z", "rootfs": "/data/user/0/com.droidnode/files/rootfs/192.168.2.7:5000/chatapp/backend/arm64"}], "total_size": 312300000, "count": 1}` |

```bash
curl -k "https://<node-ip>:<DROIDNODE_KUBELET_PORT>/images"
```

### `POST /images/delete`

删除镜像

只删除 image rootfs 与对应 layer；被运行中 Pod 使用的镜像返回 409。

- 认证：`none`
- 传输：`http`

| 参数 | 位置 | 必填 | 类型 | 说明 |
|---|---|---|---|---|
| `id` | query | 是 | string | /images 返回的 rootfs 绝对路径（示例：`/data/user/0/com.droidnode/files/rootfs/192.168.2.7:5000/chatapp/backend/arm64`） |

| 状态码 | 说明 | 响应 |
|---|---|---|
| `200` | 已删除 | application/json `{"deleted": "…/arm64", "freed_bytes": 484800000}` |
| `409` | 镜像正在被 Pod 使用 | application/json `{"error": "image is in use: …/arm64"}` |
| `500` | 删除失败 | application/json `{"error": "…"}` |

```bash
curl -k -X POST "https://<node-ip>:<DROIDNODE_KUBELET_PORT>/images/delete?id=<id>"
```

### `GET /containerLogs/{namespace}/{pod_id}/{container_name}`

读取容器日志

返回 text/plain。按 tailLines 截断；follow=true 时以流式响应持续输出，并用 sinceSeconds/sinceTime 过滤。

- 认证：`none`
- 传输：`http`

| 参数 | 位置 | 必填 | 类型 | 说明 |
|---|---|---|---|---|
| `namespace` | path | 是 | string |  |
| `pod_id` | path | 是 | string |  |
| `container_name` | path | 是 | string |  |
| `follow` | query | 否 | boolean | 流式跟随，默认 false |
| `tailLines` | query | 否 | integer | 默认 200，最大 10000 |
| `sinceSeconds` | query | 否 | integer |  |
| `sinceTime` | query | 否 | string | RFC3339 |
| `timestamps` | query | 否 | boolean | 每行带时间戳 |
| `previous` | query | 否 | boolean | 读取上一个容器实例的日志 |
| `limitBytes` | query | 否 | integer | 接受但忽略（不截断行） |

| 状态码 | 说明 | 响应 |
|---|---|---|
| `200` | 日志文本 | text/plain `2026-11-15T12:00:00Z INFO started
` |
| `400` | previous 日志不存在 | text/plain `previous terminated container ".log not found
` |

```bash
curl -k "https://<node-ip>:<DROIDNODE_KUBELET_PORT>/containerLogs/<namespace>/<pod_id>/<container_name>?follow=<follow>&tailLines=<tailLines>&sinceSeconds=<sinceSeconds>&sinceTime=<sinceTime>&timestamps=<timestamps>&previous=<previous>&limitBytes=<limitBytes>"
```

### `GET /metrics`

Prometheus 指标

返回 text/plain，供 Prometheus 抓取。

- 认证：`none`
- 传输：`http`

| 状态码 | 说明 | 响应 |
|---|---|---|
| `200` | Prometheus exposition | text/plain `# HELP droidnode_agent_up 1
` |

```bash
curl -k "https://<node-ip>:<DROIDNODE_KUBELET_PORT>/metrics"
```

### `GET /metrics/resource`

节点资源指标

kubelet resource metrics 端点（text/plain）。

- 认证：`none`
- 传输：`http`

| 状态码 | 说明 | 响应 |
|---|---|---|
| `200` | resource metrics | text/plain `node_cpu_usage_seconds_total 1234
` |

```bash
curl -k "https://<node-ip>:<DROIDNODE_KUBELET_PORT>/metrics/resource"
```

### `GET /stats/summary`

节点与 Pod 统计

kubelet Summary API 的 JSON 子集：node 内存/文件系统、每个 Pod 的 CPU/内存。

- 认证：`none`
- 传输：`http`

| 状态码 | 说明 | 响应 |
|---|---|---|
| `200` | summary | application/json `{"node": {"nodeName": "dn-k10", "memory": {"availableBytes": 5600000000, "usageBytes": 5500000000}, "fs": {"availableBytes": 52000000000, "capacityBytes": 224900000000}}, "pods": [{"podRef": {"name": "chatapp-backend-abc", "namespace": "chatapp-vertx"}, "containers": [{"name": "chatapp-backend", "memory": {"rssBytes": 230000000}, "cpu": {"usageCoreNanoSeconds": 1000000000}}]}]}` |

```bash
curl -k "https://<node-ip>:<DROIDNODE_KUBELET_PORT>/stats/summary"
```

### `POST /drain`

排空节点

停止本节点上的 Pod（保持进程存活），用于低电量保护；幂等。

- 认证：`none`
- 传输：`http`

| 状态码 | 说明 | 响应 |
|---|---|---|
| `200` | 已排空 | application/json `{"drained": true}` |

```bash
curl -k -X POST "https://<node-ip>:<DROIDNODE_KUBELET_PORT>/drain"
```

### `POST /undrain`

恢复节点

取消排空。若操作员已开启维护模式，返回 409 且不解除排空。

- 认证：`none`
- 传输：`http`

| 状态码 | 说明 | 响应 |
|---|---|---|
| `200` | 已恢复 | application/json `{"drained": false, "maintenance": false}` |
| `409` | 维护模式生效中 | application/json `{"drained": true, "maintenance": true}` |

```bash
curl -k -X POST "https://<node-ip>:<DROIDNODE_KUBELET_PORT>/undrain"
```

### `POST /maintenance`

切换维护模式

on=true 时 cordon 并从控制面排空，on=false 时恢复调度。

- 认证：`none`
- 传输：`http`

| 参数 | 位置 | 必填 | 类型 | 说明 |
|---|---|---|---|---|
| `on` | query | 是 | boolean | （示例：`True`） |

| 状态码 | 说明 | 响应 |
|---|---|---|
| `200` | 切换结果 | application/json `{"maintenance": true, "drained": true}` |
| `500` | 控制面调用失败 | application/json `{"error": "…"}` |

```bash
curl -k -X POST "https://<node-ip>:<DROIDNODE_KUBELET_PORT>/maintenance?on=<on>"
```

### `GET /exec/{namespace}/{pod}/{container}`

在容器中执行命令（SPDY）

必须带客户端证书，并通过 Upgrade: SPDY/3.1 升级；kubectl exec 走这里。GET/POST 均可。

- 认证：`client-cert`
- 传输：`spdy`

| 参数 | 位置 | 必填 | 类型 | 说明 |
|---|---|---|---|---|
| `namespace` | path | 是 | string |  |
| `pod` | path | 是 | string |  |
| `container` | path | 是 | string |  |
| `command` | query | 是 | string | 可重复，多个 command 参数组成 argv |
| `stdin` | query | 否 | boolean |  |
| `stdout` | query | 否 | boolean |  |
| `stderr` | query | 否 | boolean |  |
| `tty` | query | 否 | boolean |  |

| 状态码 | 说明 | 响应 |
|---|---|---|
| `101` | SPDY 升级成功 | — — |
| `400` | 缺少 command 或不是 SPDY | text/plain — |
| `403` | 没有客户端证书 | text/plain `exec requires a client certificate signed by the cluster client CA` |
| `404` | 容器未运行 | text/plain `container not running` |
| `503` | SPDY sidecar 缺失 | text/plain — |

```bash
# GET /exec/{namespace}/{pod}/{container}（SPDY/3.1，kubectl 内部调用）
# 需要客户端证书，Upgrade: SPDY/3.1
```

### `POST /exec/{namespace}/{pod}/{container}`

在容器中执行命令（SPDY，POST）

与 GET 相同。

- 认证：`client-cert`
- 传输：`spdy`

| 参数 | 位置 | 必填 | 类型 | 说明 |
|---|---|---|---|---|
| `namespace` | path | 是 | string |  |
| `pod` | path | 是 | string |  |
| `container` | path | 是 | string |  |
| `command` | query | 是 | string | 可重复 |
| `stdin` | query | 否 | boolean |  |
| `stdout` | query | 否 | boolean |  |
| `stderr` | query | 否 | boolean |  |
| `tty` | query | 否 | boolean |  |

| 状态码 | 说明 | 响应 |
|---|---|---|
| `101` | SPDY 升级成功 | — — |
| `400` | 缺少 command 或不是 SPDY | text/plain — |
| `403` | 没有客户端证书 | text/plain — |
| `404` | 容器未运行 | text/plain — |
| `503` | SPDY sidecar 缺失 | text/plain — |

```bash
# POST /exec/{namespace}/{pod}/{container}（SPDY/3.1，kubectl 内部调用）
# 需要客户端证书，Upgrade: SPDY/3.1
```

### `GET /attach/{namespace}/{pod}/{container}`

附加到容器（SPDY）

必须带客户端证书并升级 SPDY/3.1；kubectl attach 走这里。

- 认证：`client-cert`
- 传输：`spdy`

| 参数 | 位置 | 必填 | 类型 | 说明 |
|---|---|---|---|---|
| `namespace` | path | 是 | string |  |
| `pod` | path | 是 | string |  |
| `container` | path | 是 | string |  |
| `stdin` | query | 否 | boolean |  |
| `stdout` | query | 否 | boolean |  |
| `stderr` | query | 否 | boolean |  |
| `tty` | query | 否 | boolean |  |

| 状态码 | 说明 | 响应 |
|---|---|---|
| `101` | SPDY 升级成功 | — — |
| `400` | 不是 SPDY | text/plain — |
| `403` | 没有客户端证书 | text/plain — |
| `404` | 容器未运行 | text/plain — |

```bash
# GET /attach/{namespace}/{pod}/{container}（SPDY/3.1，kubectl 内部调用）
# 需要客户端证书，Upgrade: SPDY/3.1
```

### `POST /attach/{namespace}/{pod}/{container}`

附加到容器（SPDY，POST）

与 GET 相同。

- 认证：`client-cert`
- 传输：`spdy`

| 参数 | 位置 | 必填 | 类型 | 说明 |
|---|---|---|---|---|
| `namespace` | path | 是 | string |  |
| `pod` | path | 是 | string |  |
| `container` | path | 是 | string |  |
| `stdin` | query | 否 | boolean |  |
| `stdout` | query | 否 | boolean |  |
| `stderr` | query | 否 | boolean |  |
| `tty` | query | 否 | boolean |  |

| 状态码 | 说明 | 响应 |
|---|---|---|
| `101` | SPDY 升级成功 | — — |
| `400` | 不是 SPDY | text/plain — |
| `403` | 没有客户端证书 | text/plain — |
| `404` | 容器未运行 | text/plain — |

```bash
# POST /attach/{namespace}/{pod}/{container}（SPDY/3.1，kubectl 内部调用）
# 需要客户端证书，Upgrade: SPDY/3.1
```

### `GET /portForward/{namespace}/{pod}`

端口转发（SPDY）

必须带客户端证书并升级 SPDY/3.1；请求头携带重复的 port，数据流协议为 portforward.k8s.io。

- 认证：`client-cert`
- 传输：`spdy`

| 参数 | 位置 | 必填 | 类型 | 说明 |
|---|---|---|---|---|
| `namespace` | path | 是 | string |  |
| `pod` | path | 是 | string |  |
| `port` | header | 是 | string | 可重复，转发到 127.0.0.1:<port> |
| `x-stream-protocol-version` | header | 否 | string | （示例：`portforward.k8s.io`） |

| 状态码 | 说明 | 响应 |
|---|---|---|
| `101` | SPDY 升级成功 | — — |
| `400` | 不是 SPDY | text/plain — |
| `403` | 没有客户端证书 | text/plain — |
| `503` | SPDY sidecar 缺失 | text/plain — |

```bash
# GET /portForward/{namespace}/{pod}（SPDY/3.1，kubectl 内部调用）
# 需要客户端证书，Upgrade: SPDY/3.1
```

### `POST /portForward/{namespace}/{pod}`

端口转发（SPDY，POST）

与 GET 相同。

- 认证：`client-cert`
- 传输：`spdy`

| 参数 | 位置 | 必填 | 类型 | 说明 |
|---|---|---|---|---|
| `namespace` | path | 是 | string |  |
| `pod` | path | 是 | string |  |
| `port` | header | 是 | string |  |

| 状态码 | 说明 | 响应 |
|---|---|---|
| `101` | SPDY 升级成功 | — — |
| `400` | 不是 SPDY | text/plain — |
| `403` | 没有客户端证书 | text/plain — |
| `503` | SPDY sidecar 缺失 | text/plain — |

```bash
# POST /portForward/{namespace}/{pod}（SPDY/3.1，kubectl 内部调用）
# 需要客户端证书，Upgrade: SPDY/3.1
```

## Agent status IPC（Unix socket）

app 与 agent 之间的状态通道：app 每 10 秒把设备遥测写成一行 JSON，agent 回一行 JSON 状态。不是 HTTP，不走网络。

- 路径：`$DROIDNODE_DATA_DIR/agent.sock（app 内为 files/agent.sock）`

### 请求（app → agent，一行 JSON）

| 字段 | 类型 | 说明 |
|---|---|---|
| `battery_percent` | u8? | 电量百分比 |
| `battery_charging` | bool? | 是否充电 |
| `battery_temperature_c` | f32? | 电池温度 |
| `network_type` | string? | wifi/cellular/… |
| `storage_total_bytes` | u64? | 存储总量 |
| `storage_available_bytes` | u64? | 可用存储 |
| `updated_at_ms` | u64 | 由 agent 覆盖，客户端传值会被忽略 |

### 响应（agent → app，一行 JSON）

| 字段 | 类型 | 说明 |
|---|---|---|
| `node_name` | string |  |
| `apiserver` | string | kubeconfig 中的 https://host:6443 |
| `ready` | bool | 未排空、网络可用、电量不低 |
| `pods_running` | usize |  |
| `restarts` | i32 | 所有容器 restartCount 之和 |
| `drained` | bool |  |
| `maintenance` | bool |  |
| `cordoned` | bool |  |
| `kubelet_port` | u16 |  |

---

本文档由 `scripts/export-api-docs.py` 从 `docs/api/routes.json` 生成；
改接口时先改 `routes.json`，再运行脚本。`--check` 会比对路由与代码。
