# Agent 运行配置

agent 的运行时配置来自两个地方：**环境变量**（App 启动 agent 时注入）和
**配置文件**（`$DROIDNODE_CONFIG_DIR/` 下，部分可以从设置页修改）。同名项以环境变量为准。

## 1. 环境变量

| 变量 | 默认值 | 含义 |
|---|---|---|
| `KUBECONFIG` | — | 运行时 kubeconfig 路径（由 App 注入） |
| `DROIDNODE_NODE_ID` | `droidnode-<ANDROID_ID>` | 节点名 |
| `DROIDNODE_KUBELET_PORT` | `10255` | kubelet HTTPS 端口 |
| `DROIDNODE_PROOT_PATH` | `nativeLibraryDir/libproot.so` | proot 二进制 |
| `DROIDNODE_SPDY_HELPER` | 同目录的 `libdroidnode_spdy.so` | SPDY sidecar |
| `DROIDNODE_LAYERS_DIR` | `<DROIDNODE_DATA_DIR>/layers` | OCI 层缓存 |
| `DROIDNODE_ROOTFS_DIR` | `<DROIDNODE_DATA_DIR>/rootfs` | 镜像 / Pod rootfs 缓存 |
| `DROIDNODE_DATA_DIR` | App 设为 `<filesDir>/node` | 节点数据根目录（socket、卷） |
| `DROIDNODE_CONFIG_DIR` | `<DROIDNODE_DATA_DIR>/config` | kubeconfig、CA、镜像源与网卡配置 |
| `DROIDNODE_KUBELET_CLIENT_CA` | `<DROIDNODE_DATA_DIR>/config/client-ca.crt` | kubelet mTLS 的客户端 CA |
| `DROIDNODE_REGISTRY_MIRROR` | 不设 = 直连上游 | `docker.io` 的镜像地址 |
| `DROIDNODE_REGISTRY_MIRROR_K8S` | 不设 = 直连上游 | `registry.k8s.io` 的镜像地址 |
| `DROIDNODE_REGISTRY_MIRROR_QUAY` | 不设 = 直连上游 | `quay.io` 的镜像地址 |
| `RUST_LOG` | `info` | tracing 过滤器 |

三个镜像地址也可以只写一个前缀通用的 `DROIDNODE_REGISTRY_MIRROR*` 系列；它们的值**只接受
主机名**（可带 `:port`），不接受 `http://` 前缀或路径。

## 2. 配置文件

| 文件 | 谁写 | 内容 |
|---|---|---|
| `registry-mirrors.env` | 设置 → 节点与网络 → 镜像源 | 三个镜像地址；保存后重启 agent 生效 |
| `network-preference.env` | 设置 → 节点与网络 → 网卡 | 选定的网卡（`DROIDNODE_NODE_IFACE`） |
| `kubeconfig.enc` | 加入集群页 | 加密保存的 kubeconfig |
| `kubeconfig.runtime` | 前台服务 | agent 运行时临时解密的明文 kubeconfig，停止即删除 |

文件布局与清除数据的影响见 [storage-layout.md](storage-layout.md)。

## 3. 读取顺序

1. 进程环境变量（App 启动 agent 时注入，优先级最高）；
2. `$DROIDNODE_CONFIG_DIR/` 下的配置文件；
3. 都没有时用上表的默认值（镜像地址为空即直连上游）。

## 4. 相关文档

- [把手机节点加入集群](cluster-bootstrap.md) —— 接入流程与排障
- [手机存储布局](storage-layout.md) —— 节点目录、配置文件的落盘位置
- [限制](limitations.md) —— 私有 registry 的 HTTP/HTTPS 规则
