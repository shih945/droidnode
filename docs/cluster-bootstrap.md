# 控制面与 DroidNode 接入

> 当前环境：Debian 控制面 `192.168.2.7`，k3s `v1.36.5+k3s1`；手机 `192.168.2.9`，
> 节点 `droidnode-37e62e0fe8ce0057`，kubelet `:10255`。

## 1. 控制面规格

| 项 | 建议 |
|---|---|
| 系统 | Debian 12/13 netinst，无桌面 |
| vCPU / 内存 | 2 vCPU / 3 GB（完整 k3s + 调试余量） |
| 磁盘 | 20 GB 薄置备 |
| 网络 | 桥接，手机能直接访问控制面 LAN IP |
| k3s | v1.36.x；完整组件（traefik/metrics-server）可用 |

## 2. 安装 k3s

```bash
curl -sfL https://rancher-mirror.rancher.cn/k3s/k3s-install.sh | \
  INSTALL_K3S_MIRROR=cn sh -s - server \
  --node-name=k3s-main \
  --tls-san=192.168.2.7 \
  --node-ip=192.168.2.7 \
  --flannel-iface=ens33 \
  --write-kubeconfig-mode 644
```

`/etc/rancher/k3s/config.yaml` 当前只保留：

```yaml
egress-selector-mode: disabled
```

不要恢复 `kubelet-certificate-authority`：A16 之后 kubelet serving 证书由集群 CA 签发，
k3s 默认校验即可。

### 镜像加速

系统组件镜像走 `docker.io` / `registry.k8s.io`，国内需要镜像源。**镜像地址不进仓库**：

```bash
# 一次性：从模板生成本地配置，填入自己的 endpoint
cp deploy/k3s-registries.example.yaml deploy/k3s-registries.yaml
$EDITOR deploy/k3s-registries.yaml

# 装到控制面
sudo install -m 644 deploy/k3s-registries.yaml /etc/rancher/k3s/registries.yaml
sudo systemctl restart k3s
```

`deploy/k3s-registries.yaml` 已在 `.gitignore` 里（模板见 [deploy/k3s-registries.example.yaml](../deploy/k3s-registries.example.yaml)）：
镜像地址往往和付费账号绑定，属于部署参数，不该出现在 git 历史里。

手机端同理：agent **没有编译进去的镜像地址**，运行时从
`$DROIDNODE_DATA_DIR/registry-mirrors.env` 或环境变量读取（模板见
[deploy/registry-mirrors.env.example](../deploy/registry-mirrors.env.example)，真正的文件同样已 gitignore）。
在 App 里用 **设置 → 节点 → 镜像源** 改这三个主机名最省事：保存会写同一个文件并重启 agent；
设置页底部那行显示的是 agent 自报的**生效值**（`registry mirrors configured …` 启动日志），
没配置时显示「未配置 · 直连上游」。

## 3. 创建 agent 身份

```bash
kubectl apply -f deploy/rbac.yaml
```

`deploy/rbac.yaml` 包含：

- ServiceAccount `kube-system/droidnode-agent`；
- Node/Pod/Status/Events 权限；
- ConfigMap/Secret/Service 读取；
- PVC/PV/StorageClass（B10 本地 PV）；
- CSR create/delete/approval + `signers` `approve`（A16）。

创建 token kubeconfig：**用脚本，一条命令**（server 自动取 LAN IP，生成后自检）：

```bash
sudo bash deploy/console/make-kubeconfig.sh
# 或指定地址：sudo IP=192.168.2.7 OUT=/tmp/droidnode.yaml bash deploy/console/make-kubeconfig.sh
```

脚本会 apply `deploy/rbac.yaml`、创建 8760h 的 token、用控制面的 CA 组装 kubeconfig，
**并用这份 kubeconfig 真的跑一次 `kubectl get nodes`** —— 通过才说明手机端能连。
手工版等价于：

```bash
kubectl -n kube-system create token droidnode-agent --duration=8760h
# cluster.server=https://<LAN IP>:6443, user.token=<上面的 token>,
# cluster.certificate-authority-data=<控制面 /etc/rancher/k3s/k3s.yaml 里的那一行>
```

