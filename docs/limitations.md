# 能力边界与已知限制

> 最后核对：2026-11-16。这里只写当前代码真实存在的行为与物理约束。

## 一、非 root / proot 的硬限制

| 限制 | 表现 | 变通 |
|---|---|---|
| **没有独立 Pod IP / CNI** | 所有容器共享设备网络栈；`podIP = nodeIP` | 直接用 `nodeIP:port`；Service 走 ClusterIP 或 Service 名代理 |
| **没有网络命名空间 / NetworkPolicy** | Pod 之间无三层隔离，NetworkPolicy 不生效 | 仅限可信负载；用端口与外部防火墙隔离 |
| **端口 <1024 不能 bind** | `containerPort: 80` 报 `PrivilegedPort`；Service port 也必须 ≥1024 | 用 8080/8443 等高端口 |
| **同一端口只能一个 Pod** | 共享 netns，第二个 Pod 报 `PortConflict` | 错开端口；Service 名代理的 Service port 也不要和容器端口相同 |
| **没有 cgroup 硬限制** | `resources.limits` 不生效；内存打满由系统 OOM 杀进程，不是 `OOMKilled` | `RLIMIT_AS` + nice 软限制 |
| **容器内 root 是假的** | `-0` 只让 `uid` 显示 0，实际仍是 app uid | 需要真正 capability 的程序跑不了 |
| **seccomp 不可用** | `securityContext.seccompProfile` 被忽略 | 无 |
| **没有独立 PID 视图** | 容器内 `ps` 能看到宿主进程 | 排障时注意 |
| **`readOnly` 挂载无内核语义** | proot `-b` 不强制只读，容器仍可写源目录 | 把源目录放到应用不可写的位置 |
| **无 cgroup 计量** | `kubectl top` 用 `/proc` 进程树估算，不是精确值 | 当作量级参考 |

## 二、网络与 Service

| 项 | 现状 |
|---|---|
| **ClusterIP** | 可用：EndpointSlice 发布 `nodeIP:targetPort`，kube-proxy DNAT；实测 `10.43.x.x:8080` 返回 200 |
| **Service 名称** | Service 名代理：每 10s 同步 Service/EndpointSlice，为每个 Service 分配 `127.10.x.y`，Pod `/etc/hosts` 写入 `svc`、`svc.ns`、`svc.ns.svc`、FQDN；代理到 Ready Endpoint |
| **Service 名代理端口约束** | 代理监听占用 `servicePort`；容器若同时 `bind 0.0.0.0:同一端口` 会 `EADDRINUSE`，Service port 与容器 port 应错开 |
| **集群 DNS** | 不可行：`ip_unprivileged_port_start=1024`，app 不能 bind 53；`resolv.conf` 不支持端口。Service 名代理只覆盖 `/etc/hosts` 里的精确 Service 名称，通配/外部域名/SRV 需 VpnService/TUN 或 root |
| **svclb / LoadBalancer** | proot 内无法执行 iptables；svclb DaemonSet 默认会调度到手机并卡 `PrivilegedPort`，需用 nodeAffinity 排除（见 README）；`type: LoadBalancer` 的 `EXTERNAL-IP` 仍为 pending，用 ClusterIP/NodePort/Pod IP |
| **多副本** | 同一节点上的副本共享同一个 `podIP`，Service 无法按地址区分；跨节点副本不受此限 |

## 三、存储

| 项 | 现状 |
|---|---|
| **PVC** | 仅 `storageClassName: droidnode-local`：agent 自动建 hostPath PV（`files/node/volumes/pv/<uid>`），数据 Retain；不支持其他 StorageClass、动态扩容、快照 |
| **exec 看不到卷** | `kubectl exec` 的 helper 只挂 rootfs，不挂 Pod volumes；容器自身能看到 `/data`，exec 内看不到 |
| **镜像元数据** | 解层丢 uid/gid/xattr/setuid；`-0` 只让 owner 看起来对 |
| **rootfs 位置** | `layers/`、`rootfs/` 在 `files/node`，系统清缓存不会删；`codeCacheDir` 只放 proot 临时目录 |

## 四、远程命令与 TTY

| 项 | 现状 |
|---|---|
| `exec` / `attach` / `port-forward` / `cp` | 通过 Go SPDY sidecar 工作；`cp` 随 exec 自动可用 |
| **TTY** | 管道而非 PTY；`kubectl exec -it` / `attach -it` 可执行命令，但没有 echo、方向键、行编辑；resize 忽略 |
| `port-forward svc/x` | 当前只实现 Pod 级 `/portForward/<ns>/<pod>`；Service 级需要 Service 名代理配合 |
| 日志 | `-f`、`--tail`、`--since`、`--timestamps`、`--previous` 均支持；日志文件 5 MiB 轮转 |

