# Kubelet / Kubernetes API 兼容性（v1.29）

当前控制面 **k3s v1.29.15+k3s1（go1.23.6）**，节点 `dn-k10`，声明
`kubeletVersion=v1.29.0`。只保证 kubelet v1.29 的预期行为和 API 兼容；
APK 专用字段/端点单独标注，不算不兼容。

- 契约：[`kubelet-contract-v1.29.json`](kubelet-contract-v1.29.json)
- 检查脚本：[`scripts/kubelet-compat.py`](../../scripts/kubelet-compat.py)
- 版本化契约规则：[`contracts/README.md`](contracts/README.md)

## 1. 当前结果

| 项目 | 结果 |
|---|---|
| L0 契约 | 缺少必填字段 **0** / 类型不符 **0** / 未识别字段 **0**；`--fail-on-missing` 通过 |
| Node 注册、心跳、状态 | ✅ `dn-k10` Ready，capacity/conditions/addresses 正常 |
| Pod 调度与启动 | ✅ |
| `kubectl logs` / `exec` / `attach` / `port-forward` | ✅ |
| apiserver proxy `/pods`、`/stats/summary` | ✅ |
| 远程直连 `/pods`、`/images`、`/stats` | ❌ 403，只允许 apiserver（客户端证书）或 loopback |
| `/healthz`、`/metrics`、`/metrics/resource` | ✅ 匿名 |
| `/images` | ✅ 标准 `ImageList`；APK 走 `/droidnode/images` |

## 2. 检查方法

```powershell
python scripts/kubelet-compat.py capture-kubectl `
  --kubeconfig D:\doc\k3s-1.29.yaml --node dn-k10 `
  --out build-cache/kubelet-compat/target

python scripts/kubelet-compat.py check --target build-cache/kubelet-compat/target `
  --k8s-version 1.29 --fail-on-missing
```

不要用 `capture-adb` 从局域网直接抓 `/pods`：非 loopback 需要客户端证书，会得到 403。
用 apiserver proxy，或 `adb forward` 后访问 `127.0.0.1:10255`。

## 3. 接口字段

### `/pods`

- `metadata.name/namespace/uid`
- `spec.containers[]`（完整 Pod spec）
- `status.phase/hostIP/podIP/startTime/qosClass`
- `status.conditions[]`（PodScheduled/Initialized/ContainersReady/Ready）
- `status.containerStatuses[].name/ready/restartCount/image/imageID/state`

`imageID` 为 `proot://<image>`（proot 没有运行时容器 ID）；`containerID` 未填。

### `/images`

- `/images`：kubelet `ImageList`，`images[].names[]/sizeBytes/id`
- `/droidnode/images`：APK 扩展，包含
  `name/registry/repository/reference/layers[]/compressed_size/unpacked_size/`
  `manifest_digest/config_digest/pulled_at/rootfs_dir`、`total_size`、`count`
- 删除镜像：`POST /images/delete?id=<rootfs_dir>`

### `/stats/summary`

- `node.nodeName/startTime/cpu.usageCoreNanoSeconds/memory.workingSetBytes/fs.*`
- `pods[].podRef.*/containers[].cpu/memory`
- `node.machine` 为额外便利字段；`systemContainers`、network/runtime 仍是占位值。

## 4. APK 扩展端点

| 端点 | 用途 |
|---|---|
| `POST /drain` | 低电量：停止本节点 Pod，保留 agent |
| `POST /undrain` | 恢复调度 |
| `POST /maintenance?on=` | 维护模式：cordon + drain |
| `POST /images/delete?id=` | 删除镜像 |
| `GET /droidnode/images` | APK 镜像页字段 |

`/metrics` 使用 `droidnode_*` 指标，不是 kubelet 指标名集合。

## 5. 尚未实现 / 差异

| 端点 | 状态 |
|---|---|
| `/runningpods/` | 未实现 |
| `/configz` | 未实现 |
| `/metrics/cadvisor` | 未实现 |
| `/metrics/probes` | 未实现 |
| `/debug/pprof/` | 未实现 |

| 项 | kubelet v1.29 | DroidNode |
|---|---|---|
| 只读端口 | 10250 需要 authn/authz | 10255；`/healthz`、`/metrics` 匿名，数据端点要求客户端证书 |
| 授权 | 客户端证书 + RBAC | 只校验证书签名，无 RBAC |
| 协议 | SPDY + WebSocket + HTTP/2 | 仅 SPDY/3.1 |
| kubeletVersion | 跟随集群 | 固定 `v1.29.0` |
| Pod IP | 每 Pod 独立 IP | 等于节点 IP |
| 指标 | `kubelet_*`/`container_*` | `droidnode_*` |
| 运行时 | containerd/CRI | proot |

## 6. 基线约束

- 编译 schema：`k8s-openapi 0.21`，feature `v1_29`。
- 版本字符串唯一来源：`models::KUBELET_VERSION = "v1.29.0"`。
- 不使用 1.30+ 新增字段，不按集群版本分支行为，宽松解析未知字段。
- 当前只验证 1.29；1.30+ 未测试。

## 7. 注意

- k3s 1.29 必须保持 `egress-selector-mode: disabled`，否则 apiserver 代理 502。
- `kubectl logs/exec` 需要 client CA；token 加入时 agent 从
  `kube-system/droidnode-client-ca` ConfigMap 自动缓存，否则手动推
  `files/node/config/client-ca.crt`。
- 地址 + token 加入时，App 会清理粘贴产生的空格/换行。
- svclb 排除方法见根目录 README 的“加入节点后：处理 svclb”。
