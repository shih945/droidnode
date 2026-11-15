# 能力清单与实现状态

> 最后核对：2026-11-13。当前 **97 / 97**，没有未实现的功能项。
> 本文只保留与代码/真机现状一致的结论；历史计划与逐条打勾记录已删除。
> 物理限制与使用前提见 [limitations.md](limitations.md)；变更摘要见 [CHANGELOG.md](CHANGELOG.md)。

## 1. 验证环境

| 项 | 值 |
|---|---|
| 手机 | OnePlus PGJM10，Android 14，8 核，11.7 GB |
| 节点 | `droidnode-37e62e0fe8ce0057`，`192.168.2.9`，kubelet `:10255` |
| 控制面 | Debian，`192.168.2.7`，k3s `v1.36.5+k3s1` |
| API schema | `k8s-openapi 0.21 / v1_29`，上报 `kubeletVersion=v1.29.0` |
| Android 构建 | `compileSdk 35`、`targetSdk 35`、`minSdk 26`，FGS `specialUse` |

## 2. 功能状态

### Pod 生命周期

| 能力 | 状态 | 说明 |
|---|---|---|
| 多容器 Pod / initContainers | ✅ | init 串行，主容器各自 rootfs；共享 Pod 卷 |
| 每 Pod 可写 rootfs | ✅ | 镜像 rootfs 共享，Pod 启动时复制到 `rootfs/pods/<uid>/<container>` |
| restartPolicy | ✅ | Always / OnFailure / Never；指数退避 10s→5min |
| 探针 | ✅ | exec / HTTP / TCP；startup→liveness→readiness，失败重启 |
| 状态与条件 | ✅ | phase、containerStatuses、Ready/Initialized/ContainersReady、podIP/podIPs |
| 事件 | ✅ | Pulling/Pulled/Started/Failed/PortConflict/PrivilegedPort 等 |
| 删除/优雅终止 | ✅ | deletionTimestamp + grace，SIGTERM→SIGKILL |
| 日志 | ✅ | 内存 + 落盘轮转；`-f`、`--tail`、`--since`、`--timestamps`、`--previous` 均支持 |

### 镜像与存储

| 能力 | 状态 | 说明 |
|---|---|---|
| OCI 拉取 | ✅ | manifest list 选 arm64、sha256 校验、层缓存 |
| 镜像源 | ✅ | `docker.io` / `registry.k8s.io` / `quay.io` 可改写 |
| imagePullPolicy / imagePullSecrets | ✅ | 含 dockerconfigjson |
| 镜像列表/删除 | ✅ | kubelet `/images`、`/images/delete` |
| emptyDir / hostPath / configMap / secret | ✅ | 挂载到 proot |
| ServiceAccount token | ✅ | 使用 agent 自己的 SA 身份 |
| PVC | ✅ | `storageClassName: droidnode-local` 自动建 hostPath PV；数据 Retain |

### 网络与 Service

| 能力 | 状态 | 说明 |
|---|---|---|
| hostNetwork 语义 | ✅ | 所有容器共享设备网栈；`podIP=podIPs=nodeIP` |
| 端口冲突/特权端口诊断 | ✅ | `PortConflict` / `PrivilegedPort` |
| EndpointSlice | ✅ | Ready Pod 发布 `nodeIP:targetPort` |
| ClusterIP | ✅ | kube-proxy DNAT，实测 `10.43.x.x:8080` 返回 200 |
| Service 名称 | ✅ | D3：每 Service 一个 `127.10.x.y` 回环代理，Pod `/etc/hosts` 写 `svc`/`svc.ns`/FQDN |
| 集群 DNS | ❌ | 端口 53 非 root 不可 bind；D3 hosts 只覆盖精确 Service 名称 |

### 调试与远程命令

| 能力 | 状态 | 说明 |
|---|---|---|
| `kubectl exec` | ✅ | Rust TLS/101 + Go SPDY sidecar；stdin/stdout/stderr/exit code |
| `kubectl attach` | ✅ | 容器 IO 注册表 + 三对 Unix socket |
| `kubectl port-forward` | ✅ | SPDY `port` stream → `127.0.0.1:port` |
| `kubectl cp` | ✅ | 随 exec 自动可用 |
| TTY | ⚠️ | 管道而非 PTY；`-it` 可执行命令，但无 echo/行编辑 |

### 安全

| 能力 | 状态 | 说明 |
|---|---|---|
| kubelet API 鉴权 | ✅ | 远程请求要求集群客户端 CA；loopback/health/metrics 例外 |
| kubeconfig 加密 | ✅ | Android Keystore AES-GCM；`kubeconfig.enc` + 运行时 `kubeconfig.runtime` |
| serving 证书 | ✅ | `kubernetes.io/kubelet-serving` CSR，集群 CA 签发 |
| 容器真 root / capability | ❌ | proot `-0` 只是假 root |
| seccomp / cgroup / NetworkPolicy | ❌ | 非 root 无法实现 |

### Android 与运维

| 能力 | 状态 | 说明 |
|---|---|---|
| 前台服务 | ✅ | `targetSdk 35` + `specialUse` + `PROPERTY_SPECIAL_USE_FGS_SUBTYPE` |
| 通知权限 | ✅ | `POST_NOTIFICATIONS` 运行时申请 |
| WakeLock / WifiLock | ✅ | 有 Pod 时持有，空闲释放；Doze 仍可能延迟心跳 |
| 电池/网络遥测 | ✅ | IPC 推送至 Rust，用于 Node condition |
| 低电 drain | ✅ | 先 NotReady，停容器，保留 agent |
| 维护模式 | ✅ | 设置页 `/maintenance`：cordon + drain；节点页只读提示 |
| `kubectl top` | ✅ | `/metrics/resource` + `/stats/summary`（进程树估算） |

## 3. 已确认不可行

| 项 | 原因 | 替代 |
|---|---|---|
| Pod 独立 IP / CNI | 非 root 不能加地址、不能收非本机 IP 入站 | `podIP=nodeIP` |
| NetworkPolicy / 网络隔离 | 无 netns / NET_ADMIN | 共用宿主网络，靠端口隔离 |
| cgroup 硬限制 / OOMKilled | 无法创建 cgroup | `RLIMIT_AS` + nice 软限制 |
| 容器内真 root / capability | 假 root | 无 |
| seccomp | 无法安装过滤器 | 无 |
| 集群 DNS stub | 不能 bind 53 | D3 `/etc/hosts` 精确名称 |
| svclb / LoadBalancer | proot 内无法 iptables | 用 ClusterIP/NodePort/Pod IP |

## 4. 仍需复验（不是未实现）

- **E3/E4 机型矩阵**：只在 Android 14 / PGJM10 验证 targetSdk 35 + proot exec；
  Android 15/16 与其他 OEM 需重跑。
- **CSR 自动批准**：agent SA 可批准 `kubernetes.io/kubelet-serving` CSR，仅适合可信集群。
- **D3 端口冲突**：D3 listener 绑定 `servicePort`，容器不要同时 `bind 0.0.0.0:同一端口`。
- **TTY**：交互式 exec/attach 没有 PTY 语义。

## 5. 相关文档

- [limitations.md](limitations.md)
- [benchmark.md](benchmark.md)
- [cluster-bootstrap.md](cluster-bootstrap.md)
- [api-compatibility.md](api-compatibility.md)
- [CHANGELOG.md](CHANGELOG.md)
