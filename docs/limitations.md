# 能力边界与已知限制

> 最后核对：2026-11-13。这里只写**当前代码真实存在的行为**与物理约束。
> 功能清单见 [gap-list-feasibility.md](gap-list-feasibility.md)。

## 一、非 root / proot 的硬限制

| 限制 | 表现 | 变通 |
|---|---|---|
| **没有独立 Pod IP / CNI** | 所有容器共享设备网络栈；`podIP = nodeIP` | 直接用 `nodeIP:port`；Service 走 ClusterIP 或 D3 名称代理 |
| **没有网络命名空间 / NetworkPolicy** | Pod 之间无三层隔离，NetworkPolicy 不生效 | 仅限可信负载；用端口与外部防火墙隔离 |
| **端口 <1024 不能 bind** | `containerPort: 80` 报 `PrivilegedPort`；Service port 也必须 ≥1024 | 用 8080/8443 等高端口 |
| **同一端口只能一个 Pod** | 共享 netns，第二个 Pod 报 `PortConflict` | 错开端口；D3 的 Service port 也不要和容器端口相同 |
| **没有 cgroup 硬限制** | `resources.limits` 不生效；内存打满由系统 OOM 杀进程，不是 `OOMKilled` | `RLIMIT_AS` + nice 软限制（B26） |
| **容器内 root 是假的** | `-0` 只让 `uid` 显示 0，实际仍是 app uid | 需要真正 capability 的程序跑不了 |
| **seccomp 不可用** | `securityContext.seccompProfile` 被忽略 | 无 |
| **没有独立 PID 视图** | 容器内 `ps` 能看到宿主进程 | 排障时注意 |
| **`readOnly` 挂载无内核语义** | proot `-b` 不强制只读，容器仍可写源目录 | 把源目录放到应用不可写的位置 |
| **无 cgroup 计量** | `kubectl top` 用 `/proc` 进程树估算，不是精确值 | 当作量级参考 |

## 二、网络与 Service

| 项 | 现状 |
|---|---|
| **ClusterIP** | 可用：EndpointSlice 发布 `nodeIP:targetPort`，kube-proxy DNAT；实测 `10.43.x.x:8080` 返回 200 |
| **Service 名称** | D3：每 10s 同步 Service/EndpointSlice，为每个 Service 分配 `127.10.x.y`，Pod `/etc/hosts` 写入 `svc`、`svc.ns`、`svc.ns.svc`、FQDN；代理到 Ready Endpoint |
| **D3 端口约束** | D3 listener 占用 `servicePort`；容器若同时 `bind 0.0.0.0:同一端口` 会 `EADDRINUSE`，Service port 与容器 port 应错开 |
| **集群 DNS** | 不可行：`ip_unprivileged_port_start=1024`，app 不能 bind 53；`resolv.conf` 不支持端口。D3 hosts 只覆盖精确 Service 名称，通配/外部域名/SRV 需 VpnService/TUN 或 root |
| **svclb / LoadBalancer** | proot 内无法执行 iptables；`type: LoadBalancer` 会 `EXTERNAL-IP: <pending>`。用 ClusterIP/NodePort/Pod IP |
| **多副本** | 同一节点上的副本共享同一个 `podIP`，Service 无法按地址区分；跨节点副本不受此限 |

## 三、存储

| 项 | 现状 |
|---|---|
| **PVC** | 仅 `storageClassName: droidnode-local`：agent 自动建 hostPath PV（`files/volumes/pv/<uid>`），数据 Retain；不支持其他 StorageClass、动态扩容、快照 |
| **exec 看不到卷** | `kubectl exec` 的 helper 只挂 rootfs，不挂 Pod volumes；容器自身能看到 `/data`，exec 内看不到 |
| **镜像元数据** | 解层丢 uid/gid/xattr/setuid；`-0` 只让 owner 看起来对 |
| **rootfs 位置** | `layers/`、`rootfs/` 在 `filesDir`，系统清缓存不会删；`codeCacheDir` 只放 proot 临时目录 |

## 四、远程命令与 TTY

| 项 | 现状 |
|---|---|
| `exec` / `attach` / `port-forward` / `cp` | 通过 Go SPDY sidecar 工作；`cp` 随 exec 自动可用 |
| **TTY** | 管道而非 PTY；`kubectl exec -it` / `attach -it` 可执行命令，但没有 echo、方向键、行编辑；resize 忽略 |
| `port-forward svc/x` | 当前只实现 Pod 级 `/portForward/<ns>/<pod>`；Service 级需要 D3 代理配合 |
| 日志 | `-f`、`--tail`、`--since`、`--timestamps`、`--previous` 均支持；日志文件 5 MiB 轮转 |

## 五、安全

| 项 | 现状 / 风险 |
|---|---|
| **kubelet API 鉴权（F1）** | 远程请求要求集群客户端 CA；loopback（Android UI）与 `/healthz`、`/metrics*` 例外。没有 `SubjectAccessReview`/细粒度 RBAC |
| **kubeconfig（F2）** | `files/kubeconfig.enc`（Android Keystore AES-GCM）；运行时 `kubeconfig.runtime` 仅在服务运行期间存在；`force-stop` 跳过 onDestroy 时可能残留到下次启动，启动会覆盖；Keystore 密钥失效需重新导入 |
| **serving 证书（A16）** | 通过 `kubernetes.io/kubelet-serving` CSR 自动批准并由集群 CA 签发；自动批准绕过人工审批，仅适合单租户/可信集群；CSR 失败回退自签 CA，此时需自定义 bundle |
| **容器隔离** | proot 不是安全沙箱；容器与应用同 uid，能读应用私有文件；不要跑不受信镜像 |

## 六、Android 运行时

| 项 | 现状 |
|---|---|
| **targetSdk 35** | 已升级；Android 14 / PGJM10 上 proot rootfs exec、exec/logs/top 正常。Android 15/16 与其他 OEM 仍需复验 |
| **FGS 类型** | `specialUse` + `FOREGROUND_SERVICE_SPECIAL_USE`；`dataSync` 6 小时限制不再适用 |
| **Doze / 息屏** | WakeLock + WifiLock 已启用，空闲 60s 释放 WakeLock；部分 ROM 仍可能延迟心跳，节点短暂 `NotReady` |
| **通知** | Android 13+ 需要 `POST_NOTIFICATIONS`；拒绝后服务仍运行，但通知不可见 |
| **ABI** | 只验证 `arm64-v8a`；ARM32/x86 未测 |

## 七、D3 ClusterIP 代理

| 项 | 现状 / 风险 |
|---|---|
| **端口范围** | 非 root 应用不能绑定 <1024 端口（Android 的 `ip_unprivileged_port_start=1024`）。Service 端口为 53/80/443 等时，`127.10.x.y` 上的 loopback 监听会失败，日志只提示一次并建议改用 NodePort；≥1024 的端口正常工作 |
| **协议** | 只代理 TCP；UDP（DNS）不支持 |
| **可见性** | 仅供本机容器通过 `/etc/hosts` 注入的 `127.10.x.y` 访问；集群其他节点不经过这里 |

## Mirror registries

- 私有/环回 IP 上的 registry 默认按 HTTP 访问（LAN 本地仓库通常没有可信证书）；主机名形式的 registry 仍按 HTTPS，需在 egistry-mirrors.env 写 DROIDNODE_INSECURE_REGISTRIES=host:port（Settings 暂无该字段的 UI）。
