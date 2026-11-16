# 控制面与 DroidNode 接入

> 当前环境（2026-11-16）：Debian 13 控制面 `192.168.2.7`，**k3s v1.29.15+k3s1**（内含
> **go1.23.6**）；手机 OnePlus PGJM10 `192.168.2.5`，节点名 `dn-k10`，kubelet `:10255`。

## 1. 控制面规格

| 项 | 建议 |
|---|---|
| 系统 | Debian 12/13 netinst，无桌面 |
| vCPU / 内存 | 2 vCPU / 3 GB（完整 k3s + 调试余量） |
| 磁盘 | 20 GB 薄置备 |
| 网络 | 桥接，手机能直接访问控制面 LAN IP |
| k3s | **v1.29.15+k3s1**（控制面当前实际版本）；traefik/metrics-server 可用 |
| Go（k3s 内置） | go1.23.6，不需要单独安装 |

## 2. 安装 k3s 1.29

当前控制面使用国内镜像安装。若从头装：

```bash
curl -sfL https://rancher-mirror.rancher.cn/k3s/k3s-install.sh | \
  INSTALL_K3S_MIRROR=cn \
  INSTALL_K3S_VERSION=v1.29.15+k3s1 \
  sh -s - server \
  --node-name=k3s-main \
  --tls-san=192.168.2.7 \
  --write-kubeconfig-mode=644
```

当前 `/etc/rancher/k3s/config.yaml`：

```yaml
tls-san:
  - "192.168.2.7"
write-kubeconfig-mode: "644"
egress-selector-mode: disabled
```

> `egress-selector-mode: disabled` 是**必须**的：k3s 1.29 默认的 agent tunnel 只认识
> k3s agent，DroidNode 不是 k3s agent，保持默认会让 apiserver → kubelet 的代理返回
> `502 Bad Gateway`。修改后 `sudo systemctl restart k3s`。

### 镜像加速

系统组件走 `docker.io` / `registry.k8s.io`，国内需要镜像源。当前用 DaoCloud：

```yaml
# /etc/rancher/k3s/registries.yaml
mirrors:
  docker.io:
    endpoint:
      - "https://docker.m.daocloud.io"
```

```bash
sudo systemctl restart k3s
```

模板见 [`deploy/k3s-registries.example.yaml`](../deploy/k3s-registries.example.yaml)；
真实文件 `deploy/k3s-registries.yaml` 已 gitignore，不要提交。

手机端 agent 没有编译进去的镜像地址，运行时读
`$DROIDNODE_CONFIG_DIR/registry-mirrors.env`（默认 `files/node/config/registry-mirrors.env`）
或环境变量。模板：[`deploy/registry-mirrors.env.example`](../deploy/registry-mirrors.env.example)。
App 的 **设置 → 节点 → 镜像源** 直接写这个文件并重启 agent。

## 3. 创建 agent 身份

```bash
kubectl apply -f deploy/rbac.yaml
```

`deploy/rbac.yaml` 包含：

- ServiceAccount `kube-system/droidnode-agent`；
- Node/Pod/Status/Events 权限；
- ConfigMap/Secret/Service 读取；
- PVC/PV/StorageClass（本地 PV）；
- CSR create/delete/approval + `signers` approve；
- `configmaps` 读取，用于 token 加入时自动加载 client CA。

### 生成 kubeconfig（推荐）

```bash
sudo env KUBECONFIG=/etc/rancher/k3s/k3s.yaml \
  IP=192.168.2.7 OUT=/tmp/droidnode-kubeconfig.yaml \
  bash deploy/console/make-kubeconfig.sh
```

脚本会生成 8760h token、控制面 CA、并**把 client CA 写进 kubeconfig 的
`droidnode-client-ca-data`**，同时发布 `kube-system/droidnode-client-ca` ConfigMap，
最后用这份 kubeconfig 跑一次 `kubectl get nodes` 自检。

### 地址 + token

```bash
kubectl -n kube-system create token droidnode-agent --duration=8760h
```

把完整 JWT（约 900 字符，以 `eyJ` 开头、两个点）填进 App。**不要填 k3s 的 node-token**
（`/var/lib/rancher/k3s/server/node-token`），也不要带换行/空格；App 会自动清理粘贴产生的空白。
CA 由 App 从 `/cacerts` 自动获取，也可手填。

三种加入方式：

| 方式 | 控制面要做什么 | 手机端要填什么 |
|---|---|---|
| **地址 + token（推荐）** | 上面的 `create token`，并发布 client CA ConfigMap | 地址 + token；CA 自动 |
| **导入 kubeconfig 文件** | `make-kubeconfig.sh` 生成 yaml | 选文件/粘贴全文 |
| **粘贴 kubeconfig（调试）** | 无 | 控制面 `k3s.yaml` 全文（权限过大，仅排障） |

成功后会写入 `files/node/config/kubeconfig.enc`，重启 agent。设置页只显示结果。

### client CA（token 方式容易漏）

