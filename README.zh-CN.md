[English](README.md) · **简体中文**

# DroidNode

把一台 Android 手机变成 Kubernetes 工作节点 —— **不需要 root，不需要自定义内核，不需要改 k3s**。

`kubectl apply`、`logs`、`exec`、`attach`、`cp`、`port-forward`、Service 名称与 metrics，
都能对一个普通的 k3s 控制面工作。从调度器视角看，这台手机就是一个普通 Linux worker；
在设备上，agent 是一个用户态迷你 kubelet，加一套基于 proot 的 OCI 运行时。

```
kubectl ──▶ k3s apiserver ──▶ Android 上的 Rust agent（HTTPS kubelet，:10255）
                                  ├─ 对账分配到本节点的 Pod
                                  ├─ 拉取 / 校验 / 解包 OCI 镜像
                                  ├─ proot 容器进程（假 root，共享宿主 netns）
                                  └─ Go SPDY sidecar，负责 exec / attach / port-forward
```

## 当前能力

| 领域 | 现在可用 |
|---|---|
| Pod 生命周期 | 多容器 Pod、initContainers（串行）、restartPolicy Always/OnFailure/Never 与 kubelet 风格退避、探针（exec/HTTP/TCP）、优雅删除、事件、`CrashLoopBackOff` |
| 镜像 | OCI 拉取（含镜像源改写）、linux/arm64 manifest 选择、sha256 校验、层缓存、`imagePullPolicy`、`imagePullSecrets`、镜像列表/删除 |
| 存储 | emptyDir、hostPath、configMap、secret、投影的 ServiceAccount token，以及 `droidnode-local` PVC（自动创建 hostPath PV） |
| 网络 | 共享设备网络栈（hostNetwork 语义）；`podIP = nodeIP`；selector Service 发布 EndpointSlice；ClusterIP DNAT 可用；Service **名称** 通过 Service 名代理（回环代理）解析（`svc`、`svc.ns`、FQDN） |
| 调试 | `kubectl exec`、`attach`、`port-forward`、`cp`、`logs -f/--tail`；SPDY/3.1 由 [agent/spdy-sidecar](agent/spdy-sidecar/main.go) 处理 |
| 安全 | 远程 kubelet 调用要求集群客户端 CA；kubeconfig 用 Android Keystore 做 AES-GCM 加密；kubelet serving 证书由 `kubernetes.io/kubelet-serving` CSR 签发 |
| 运维 | 维护模式（cordon + drain）、低电 drain、WakeLock/WifiLock、电池白名单、`kubectl top`、节点注册/心跳/重新注册 |
| Android | `compileSdk/targetSdk 35`、`specialUse` 前台服务、运行时通知权限、agent 与 proot 二进制来自 `nativeLibraryDir`、亮/暗外观（默认亮色，可选深色）、系统栏安全区 |

已知限制与物理约束见 [docs/limitations.md](docs/limitations.md)；


## 仓库结构

```
agent/
├── executor/src/
│   ├── brokers/                 # k8s API、OCI registry、文件系统、proot
│   ├── services/foundation/     # 镜像拉取/解包、执行、探针、遥测、
│   │                            # cluster_ip_proxy（Service 名代理）、pull secret、镜像存储
│   ├── services/orchestration/  # 对账、工作负载生命周期、镜像流水线、
│   │                            # 节点注册
│   ├── exposers/                # 虚拟 kubelet HTTPS API
│   └── status_ipc.rs            # 给 APK 用的 Unix socket 状态/遥测
├── node-agent/src/main.rs       # 入口，任务装配
└── spdy-sidecar/main.go         # Go SPDY 服务，负责 exec/attach/port-forward
android/app/src/main/java/com/droidnode/
├── ui/v2/                       # 节点 / Pods / 镜像 / 日志 / 设置 控制台
├── exposers/                    # ForegroundServiceExposer
├── brokers/                     # 进程、电池、网络、wake/wifi 锁、状态 IPC
└── services/                    # 生命周期 + 就绪/退避
deploy/rbac.yaml                 # agent 用的 ServiceAccount + ClusterRole
deploy/registry-mirrors.env.example   # 手机镜像模板 -> 复制成 registry-mirrors.env（已 gitignore）
toolchain.env.example            # 工具链路径模板 -> 复制成 toolchain.env.local（已 gitignore）
docs/                            # 当前能力、限制、构建、实测
docs/images/                     # 文档引用的控制台截图
prototype/                       # 交互原型（HTML，不参与 APK 构建）
```

