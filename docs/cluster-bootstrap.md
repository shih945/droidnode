# 把手机节点加入集群（App 操作流程）

> 覆盖范围：**装 APK → 控制面准备凭据 → App 里配置 → 节点 Ready → 加入后的必要调整**。
> 控制面（k3s / Kubernetes）本身的安装、主节点镜像加速、自建 registry 等不在本文范围，
> 前提是控制面已经可用、你能在上面执行 `kubectl`。
>
> 能力边界见 [`limitations.md`](limitations.md)，节点目录结构见 [`storage-layout.md`](storage-layout.md)。

整体顺序：

```
1. 手机装 APK 并打开
2. 控制面：2.1 建 agent 身份（必做，一次性）
             2.2 给手机一份凭据（二选一：地址+token / kubeconfig 文件）
3. App：设置 → 控制面 → 加入集群
4. 两边确认：kubectl get nodes / App 节点页
5. 按需调整：svclb、metrics-server、镜像源、网卡
```

---

## 1. 手机端：安装并打开 App

### 1.1 安装

```powershell
adb install -r dist\droidnode-debug-<stamp>.apk
adb shell am start -n com.droidnode/.ui.v2.NodeActivity
```

也可以直接点 APK 安装。装完不需要在手机上做任何授权操作（前台服务、WakeLock 都在应用内申请）。

### 1.2 首次打开的预期状态

打开 App 会**自动拉起前台服务**，并在 `files/node/` 下建好 `config / rootfs / layers / logs / volumes`。

此时看到的状态是正常的，不是故障：

| 位置 | 显示 |
|---|---|
| 顶部徽标 | `已停止` |
| 节点页 | `节点已停止` / `前台服务未在运行` |
| 设置 → 控制面 | `未配置`、地址 `—`、身份 `—` |

因为还没有 kubeconfig，agent 起来后找不到集群会立刻退出（日志里是 `failed to create k8s client — is KUBECONFIG set?`）。

### 1.3 （建议）先做两件事，避免息屏掉线

**设置 → 电源与后台**：

| 开关 | 作用 |
|---|---|
| **电池优化白名单** | 不加白名单，系统可能在息屏后清掉前台服务，节点随机 `NotReady` |
| **开机自启** | 重启手机后自动拉起前台服务，不用每次手动开 App |

---

## 2. 控制面：一次性准备

### 2.1 必做：创建 agent 身份（RBAC）

```bash
kubectl apply -f deploy/rbac.yaml
kubectl -n kube-system get sa droidnode-agent     # 应能看到该 SA
```

它会建 **ServiceAccount + ClusterRole + ClusterRoleBinding**（`kube-system/droidnode-agent`），
权限就是 agent 真实需要的那几条：Node 注册/心跳、Pod 读取/状态/删除、ConfigMap/Secret 读取、
Service + EndpointSlice 列表（Service 名代理服务发现）、本地 PV 供给、自签 kubelet 证书、写事件。
每条权限对应哪段代码，写在 [`deploy/rbac.yaml`](../deploy/rbac.yaml) 的注释里。

注意事项：

- **幂等，每个集群只做一次**；不需要每加一台手机重复执行。
- **升级 App/agent 后要重新 apply 一次**：`apply` 会整体替换 ClusterRole 的 `rules`，文件里新增的权限只有重跑才会生效。
  漏掉的后果不是"连不上"，而是更难查的现象：
  - 缺 `endpointslices` → 节点 `Ready`、Pod 正常，但**服务间按 Service 名互访全部失败**（日志只告警一次）；
  - 缺 `pods` / `pods/status` → `403`，Pod 卡在 `ContainerCreating` 不推进。
- 没有这个 SA 时，`make-kubeconfig.sh` 会直接报错并提示你先 apply。

### 2.2 二选一：给手机一份凭据

两种方式都只认 **`kube-system/droidnode-agent` 这个身份**，不要用管理员 `/etc/rancher/k3s/k3s.yaml`
（`system:masters` 无法按设备吊销，只能排障时临时用）。

#### 2.2.1 地址 + token（推荐，适合多台手机）

```bash
kubectl -n kube-system create token droidnode-agent --duration=8760h
```

复制输出的完整 JWT（约 900 字符，`eyJ` 开头、含两个点）。
**不要**填 k3s 的 `node-token`（`/var/lib/rancher/k3s/server/node-token`），它跟 API 凭据无关。

这一步还需要 **client CA**，否则后面 `kubectl logs / exec / port-forward` 会被手机的 kubelet 拒绝
（报 `client certificate required by DroidNode kubelet`）。两种给法：

- **让 agent 自动读**：把 CA 发布成 ConfigMap（推荐，一条命令）：

  ```bash
  kubectl -n kube-system create configmap droidnode-client-ca \
    --from-file=ca.crt=/var/lib/rancher/k3s/server/tls/client-ca.crt \
    --dry-run=client -o yaml | kubectl apply -f -
  ```