## 五、安全

| 项 | 现状 / 风险 |
|---|---|
| **kubelet API 鉴权** | 远程请求要求集群客户端 CA；loopback（Android UI）与 `/healthz`、`/metrics*` 例外。没有 `SubjectAccessReview`/细粒度 RBAC |
| **kubeconfig** | `files/node/config/kubeconfig.enc`（Android Keystore AES-GCM）；运行时 `kubeconfig.runtime` 仅在服务运行期间存在；`force-stop` 跳过 onDestroy 时可能残留到下次启动，启动会覆盖；Keystore 密钥失效需重新导入 |
| **serving 证书** | 通过 `kubernetes.io/kubelet-serving` CSR 自动批准并由集群 CA 签发并持久化（CA 指纹变化时重新申请）；自动批准绕过人工审批，仅适合单租户/可信集群 |
| **client CA** | `files/node/config/client-ca.crt`；token 加入可从 `kube-system/droidnode-client-ca` ConfigMap 自动缓存 |
| **容器隔离** | proot 不是安全沙箱；容器与应用同 uid，能读应用私有文件；不要跑不受信镜像 |

## 六、Android 运行时

| 项 | 现状 |
|---|---|
| **targetSdk 35** | 已升级；Android 14 / PGJM10 上 proot rootfs exec、exec/logs/top 正常。Android 15/16 与其他 OEM 仍需复验 |
| **FGS 类型** | `specialUse` + `FOREGROUND_SERVICE_SPECIAL_USE`；`dataSync` 6 小时限制不再适用 |
| **Doze / 息屏** | 默认全程持有 WakeLock/WifiLock，锁屏也保持 Ready；电池优化未豁免时 Doze 会忽略 WakeLock 并冻结 agent，节点转 `NotReady`（节点页有提示）。设置里可关掉「锁屏保持在线」，退回空闲 60s 放锁的省电模式 |
| **通知** | Android 13+ 需要 `POST_NOTIFICATIONS`；拒绝后服务仍运行，但通知不可见 |
| **ABI** | 只验证 `arm64-v8a`；ARM32/x86 未测 |

## 七、Service 名代理（ClusterIP 回环代理）

| 项 | 现状 / 风险 |
|---|---|
| **端口范围** | 非 root 应用不能绑定 <1024 端口（Android 的 `ip_unprivileged_port_start=1024`）。Service 端口为 53/80/443 等时，`127.10.x.y` 上的 loopback 监听会失败，日志只提示一次并建议改用 NodePort；≥1024 的端口正常工作 |
| **协议** | 只代理 TCP；UDP（DNS）不支持 |
| **可见性** | 仅供本机容器通过 `/etc/hosts` 注入的 `127.10.x.y` 访问；集群其他节点不经过这里 |

## 八、节点失联后的残留对象

手机被关机、掉电或被系统杀掉后，它的 Node 对象会留在集群里（`NotReady` + `unreachable` 污点），而它上面的 Pod 会停在 `Terminating` 并且**永远不会消失**：

1. 心跳停止 40 s 后节点变 `NotReady`；
2. 污点管理器按 `--default-unreachable-toleration-seconds`（默认 300 s）把 Pod 标记删除，写入 `deletionTimestamp`，宽限期沿用 Pod 自己的 `terminationGracePeriodSeconds`；
3. 删除 Pod 的最后一步是"该节点的 kubelet 确认容器已停止"，而 agent 已经不在了；
4. Pod 对象仍然引用一个**存在的** Node，而 `pod-garbage-collector` 只回收"Node 已不存在"的孤儿 Pod，所以没人接手。

这是 Kubernetes 的默认行为，不是本平台的缺陷——它宁可留一个僵尸对象，也不肯凭空认定容器已死。普通集群靠"删除节点"或"机器很快回来"收敛，手机会被反复关机，所以需要主动清理。

残留对象不占流量（`Ready=False`，已从 EndpointSlice 摘除）、不占资源、不占端口，真正的问题是**观感与排障**：`kubectl get pods` 长期挂着 Terminating，`kubectl top` 与监控计数偏移，对该节点 `drain` 会永远等下去。

### 自动清理（控制面 CronJob，每 5 分钟）

判定条件刻意收窄，保证不碰正常流程：**Pod 已有 `deletionTimestamp`、删除开始超过 10 分钟、且其 `spec.nodeName` 对应的 Node 不存在或超过 10 分钟没有心跳**。三条同时满足才强制删除。