架构规则（The Standard）：broker 只对接外部系统；foundation 服务只做一件事；
orchestration 服务负责协调；exposer 是薄入口。

## 构建

工具链位置从不写死，来自 gitignore 的 `toolchain.env.local`（模板：[`toolchain.env.example`](toolchain.env.example)）；
没设的项依次回退到环境变量和自动探测。变量表见 [docs/dev-environment.md](docs/dev-environment.md)。

```powershell
# 1) 一次性：告诉脚本工具链在哪
Copy-Item toolchain.env.example toolchain.env.local   # 然后编辑这份副本
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-apk-win.ps1 -CheckToolchain

# 2) Rust agent + proot + APK
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-apk-win.ps1

# 3) Go SPDY sidecar（单独一步；用 PATH 上的 go，或 env 文件里的 GO=）
Push-Location agent\spdy-sidecar
$env:GOOS='android'; $env:GOARCH='arm64'; $env:CGO_ENABLED='0'
go build -trimpath -ldflags '-s -w' -o libdroidnode_spdy.so .
Copy-Item libdroidnode_spdy.so ..\..\android\app\src\main\jniLibs\arm64-v8a\
Pop-Location
```

Linux/macOS 用 `scripts/build-android.sh`，读的是同一个 env 文件。

## 控制面准备

基线：**k3s v1.29.15+k3s1**（go1.23.6）。

1. 应用 [deploy/rbac.yaml](deploy/rbac.yaml)。
2. 为 `kube-system/droidnode-agent` 创建 token，并组装一个 server 指向控制面 LAN IP
   （不能是 `127.0.0.1`）的 kubeconfig。发布 client CA，供“地址 + token”自动加载：
   `kubectl -n kube-system create configmap droidnode-client-ca --from-file=ca.crt=/var/lib/rancher/k3s/server/tls/client-ca.crt --dry-run=client -o yaml | kubectl apply -f -`
3. `/etc/rancher/k3s/config.yaml` 里保持 `egress-selector-mode: disabled`；k3s 的 tunnel
   不包含 DroidNode 的 kubelet。
4. `kubectl top` 需要 metrics-server 带 `--kubelet-insecure-tls`（手机的 serving 证书由
   集群 CA 签发，但 metrics-server 的 CA bundle 可能不含它）。
5. 不需要 `kubelet-certificate-authority` bundle。agent 会申请
   `kubernetes.io/kubelet-serving` CSR 并自动批准（见 [docs/cluster-bootstrap.md](docs/cluster-bootstrap.md)）。

## 安装与使用

控制面只要准备一份凭据，二选一：

```bash
# 方式一：只签一个 token，手机端填地址 + token（CA 由 App 从 /cacerts 自动取）
kubectl -n kube-system create token droidnode-agent --duration=8760h

# 方式二：生成完整 kubeconfig，推到手机导入
sudo env KUBECONFIG=/etc/rancher/k3s/k3s.yaml IP=<control-plane-ip> \
  OUT=/tmp/droidnode-kubeconfig.yaml bash deploy/console/make-kubeconfig.sh
```

在 App 里进入 **设置 → 控制面 → 加入集群**（独立页面，三种方式任选：地址 + token / 导入
kubeconfig 文件 / 粘贴 kubeconfig）。导入成功后会加密保存为 `files/node/config/kubeconfig.enc`；
前台服务只在 agent 运行时生成私有的 `kubeconfig.runtime`。打开 App 即启动节点；
设置页有停止 / 启动 / 重启与维护控制。设置页本身只显示结果（控制面地址、身份、文件路径）。

