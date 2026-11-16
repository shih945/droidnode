# Kubelet API 兼容性（v1.29 基线）

目标：**只保证 kubelet v1.29 的预期行为和 API 兼容**；为 APK 功能新增的字段/端点单独标注，不算不兼容。

- 契约（单一来源）：[`kubelet-contract-v1.29.json`](kubelet-contract-v1.29.json)
- 对比脚本：[`scripts/kubelet-compat.py`](../../scripts/kubelet-compat.py)
- 当前状态：**初始静态审计**（设备/控制面离线时，用当前 handler 输出形状构造的 fixture 跑出；连上设备后用 `capture-adb` 刷新）

---

## 1. 怎么跑

```powershell
# 1) 从手机抓当前 agent 响应（adb forward + HTTPS）
$env:ADB='D:\app\android-sdk\platform-tools\adb.exe'
python scripts/kubelet-compat.py capture-adb --serial 192.168.2.5:34351 --out build-cache/kubelet-compat/target

# 2) 对照 v1.29 契约生成报告
python scripts/kubelet-compat.py check --target build-cache/kubelet-compat/target `
  --report build-cache/kubelet-compat/report.md

# 3) 控制面恢复后，抓真实 kubelet 做 live diff
python scripts/kubelet-compat.py capture-kubectl `
  --kubeconfig D:\doc\k3s-debian13.7.yaml --node k3s-main --out build-cache/kubelet-compat/reference
python scripts/kubelet-compat.py compare --reference build-cache/kubelet-compat/reference `
  --target build-cache/kubelet-compat/target