- **手动推到手机**（没有 ConfigMap 时）：

  ```bash
  adb push client-ca.crt /data/local/tmp/
  adb shell "cat /data/local/tmp/client-ca.crt | run-as com.droidnode sh -c 'mkdir -p files/node/config; cat > files/node/config/client-ca.crt'"
  ```

CA 同理也可在 App 的加入页里手填（`CA（选填）`），留空则自动从控制面 `/cacerts` 获取。

#### 2.2.2 导入 kubeconfig 文件（适合单台、离线传文件）

在控制面执行：

```bash
sudo env KUBECONFIG=/etc/rancher/k3s/k3s.yaml \
  IP=192.168.2.7 OUT=/tmp/droidnode-kubeconfig.yaml \
  bash deploy/console/make-kubeconfig.sh
```

脚本做四件事：

1. 检查 SA 是否存在（没有就提示先做 2.1，或加 `--with-rbac` 一并 apply）；
2. 生成 **8760h token** 并写进 kubeconfig（加 `--long-lived` 可生成不过期、可多机复用的 token Secret）；
3. 把 `client-ca-data` 写进 kubeconfig，并把 CA 发布成 `kube-system/droidnode-client-ca` ConfigMap；
4. 用**这份 kubeconfig 自己**跑一次 `kubectl get nodes` 自检——自检不过就不要往手机上导。

把 `/tmp/droidnode-kubeconfig.yaml` 传到手机（`adb push ... /sdcard/Download/`，或微信/文件传输）。
`IP` 必须是**手机能访问到的控制面 LAN 地址**，不能是 `127.0.0.1`、docker 网段、`192.168.137.x`（USB 共享）这类地址。

---

## 3. App：配置并加入集群

入口：**设置 → 控制面 → `加入集群`**

页面上的「方式」有三种，按你在 2.2 选的那条走：

| 方式 | 控制面要做什么 | 手机上填什么 |
|---|---|---|
| **地址 + token**（推荐） | 2.2.1 的 `create token` + client CA ConfigMap | 地址、token；CA 留空 |
| **导入 kubeconfig 文件** | 2.2.2 生成 yaml | 选文件后点「导入并加入」 |
| **粘贴 kubeconfig** | 无 | 粘贴全文（含管理员证书，仅排障用） |

### 3.1 方式 A：地址 + token

1. 方式选 **地址 + token**；
2. `控制面地址` 填 `https://192.168.2.7:6443`（不写 `https://` 会自动补；结尾 `/` 会去掉）；
3. `token` 粘贴 2.2.1 拿到的 JWT（**粘贴里带的空格/换行会自动清理**）；
4. `CA（选填）` 留空（自动从 `/cacerts` 取），失败时再手填；
5. 点 **`加入`**。

### 3.2 方式 B：导入 kubeconfig

1. 方式选 **导入 kubeconfig 文件**；
2. 点选择文件，选手机上那份 yaml（App 会显示读取到的字节数）；
3. 点 **`导入并加入`**。

### 3.3 提交后发生什么

- App 先做 **TLS + 身份校验**（会请求控制面）；**校验不过不会覆盖现有配置**，错误直接显示在输入框下方；
- 校验通过才写入 `files/node/config/kubeconfig.enc`（Android Keystore 加密），然后自动重启 agent；
- 加入页关闭，设置页 `控制面` 变成 **`已加入`**，并显示地址与身份（`token` / `客户端证书`）。

可选：加入前先点 **`连接测试`**，只验证地址+凭据是否可用，不落盘。

### 3.4 加入后 App 里的正确状态

| 位置 | 期望 |
|---|---|
| 设置 → 控制面 | `已加入` + `https://<控制面>:6443` |
| 顶部徽标 / 节点页 | `运行中` / `节点运行中` |
| 节点页 `kubelet` | `0.0.0.0:10255`（或你改过的端口） |
| 节点页 `本机 IP` | 与手机实际 LAN 地址一致 |
| 设置 → 运行控制 → `agent 上报` | `已连接 · 等待状态` 或已有 Pod 计数 |

---

## 4. 控制面：确认节点已加入

```bash
kubectl get nodes -o wide
kubectl get node <节点名> --show-labels
# 期望标签：droidnode/managed=true, droidnode/proot=true
```

冒烟（可选）：把 [`deploy/examples/hello-android.yaml`](../deploy/examples/hello-android.yaml)
里的 `nodeName` 改成你的节点名后 apply，然后 `kubectl logs <pod> --tail=5`、`kubectl exec <pod> -- sh -c 'id; uname -a'`。

---

## 5. 加入后按需调整

### 5.1 排除 svclb（集群里有 `LoadBalancer` / ServiceLB 时必须）

k3s 会给每个 `LoadBalancer` Service 部署 `svclb-*` DaemonSet，它默认也会调度到手机上，然后卡在
`PrivilegedPort`（proot 里 bind 不了 80/443）。一次性把 DaemonSet 打上 nodeAffinity 排除所有手机节点，
命令见 [README.zh-CN.md](../README.zh-CN.md) 的“加入节点后：处理 svclb（ServiceLB）”一节
（英文版 [README.md](../README.md) 同一位置，标题为 “After joining: handle svclb”），
排的是带 `droidnode/proot=true` 标签的节点。

