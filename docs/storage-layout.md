# 手机节点本地存储布局

节点所有运行数据统一在应用私有目录：

```
/data/user/0/com.droidnode/files/node/
  config/    kubeconfig、CA、镜像源/网络配置
  rootfs/    解包后的镜像 + rootfs/pods/<uid>/<container>/
  layers/    压缩层缓存
  logs/      容器日志
  volumes/   本地 PV / emptyDir 数据
  agent.sock  App ↔ agent 的 IPC socket（运行时）
```

`files/node` 之外，应用私有数据只有 `shared_prefs/`（界面偏好、节点名、端口等）和
`cache/`、`code_cache/`（可随时丢弃的临时目录）。它们同样属于 Android 的“应用数据”，
**设置 → 应用 → DroidNode → 存储 → 清除数据** 会一并删除。

## 目录明细

| 路径 | 类别 | 内容 | 清空后 |
|---|---|---|---|
| `node/config/kubeconfig.enc` | 配置 | 加密后的 kubeconfig（控制面地址 + token/证书） | 需要重新导入/配主节点 |
| `node/config/kubeconfig.runtime` | 配置 | agent 运行期明文 kubeconfig（服务停止时删除） | 自动重建 |
| `node/config/client-ca.crt` | 配置 | apiserver 客户端证书 CA（`kubectl logs/exec/port-forward` 需要）；token 加入时 agent 会从 `kube-system/droidnode-client-ca` ConfigMap 自动缓存 | 没有 ConfigMap 时需与 kubeconfig 一起导入 |
| `node/config/kubelet-ca.crt/.key/.pub` | 配置 | 设备本地兜底 CA | 首次启动自动重建 |
| `node/config/kubelet-serving.crt/.key/.ca` | 配置 | 集群签发的 kubelet serving 证书及集群 CA 指纹 | 首次启动重新 CSR |
| `node/config/registry-mirrors.env` | 配置 | Docker/K8s/Quay 镜像源 | 需在设置页重新填 |
| `node/config/network-preference.env` | 配置 | 选定的网卡（`DROIDNODE_NODE_IFACE`） | 自动选择 |
| `node/rootfs/<registry>/<repo>/<ref>/` | 镜像 | 解包后的镜像 rootfs（含 `.droidnode_image_meta.json`、`.droidnode_ready`） | 镜像列表清空，按需重新拉取 |
| `node/rootfs/pods/<uid>/<container>/rootfs/` | Pod | 每个 Pod/容器独立可写 rootfs 拷贝 | 调度过来的 Pod 数据清空 |
| `node/rootfs/pods/<uid>/volumes/` | Pod | emptyDir / 本地卷拷贝 | 同上 |
| `node/layers/<digest>` | 镜像 | 压缩层缓存，删除镜像时按引用回收 | 清空 |
| `node/logs/<namespace>/<pod>/<container>.log` | 日志 | `kubectl logs` / 日志页读取的文件（`.log.1` 为轮转） | 清空 |
| `node/volumes/pv/` | 存储 | 本地静态 PV（`droidnode-local`） | 清空 |
| `node/agent.sock` | 运行时 | IPC socket，进程退出/清数据后消失 | 自动重建 |
| `shared_prefs/droidnode_console.xml` | 配置 | 节点名、kubelet 端口、日志级别、界面偏好 | 恢复默认，需要重新配置 |
| `cache/`、`code_cache/tmp/` | 临时 | proot/TMPDIR 临时文件 | 自动重建 |
| `/sdcard/Download/droidnode-*` | 外部残留 | 手工 push 的旧 APK / kubeconfig | **不会被应用清除**，可手动删 |

当前一次实测占用（dn-k10）：

| 目录 | 大小 |
|---|---:|
| `node/rootfs` | ≈ 6.8 GB |
| `node/layers` | ≈ 249 MB |
| `node/logs` | ≈ 10 MB |
| `node/config` | ≈ 44 KB |

## 一键清空 = 全新空节点

Android 的“清除数据”会删除整个应用私有目录，因此会同时清掉：

- kubeconfig、client CA、kubelet CA/serving 证书；
- 全部镜像 rootfs、层缓存、Pod rootfs/卷；
- 全部日志；
- 节点名/端口/镜像源等 SharedPreferences；
- 运行时 socket 和临时文件。

APK 的 native 库（`libnode_agent.so`、`libproot.so`）不在应用数据里，不会被清除；
下次启动应用会重新建目录、显示加入/导入页面，相当于一台空节点。

## 重新配置一台空节点

1. 控制面生成 kubeconfig（新脚本会带 client CA）：

   ```bash
   sudo IP=192.168.2.7 OUT=/tmp/droidnode-kubeconfig.yaml \
     bash deploy/console/make-kubeconfig.sh
   ```

2. 导入 App（设置 → 控制面 → 导入 kubeconfig），或直接推文件：

   ```bash
   adb push droidnode-kubeconfig.yaml /data/local/tmp/
   adb shell "cat /data/local/tmp/droidnode-kubeconfig.yaml | \
     run-as com.droidnode sh -c 'mkdir -p files/node/config; cat > files/node/config/kubeconfig'"
   ```

3. 填镜像源（设置页）或推文件：

   ```bash
   adb shell "cat /data/local/tmp/registry-mirrors.env | \
     run-as com.droidnode sh -c 'mkdir -p files/node/config; cat > files/node/config/registry-mirrors.env'"
   ```

4. 重启 agent / 启动服务，节点会以空镜像仓库重新注册。

## 手工清理（不依赖系统设置页）

```bash
# 完全清空（等价于系统“清除数据”，会退出应用）
adb shell pm clear com.droidnode

# 或者只清节点数据、保留界面偏好
adb shell run-as com.droidnode rm -rf files/node
```

`/sdcard/Download/droidnode-*` 是外部残留，需要单独删；应用不会读它们。