kubelet 拒绝没有客户端证书的 apiserver 请求。优先让 agent 自动从
`kube-system/droidnode-client-ca` 读取；没有 ConfigMap 时手动推：

```bash
adb push client-ca.crt /data/local/tmp/
adb shell "cat /data/local/tmp/client-ca.crt | run-as com.droidnode sh -c 'mkdir -p files/node/config; cat > files/node/config/client-ca.crt'"
```

不配置的后果：节点 Ready，但 `kubectl logs` / `exec` / `port-forward` 报
`client certificate required by DroidNode kubelet`。

## 4. 手机网络与存储

`InternalIP` 自动跳过 Wi-Fi Direct（`p2p*`）、USB 共享（`rndis*`/`usb*`）和 `tun*`。
多网卡时可手动指定：

**设置 → 网络 → 选一张网卡**，写入
`files/node/config/network-preference.env`（`DROIDNODE_NODE_IFACE=wlan0`），并重启 agent。

存储布局见 [`storage-layout.md`](storage-layout.md)；节点数据统一在
`files/node/{config,rootfs,layers,logs,volumes}`。

## 5. 自建 registry（局域网明文 HTTP）

```bash
# files/node/config/registry-mirrors.env 里加一行；每台手机都要推
echo 'DROIDNODE_INSECURE_REGISTRIES=192.168.2.7:5000' > /tmp/registry-mirrors.env
adb push /tmp/registry-mirrors.env /data/local/tmp/
adb shell "cat /data/local/tmp/registry-mirrors.env | run-as com.droidnode sh -c 'mkdir -p files/node/config; cat > files/node/config/registry-mirrors.env'"
```

只在受信局域网使用；公网 registry 走 TLS。未配置时私有 IP 的 registry 默认按 HTTP 拉取
（显式 `DROIDNODE_INSECURE_REGISTRIES` 优先）。

## 6. 401 / 403 排查

| 现象 | 含义 | 处理 |
|---|---|---|
| 401 + 「没有可用凭据」 | 请求没带凭据，或 token 被空格/换行破坏 | 用 `create token` 的完整 JWT；App 会自动去空白 |
| 401 + 「token 被拒绝」 | token 过期或不属于这个集群 | 重新生成 |
| 401 + 「客户端证书被拒」 | 证书过期/不属于该集群 | 换 token |
| 403 | 凭据有效但 RBAC 不足 | 确认 `deploy/rbac.yaml` 已 apply |
| TLS 失败 | CA 不匹配、SAN 不含访问地址 | 检查 `--tls-san` 与 `/cacerts` |

不要直接导入管理员 `/etc/rancher/k3s/k3s.yaml`：`system:masters` 无法按设备吊销。

## 7. metrics-server

`kubectl top` 需要 metrics-server 跳过 kubelet 证书校验：

```bash
kubectl -n kube-system patch deploy metrics-server --type=json \
  -p='[{"op":"add","path":"/spec/template/spec/containers/0/args/-","value":"--kubelet-insecure-tls"}]'
```

## 8. 安装 APK

```powershell
powershell -File scripts\build-apk-win.ps1
powershell -File scripts\adb-install-all.ps1 -Start
```

或手工：

```powershell
adb install -r dist\droidnode-debug-<stamp>.apk
adb shell am start -n com.droidnode/.ui.v2.NodeActivity
```

当前版本 **0.1.55**（versionCode 56）。打开 App 即启动前台服务；设置页可停止/启动/重启、
维护模式、改端口/镜像源。

## 9. 加入后的检查

```bash
kubectl get nodes -o wide
kubectl get node dn-k10 --show-labels
# 期望：droidnode/managed=true, droidnode/proot=true

kubectl apply -f deploy/examples/hello-android.yaml   # 先把 nodeName 改成 dn-k10
kubectl logs <pod> --tail=5
kubectl exec <pod> -- sh -c 'id; uname -a'
kubectl top node dn-k10
```

Service 用名称或 ClusterIP；端口必须 ≥1024，Service 端口与容器端口错开（D3 loopback 代理
占用 `servicePort`）。集群 DNS 当前不可用（coredns 在 proot 上无法 bind 53），使用 Service
名称（D3 hosts）或 ClusterIP。

**svclb 必须排除手机节点**，否则会卡在 `PrivilegedPort`；操作见根目录 README 的
“加入节点后：处理 svclb”章节。

**不要靠改“节点名”重命名 Node**：Kubernetes Node `metadata.name` 不可变，改名字会新建
Node 且旧 Node 不自动删除；旧节点用 `kubectl delete node <旧节点名>` 清理。

## 10. 当前快照

```
$ kubectl get nodes -o wide
NAME                         STATUS   ROLES           VERSION          INTERNAL-IP
dn-k10                       Ready    <none>          v1.29.0          192.168.2.5
k3s-main                     Ready    control-plane   v1.29.15+k3s1   192.168.2.7
```

已知限制见 [`limitations.md`](limitations.md)；兼容性见
[`api/compatibility.md`](api/compatibility.md)。