```

`check` 输出四类结果：

| 分类 | 含义 |
|---|---|
| 缺少的 kubelet 必填字段 | **不兼容**，需要修 |
| 类型不符 | **不兼容**，需要修 |
| APK 扩展字段 | kubelet 没有、APK 需要，**只标注** |
| 未识别额外字段 | 需要确认是标准可选字段还是扩展 |

---

## 2. 当前不兼容清单

### 2.1 `/pods`（PodList）

缺少 8 个 kubelet 必填字段：

| 字段 | 说明 |
|---|---|
| `items[].spec.containers[].name` | 没有返回 Pod spec，工具无法看到容器定义 |
| `items[].spec.containers[].image` | 同上 |
| `items[].status.startTime` | Pod 启动时间缺失（`describe`/dashboard 用） |
| `items[].status.qosClass` | QoS class 缺失 |
| `items[].status.conditions[].type` | Ready/Initialized 等条件缺失 |
| `items[].status.conditions[].status` | 条件状态缺失 |
| `items[].status.containerStatuses[].image` | 容器镜像名缺失 |
| `items[].status.containerStatuses[].imageID` | 镜像 ID 缺失 |

已有且兼容：`metadata.name/namespace/uid`、`spec.nodeName`、`status.phase/hostIP/podIP`、`containerStatuses[].name/ready/restartCount/state`。

### 2.2 `/images`（ImageList）

**结构不兼容**：kubelet 返回的是

```json
{ "images": [ { "names": ["repo:tag"], "sizeBytes": 123, "id": "sha256:..." } ] }
```

DroidNode 返回的是 APK 用的自定义结构：

```json
{ "images": [ { "name": "...", "layers": 3, "compressed": ..., "unpacked": ..., "digest": "...", "registry": "...", "pulled_at": "...", "rootfs": "..." } ], "total_size": ..., "count": ... }
```

缺少 kubelet 必填字段：`images[].names`、`images[].sizeBytes`、`images[].id`。

**建议修法**：`/images` 改成 kubelet 的 `ImageList`，APK 改调 `/droidnode/images`（或 `/images?format=droidnode`），把现有字段保留为扩展端点。

### 2.3 `/stats/summary`

必填字段齐全（`node.*`、`pods[].podRef.*`、`cpu`、`memory`、`fs`），
但 `node.systemContainers` 为空、`network/runtime` 是占位值；这些是可选/弱字段，不影响基本消费方。

### 2.4 `/metrics`

kubelet 暴露 `kubelet_*` / `container_*` / `process_*`；DroidNode 是 `droidnode_*` 自定义指标（电量、内存、存储、Pod 计数）。
**按 APK 扩展处理**，不追求指标名兼容；如果以后接 metrics-server，需要单独补 kubelet 指标。

---

## 3. 缺失的 kubelet 端点

| 端点 | 状态 | 说明 |
|---|---|---|
| `/runningpods/` | 未实现 | 只读端口上的运行中 Pod 列表 |
| `/configz` | 未实现 | kubelet 配置导出 |
| `/metrics/cadvisor` | 未实现 | cAdvisor 指标 |
| `/metrics/probes` | 未实现 | 探针结果指标 |
| `/debug/pprof/` | 未实现 | 性能分析 |

这些端点目前没有消费方；如果要做“完整 kubelet 兼容”，需要补，优先级低于 `/pods`、`/images`。

---

## 4. APK 扩展（不算不兼容，只标注）

### 端点

| 端点 | 用途 |
|---|---|
| `POST /drain` | 低电量保护：停止本节点 Pod、保留 agent |
| `POST /undrain` | 恢复调度 |
| `POST /maintenance?on=` | 维护模式：cordon + 排空 |
| `POST /images/delete?id=` | 镜像页删除（kubelet 没有删除 API） |

### `/images` 字段

`images[].name/layers/compressed/unpacked/digest/registry/pulled_at/rootfs`、`total_size`、`count`。

### `/metrics` 指标

`droidnode_battery_*`、`droidnode_network_up`、`droidnode_memory_*`、`droidnode_storage_*` 等。

---

## 5. 契约之外的差异（行为/协议/安全）

| 项 | kubelet v1.29 | DroidNode | 影响 |
|---|---|---|---|
| 只读端点端口 | 10250 需要 authn/authz；只读端口默认关闭 | 10255 匿名可读 | 同 LAN 可读 Pod/镜像/日志；安全模型不同 |
| exec/attach/port-forward 授权 | 客户端证书 + RBAC（SubjectAccessReview） | 只校验客户端证书签名，无 RBAC | 拿到集群客户端证书即可 exec，权限粒度更粗 |
| exec/attach/port-forward 协议 | SPDY/3.1 + WebSocket + HTTP/2 流 | 仅 SPDY/3.1 | 新版 kubectl 默认 WebSocket 时会回退 SPDY（需要验证） |
| kubeletVersion | 跟随集群（1.29/1.36） | 固定 `v1.29.0` | 控制面 1.36 下 skew 7 个 minor，`kubectl` 可能出现版本警告 |
| Pod IP | 每 Pod 独立 IP（CNI） | 全部等于节点 IP | `podIPs` 重复、网络策略/Service 语义不完整 |
| 容器运行时 | containerd/CRI | proot | 无 cgroup、无 privileged、无 hostNetwork |
| 日志 | `/var/log/pods` + `/logs/` | 内存/文件缓冲，`/logs/` 未实现 | 日志轮转/大文件行为不同 |

---

## 6. 修复顺序建议

1. `/images` 结构（最硬的 schema 不兼容；APK 改调扩展端点）
2. `/pods` 补 `spec.containers`、`status.startTime`、`conditions`、`qosClass`、`image/imageID`
3. 确认 kubectl 新版走 WebSocket 时是否回退 SPDY（`kubectl exec -v=6` 看协议）
4. 按需补 `/runningpods/`、`/metrics/cadvisor`
5. 把 `check --fail-on-missing` 接进 CI（先修完 1、2 再开）

---

## 7. 当前结论

- **不兼容**：`/images` 结构、`/pods` 缺 8 个字段、缺少 5 个 kubelet 端点、认证/协议/版本 skew 差异。
- **仅为 APK 扩展**：4 个自定义端点、`/images` 的 10 个自定义字段、`droidnode_*` 指标。
- 具体报告以 `scripts/kubelet-compat.py check` 的输出为准；本文件是当前基线的摘要。