不要直接导入管理员的 `/etc/rancher/k3s/k3s.yaml`：那是 `system:masters`，而且没法按设备吊销。
401 / 403 的判断与排障表见 [docs/cluster-bootstrap.md](docs/cluster-bootstrap.md#6-常见问题)。

```bash
kubectl get nodes
kubectl apply -f workload.yaml
kubectl logs -f <pod>
kubectl exec <pod> -- sh -c 'id; ls /'
kubectl cp ./file <pod>:/tmp/file
kubectl port-forward <pod> 18080:8080
```

Service 用名称（`http://<svc>`、`<svc>.<ns>`、`<svc>.<ns>.svc`）或数字 ClusterIP 都可以。
Service 与容器端口必须 **≥1024**，且两者应当不同 —— Service 名代理的监听器占用了
`127.10.x.y` 上的 Service 端口。

### 加入节点后：处理 svclb（ServiceLB）

k3s 会为 `LoadBalancer` 类型的 Service（默认 Traefik）起一个 `svclb-*` **DaemonSet**。
它没有 nodeSelector/affinity，所以也会调到手机上；proot 不能监听 80/443 这类特权端口，
Pod 会一直停在 `PrivilegedPort`（0/2）。`droidnode/proot=true` 标签在 k3s 1.29 下
**不会自动排除**，需要给 DaemonSet 打一次 affinity：

```bash
# 1. 找到 svclb DaemonSet（hash 每个集群不同）
kubectl -n kube-system get ds | grep svclb

# 2. 排除带 DroidNode 标签的节点
kubectl -n kube-system patch ds <svclb-ds> --type=merge -p '{
  "spec": {
    "template": {
      "spec": {
        "affinity": {
          "nodeAffinity": {
            "requiredDuringSchedulingIgnoredDuringExecution": {
              "nodeSelectorTerms": [
                {
                  "matchExpressions": [
                    { "key": "droidnode/proot", "operator": "DoesNotExist" }
                  ]
                }
              ]
            }
          }
        }
      }
    }
  }
}'

# 3. 删掉已经在手机上的那个 Pod
kubectl -n kube-system get pods -o wide | grep <手机节点名>
kubectl -n kube-system delete pod <手机上的-svclb-pod>

# 4. 确认：只剩控制面上的 svclb
kubectl -n kube-system get pods -o wide | grep svclb
```

agent 注册时会自动打 `droidnode/proot=true`，所以这条 affinity 对所有手机节点都生效。
如果重装/升级 Traefik，DaemonSet 会重建，需要重新 patch。完全不需要 ServiceLB 时，
也可以在 k3s 安装参数里加 `--disable traefik --disable servicelb`。

## Agent 环境变量

| 变量 | 默认值 | 含义 |
|---|---|---|
| `KUBECONFIG` | — | 运行时 kubeconfig 路径（由 APK 设置） |
| `DROIDNODE_NODE_ID` | `droidnode-<ANDROID_ID>` | 节点名 |
| `DROIDNODE_KUBELET_PORT` | `10255` | kubelet HTTPS 端口 |
| `DROIDNODE_PROOT_PATH` | `nativeLibraryDir/libproot.so` | proot 二进制 |
| `DROIDNODE_SPDY_HELPER` | 同目录 `libdroidnode_spdy.so` | SPDY sidecar |
| `DROIDNODE_LAYERS_DIR` | `<DROIDNODE_DATA_DIR>/layers` | OCI 层缓存 |
| `DROIDNODE_ROOTFS_DIR` | `<DROIDNODE_DATA_DIR>/rootfs` | 镜像 / Pod rootfs |
| `DROIDNODE_DATA_DIR` | APK 设为 `<filesDir>/node` | 节点根目录（socket/volumes） |
| `DROIDNODE_CONFIG_DIR` | `<DROIDNODE_DATA_DIR>/config` | kubeconfig、CA、镜像源/网络配置 |
| `DROIDNODE_KUBELET_CLIENT_CA` | `<DROIDNODE_DATA_DIR>/config/client-ca.crt` | kubelet mTLS 客户端 CA |
| `DROIDNODE_REGISTRY_MIRROR*` | —（未设置即直连上游） | 改写 `docker.io` / `registry.k8s.io` / `quay.io` |
| `RUST_LOG` | `info` | tracing 过滤器 |

镜像地址**不编译进程序、也不进 git**：agent 运行时读
`$DROIDNODE_CONFIG_DIR/registry-mirrors.env`（模板：[`deploy/registry-mirrors.env.example`](deploy/registry-mirrors.env.example)），
这份真实文件已 gitignore。手机端在 **设置 → 节点 → 镜像源** 里直接改（三个主机名输入框，
留空即直连上游），保存后 agent 会重启。

## 文档

- [控制面接入](docs/cluster-bootstrap.md) —— 控制面、kubeconfig、CSR
- [Agent API](docs/api/README.md) —— HTTP 端点、OpenAPI、IPC
- [Kubelet 兼容性](docs/api/compatibility.md) —— kubelet/Kubernetes 1.29 契约与当前结果
- [限制](docs/limitations.md) —— 物理/内核边界
- [手机存储布局](docs/storage-layout.md) —— 节点目录与清除数据
- [开发环境](docs/dev-environment.md) —— 工具链与构建路径
- [开源说明](docs/opensource-compliance.md) —— 协议、第三方组件及其条款

## 许可

MIT —— 上游代码 © 2026 Jamiu Tijani，fork 部分 © 2026 SHIH。
第三方组件及其协议见 [LICENSES/](LICENSES/) 与 [docs/opensource-compliance.md](docs/opensource-compliance.md)。