### 5.2 metrics-server（要用 `kubectl top` 时）

```bash
kubectl -n kube-system patch deploy metrics-server --type=json \
  -p='[{"op":"add","path":"/spec/template/spec/containers/0/args/-","value":"--kubelet-insecure-tls"}]'
```

### 5.3 拉镜像慢 / 拉不到（国内网络常见）

**设置 → 节点 → 镜像源**，填 docker.io / registry.k8s.io / quay.io 的镜像地址（留空 = 直连上游），
点 **`保存并重启 agent`**。写入的是 `files/node/config/registry-mirrors.env`。
不配的话表现是 Pod 长时间 `ContainerCreating`，随后 `ErrImagePull`。

### 5.4 多网卡 / 开过热点

**设置 → 网络** 选一张真实 LAN 网卡（`自动选择` 会跳过 Wi-Fi Direct `p2p*`、USB 共享 `rndis*`/`usb*`、`tun*`）。
选错了的现象是：节点 `Ready`，但日志/Service 从集群侧不通。

### 5.5 日常运维开关（都在 设置 内）

| 位置 | 用途 |
|---|---|
| 运行控制 → **维护模式** | 先 cordon 再停本机容器，关闭后恢复调度 |
| 运行控制 → **重启 agent** | 重读 kubeconfig 与节点配置；运行中的容器会重建 |
| 代理进程 → **停止 agent / 启动 agent** | 临时让节点下线，不影响已安装的 App |
| 节点 → **保存并重启 agent** | 改完节点名/端口/镜像源/日志级别后必须点这个才生效 |

---

## 6. 常见问题

| 现象 | 原因 | 处理 |
|---|---|---|
| 设置页一直 `未配置`、节点页 `已停止` | 还没加入集群 / agent 没起来 | 走第 3 步；`设置 → 代理进程 → 启动 agent` |
| 加入时 **401「没有可用凭据」** | 没填凭据，或 token 被换行/空格破坏 | 用 `create token` 的完整 JWT（App 会自动去空白） |
| 加入时 **401「token 被拒绝」** | token 过期/不属于该集群 | 重新 `create token` |
| 加入时 **403** | 凭据有效但 RBAC 不足 | 确认 2.1 已 apply，且是升级后重新 apply 过的版本 |
| 加入时 **TLS 校验失败** | CA 不匹配，或控制面证书 SAN 不含你填的地址 | 用控制面实际可达的 IP；检查控制面 `tls-san` |
| 节点 `Ready`，但 `kubectl logs/exec/port-forward` 报 `client certificate required` | 手机没有 client CA | 发布 `droidnode-client-ca` ConfigMap（2.2.1）或手动推 `client-ca.crt` |
| Pod 长时间 `ContainerCreating` → `ImagePullBackOff` | 镜像拉不动（网络/镜像源），或镜像**没有 arm64 变体** | 配镜像源（5.3）；`describe` 里的报错会列出该镜像实际存在的架构 |
| Pod 都 `Running`，但服务间按 Service 名互访失败（页面显示 unavailable） | 缺 `endpointslices` 权限，Service 名代理拿不到后端 | 重新 `apply -f deploy/rbac.yaml`；日志会提示缺哪条权限 |
| Pod 报 `PortConflict` / `port <n> is already in use by another process` | 端口被同节点其它 Pod 或遗留进程占用（共享网络栈） | 错开端口；遗留进程可重启 agent 清理 |
| 想改节点名 | Node 的 `metadata.name` 不可变，改名等于新建 Node | 改完手动 `kubectl delete node <旧节点名>` |
| 节点过一阵就 `NotReady` | 系统把前台服务清掉了 | 电池优化白名单 + 开机自启（1.3） |

---

## 7. 重置或下线一个节点

1. **设置 → 代理进程 → 停止 agent**（节点立即从集群掉线，App 保留）；
2. 需要彻底重置就在系统里**清除应用数据**——kubeconfig、镜像、rootfs、日志、设置全部删除，等同全新节点；
3. 控制面上清理 Node 对象：

   ```bash
   kubectl delete node <节点名>
   ```

清理后再走第 3 步重新加入即可。

---

## 8. 最容易踩的三个坑

1. **`deploy/rbac.yaml` 变了就要重跑**。它是集群级的一次性组件，但升级 agent 后必须重新 `apply`，
   否则新增权限不生效——症状往往不是 403，而是"节点和 Pod 都正常，服务之间却互访不通"。
2. **client CA 不配，`logs`/`exec`/`port-forward` 一定失败**。用地址 + token 加入时最容易漏，
   一条 `kubectl create configmap droidnode-client-ca ...` 就能长期解决（见 2.2.1）。
3. **端口必须 ≥1024，且同一台手机上不能重复**。所有容器共享设备的网络栈；Service 端口还要和容器端口
   错开（Service 名代理会占用 `servicePort` 做 loopback 代理）。