```yaml
apiVersion: v1
kind: ServiceAccount
metadata: { name: droidnode-reaper, namespace: kube-system }
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata: { name: droidnode-reaper }
rules:
  - apiGroups: [""]
    resources: ["pods"]
    verbs: ["list", "delete"]
  - apiGroups: [""]
    resources: ["nodes"]
    verbs: ["get", "list"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata: { name: droidnode-reaper }
roleRef: { apiGroup: rbac.authorization.k8s.io, kind: ClusterRole, name: droidnode-reaper }
subjects:
  - { kind: ServiceAccount, name: droidnode-reaper, namespace: kube-system }
---
apiVersion: batch/v1
kind: CronJob
metadata: { name: droidnode-reaper, namespace: kube-system }
spec:
  schedule: "*/5 * * * *"
  concurrencyPolicy: Forbid
  startingDeadlineSeconds: 60
  jobTemplate:
    spec:
      backoffLimit: 1
      template:
        spec:
          serviceAccountName: droidnode-reaper
          restartPolicy: Never
          containers:
            - name: reaper
              image: docker.io/bitnami/kubectl:1.29   # 国内网络需加镜像源前缀
              command: ["/bin/sh", "-c"]
              args:
                - |
                  set -eu
                  now=$(date +%s)
                  kubectl get nodes -o jsonpath='{range .items[*]}{.metadata.name}{" "}{.status.conditions[?(@.type=="Ready")].lastHeartbeatTime}{"\n"}{end}' > /tmp/nodes
                  kubectl get pods -A -o jsonpath='{range .items[*]}{.metadata.namespace}{" "}{.metadata.name}{" "}{.spec.nodeName}{" "}{.metadata.deletionTimestamp}{"\n"}{end}' |
                  while read -r ns pod node deleted; do
                    [ -n "${deleted:-}" ] || continue
                    since=$(date -d "$deleted" +%s) || continue
                    [ $((now - since)) -ge 600 ] || continue
                    hb=$(awk -v n="$node" '$1 == n {print $2}' /tmp/nodes)
                    if [ -z "$hb" ]; then
                      echo "node gone: $ns/$pod"
                    else
                      hb_at=$(date -d "$hb" +%s) || continue
                      [ $((now - hb_at)) -ge 600 ] || continue
                      echo "node stale ($hb): $ns/$pod"
                    fi
                    kubectl delete pod -n "$ns" "$pod" --force --grace-period=0
                  done
```

放在控制面而不是每台手机，是因为**只有一个执行者**：好审计、好停用，也不需要给每个节点的 ServiceAccount 追加权限。

### 手工兜底

仓库里带了脚本，只处理同时满足三个条件的对象：**已在删除**、**删除开始超过 N 分钟（默认 10）**、**所属 Node 不存在或心跳也超过 N 分钟**。第三条不能省：只看"删除很久"就动手，会把健康节点上正在优雅退出（长 grace、drain 卡住）的 Pod 误删。

```bash
scripts/reap-stale-pods.sh                 # 干跑，只打印计划
scripts/reap-stale-pods.sh --apply         # 执行
scripts/reap-stale-pods.sh --apply --minutes 30
```

```powershell
pwsh -File scripts/reap-stale-pods.ps1                # 干跑
pwsh -File scripts/reap-stale-pods.ps1 -Apply         # 执行
pwsh -File scripts/reap-stale-pods.ps1 -Apply -Minutes 30
```

两个脚本都会先校验 `kubectl get pods` / `kubectl get nodes`：任一失败就退出，不会在"拿不到数据"的情况下把所有 Pod 当成孤儿删掉。需要能 `list nodes` 和 `delete pods` 的 kubeconfig（控制面 admin）；节点 SA `droidnode-agent` 没有 `nodes list`，用它跑会直接报错——这是有意的。

> 为什么不再给一行式管道：Windows PowerShell 会吞掉 jsonpath 里 `{"\n"}` 的引号、`\` 也不是续行符；更关键的是单行管道没有退出码检查，kubectl 失败时会**静默地什么都不做**，看起来像"跑过了没效果"。


### 与优雅停止的配合

agent 正常收到 SIGTERM 时会按"先摘流量、再停容器、再删对象、最后标记节点"的顺序收尾，把自己节点上的 Pod 全部删干净，因此**主动停止不会产生残留对象**；CronJob 只负责兜住掉电/被系统杀掉这类非优雅退出。两者的分工是：能说话的时候自己打扫，不能说话的时候由控制面代扫。

## Mirror registries

- 私有/环回 IP 上的 registry 默认按 HTTP 访问（LAN 本地仓库通常没有可信证书）；主机名形式的 registry 仍按 HTTPS，需在 `files/node/config/registry-mirrors.env` 写 `DROIDNODE_INSECURE_REGISTRIES=host:port`（Settings 暂无该字段的 UI）。