手机端从 **设置 → 控制面 → 加入集群** 进入一个独立页面，三种方式任选：

| 方式 | 控制面要做什么 | 手机端要填什么 |
|---|---|---|
| **地址 + token（推荐）** | `kubectl -n kube-system create token droidnode-agent --duration=8760h` | 控制面地址 + token；CA 自动从 `/cacerts` 取（也可手填） |
| **导入 kubeconfig 文件** | `make-kubeconfig.sh` 生成 yaml | 选文件（或粘贴全文） |
| **粘贴 / 管理员 kubeconfig（调试）** | 无 | 粘贴 `/etc/rancher/k3s/k3s.yaml` 全文（会提示权限过大） |

页面会先验证（TLS + 身份）再落盘：验证不通过不会覆盖现有配置，也不会重启 agent。
验证通过后写入 `files/kubeconfig.enc`、重启 agent；设置页只显示结果（控制面地址、身份、文件路径）和入口。

> CA 能不能不填？能，而且 k3s agent 就是这么做的 —— 它也不用你填 CA，只是**自己从
> `/cacerts` 取**（先不校验证书连上去，拿到 CA 后再用它校验真正的连接）。我们做的是同一件事，
> 所以保留了手动填 CA 的入口，并在自动取回时显示 SHA256 指纹供核对。

### 手机端还需要两份文件（很容易漏）

加入集群只解决"能连上控制面"。下面两份不推，节点会 Ready 但用起来像"起不来"：

| 文件 | 从哪来 | 不推的后果 |
|---|---|---|
| `files/client-ca.crt` | 控制面 `/var/lib/rancher/k3s/server/tls/client-ca.crt` | kubelet 拒绝 apiserver 的请求：`kubectl logs` / `exec` / `port-forward` 报 `client certificate required by DroidNode kubelet` |
| `files/registry-mirrors.env` | [`deploy/registry-mirrors.env.example`](../deploy/registry-mirrors.env.example) 填好后 | 国内直连 `docker.io` 失败，Pod 卡在 `ErrImagePull` |

```bash
# 两份都这样推（debug 包允许 run-as）
adb push client-ca.crt /data/local/tmp/
adb shell "cat /data/local/tmp/client-ca.crt | run-as com.droidnode sh -c 'cat > files/client-ca.crt'"
adb push deploy/registry-mirrors.env /data/local/tmp/
adb shell "cat /data/local/tmp/registry-mirrors.env | run-as com.droidnode sh -c 'cat > files/registry-mirrors.env'"
# 推完在 App 里重启 agent
```

`make-kubeconfig.sh` 会把 `client-ca.crt` 一起产出来，并在结尾打印这几条 adb 命令。

### 手机要和控制面在同一个网段

`InternalIP` 会自动跳过 Wi-Fi Direct（`p2p*`）、USB 共享（`rndis*`/`usb*`）和 `tun*` 这类
集群到不了的网卡（agent 侧 `net_util::primary_ipv4`）。但多网卡时自动判断仍可能猜错，
所以 App 里可以手动指定：

**设置 → 网络 → 选一张网卡**（`自动（推荐）` / `wlan0 · 192.168.2.4` / `p2p-…`）

- 选中后写 `files/network-preference.env`（`DROIDNODE_NODE_IFACE=wlan0`），并自动重启 agent。
- agent 启动时按网卡名解析地址（也接受直接写 IP）；如果这张网卡当前没有地址（例如 Wi-Fi 关了），
  会回退到自动选择并打一条告警 `preferred interface has no IPv4 address right now`。
- 影响 `InternalIP`、`podIP`、EndpointSlice 和 `kubectl logs/exec` 的目标地址。
- Wi-Fi Direct / USB 共享的地址会在列表里标「集群不可达」，仅供排障。


### 自建 registry（局域网明文 HTTP）

想在手机上跑自己构建的镜像，可以在控制面起一个 `registry:2`（`hostNetwork: true`，
端口 5000），然后让 agent 对该地址走 HTTP：

