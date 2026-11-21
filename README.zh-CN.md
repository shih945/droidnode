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

## 安装与使用

安装 APK 后，在 **设置 → 集群连接 → 加入集群** 里填 token 或导入 kubeconfig 即可接入集群。

控制面侧的准备（签发 token、生成 kubeconfig、App 把凭据存在哪、为什么不要导入管理员
kubeconfig、401/403 排障）都在接入指南里：

→ **[docs/cluster-bootstrap.md](docs/cluster-bootstrap.md)**

打开 App 即启动节点；设置页有停止 / 启动 / 重启与维护控制。

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

## 文档

- [控制面接入](docs/cluster-bootstrap.md) —— 控制面、kubeconfig、CSR
- [Agent API](docs/api/README.md) —— HTTP 端点、OpenAPI、IPC
- [Kubelet 兼容性](docs/api/compatibility.md) —— kubelet/Kubernetes 1.29 契约与当前结果
- [限制](docs/limitations.md) —— 物理/内核边界
- [手机存储布局](docs/storage-layout.md) —— 节点目录与清除数据
- [开发环境](docs/dev-environment.md) —— 工具链与构建路径
- [Agent 运行配置](docs/agent-config.md) —— 环境变量与配置文件
- [开源说明](docs/opensource-compliance.md) —— 协议、第三方组件及其条款

## 许可

MIT —— 上游代码 © 2026 Jamiu Tijani，fork 部分 © 2026 SHIH。
第三方组件及其协议见 [LICENSES/](LICENSES/) 与 [docs/opensource-compliance.md](docs/opensource-compliance.md)。