```bash
# files/registry-mirrors.env 里加一行；每台手机都要推
echo 'DROIDNODE_INSECURE_REGISTRIES=192.168.2.7:5000' >> /tmp/registry-mirrors.env
adb push /tmp/registry-mirrors.env /data/local/tmp/
adb shell "cat /data/local/tmp/registry-mirrors.env | run-as com.droidnode sh -c 'cat > files/registry-mirrors.env'"
```

> 只在受信局域网里这么用；公网 registry 请走 TLS。
> 未配置的手机会报 `HTTP error ... (https://<ip>:5000/v2/...)`，Pod 卡在 `ErrImagePull`。

### 不要直接导入 `/etc/rancher/k3s/k3s.yaml`

那份是**管理员证书**（`system:masters`）kubeconfig，不是给节点用的：手机拿到的是整个集群的
root 权限，且 token 无法单独吊销。请用上面的脚本生成 `kube-system/droidnode-agent` 的 token 版本。

### 导入后报 401 / 403 怎么区分

| 现象 | 含义 | 处理 |
|---|---|---|
| 401 + 「没有可用凭据」 | 请求完全没带凭据。多见于导入了管理员证书 kubeconfig，而私钥/证书没被读出来 | 换 token kubeconfig（上面的脚本） |
| 401 + 「token 被拒绝」 | token 过期，或不属于这个集群（集群重建过） | 重新执行脚本生成 |
| 401 + 「客户端证书被拒」 | 证书过期 / 不属于该集群 | 换 token kubeconfig |
| 403 | 凭据有效但 RBAC 不足 | 确认 `deploy/rbac.yaml` 已 apply，token 属于 `kube-system/droidnode-agent` |
| TLS 失败 | CA 与服务器证书不匹配、SAN 不含该 IP | 检查 `--tls-san` 是否包含手机访问用的地址 |

设置页的测试结果现在会带上「身份：token / 客户端证书 / 匿名」，401 时按上面四种情况给出具体提示。

## 4. metrics-server

`kubectl top` 需要 metrics-server 跳过手机自签/集群 CA 校验：

```bash
kubectl -n kube-system patch deploy metrics-server --type=json \
  -p='[{"op":"add","path":"/spec/template/spec/containers/0/args/-","value":"--kubelet-insecure-tls"}]'
```

如果 patch 失败，直接 `kubectl -n kube-system edit deploy metrics-server`，在 args 中加
`--kubelet-insecure-tls`。

## 5. 安装 APK

```powershell
adb install -r -d android\app\build\outputs\apk\debug\app-debug.apk
adb shell am start -n com.droidnode/.ui.v2.NodeActivity
```

打开 App 即启动前台服务；设置页可停止/启动/重启 agent、切换维护模式。

## 6. 验收

```bash
kubectl get nodes
# droidnode-37e62e0fe8ce0057   Ready   <none>   ...

kubectl apply -f deploy/examples/hello-android.yaml   # 先把 nodeName 改成手机节点名
kubectl logs <pod>
kubectl exec <pod> -- sh -c 'id; uname -a'
kubectl top node droidnode-37e62e0fe8ce0057
```

Service 验证：给一个监听 ≥1024 端口的 Pod 加 selector Service，然后分别访问
`ClusterIP:port` 和 Service 名称（`svc` / `svc.ns` / FQDN）。两者都应返回后端响应。

注意：

- Service/容器端口必须 ≥1024；
- D3 的 Service 名称代理占用 `servicePort`，容器不要 bind `0.0.0.0:同一端口`；
- 集群 DNS 不可用，使用 Service 名称（D3 hosts）或 ClusterIP。

## 7. 当前快照

```
$ kubectl get nodes
NAME                         STATUS   ROLES           VERSION
droidnode-37e62e0fe8ce0057   Ready    <none>          v1.29.0
k3s-main                     Ready    control-plane   v1.36.5+k3s1
```

已知限制见 [limitations.md](limitations.md)。
