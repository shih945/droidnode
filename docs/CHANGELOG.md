# 变更记录

> 只保留与当前代码对应的里程碑摘要；逐次调试记录已清理。
> 最新状态：**2026-11-13，97 / 97**。

## 2026-11-10 ~ 11-11：基础节点与镜像

- Android APK 前台服务拉起 Rust `node-agent`；Rust 注册 Node、实现 HTTPS kubelet API。
- OCI 拉取/校验/解层、per-Pod rootfs、proot 启动容器；基础 Pod 状态、日志、删除。
- 镜像源改写、`imagePullPolicy`、`imagePullSecrets`、镜像列表/删除。
- 多容器/initContainer、卷（emptyDir/hostPath/configMap/secret）、探针、restartPolicy、事件。
- 节点容量/心跳、`kubectl top` 的 `/metrics/resource` + `/stats/summary`。

## 2026-11-12：稳定性与正确性

- Node 状态字段补全（podIP/hostIP/conditions/restartCount），EndpointSlice 可发布 Ready Pod。
- 镜像层 sentinel、半成品清理（G3）、Pod 端口冲突/特权端口诊断。
- 优雅终止、删除宽限、启动失败退避、崩溃一致性。
- 真实电池/网络遥测经 Unix socket 推送；低电 drain；WakeLock/WifiLock；电池白名单。
- 维护模式（G2）：`/maintenance` cordon+drain，Android 设置页统一维护/重启/停止。

## 2026-11-13：M4 完整链路 + 安全 + targetSdk

- **A18**：Kubernetes watch + 60s 全量兜底，替代 15s 全量 LIST。
- **D1**：`podIP/hostIP = nodeIP`，kubelet `/pods` 补 `podIPs`，IP 不再永久缓存。
- **A10/F1**：kubelet 远程 API 统一要求客户端 CA；loopback/健康/指标例外。
- **A1/A4**：Rust 处理 TLS/101/mTLS，Go `moby/spdystream` sidecar 处理 SPDY/3.1；
  `kubectl exec`、`kubectl cp` 可用。
- **A2/A3**：容器 IO 注册表 + Unix socket 实现 `attach`；SPDY `port` stream 实现
  `port-forward`。
- **D2/D3**：ClusterIP DNAT 复验 200；D3 为 Service 名称分配 `127.10.x.y` 回环代理，
  写入 Pod `/etc/hosts`。
- **F2**：kubeconfig 用 Android Keystore AES-GCM 加密（`kubeconfig.enc`），运行时才生成
  `kubeconfig.runtime`。
- **A16**：`kubernetes.io/kubelet-serving` CSR + `O=system:nodes`，自动批准、集群 CA 签发；
  控制面移除自定义 CA bundle。
- **E3/E4**：`compileSdk/targetSdk 35`，FGS 改 `specialUse`；Android 14 实测 proot exec 正常。
- **B10**：`droidnode-local` PVC 自动创建 hostPath PV，Pod 重建数据保留。
- **B18**：确认 `layers/`、`rootfs/` 已在 `filesDir`，缓存目录只放 proot tmp。
- **G4**：1/3 Pod `dd bs=1` 实测；agent RSS ~15.9 MiB、空闲 CPU 0%，3 Pod 时单 Pod 慢 ~4×。
- **D4**：确认端口 53 非 root 不可 bind；D3 hosts 为替代方案。

## 2026-11-14：配置外部化

- **P2 起步：Android 数据层（0.1.16）**：`AgentApi` 从 `ui/v2` 移到 `data/`；新增 `data/Validation`（节点名/端口/日志级别）与 `data/RegistryMirrorStore`（`registry-mirrors.env` 的解析/渲染），`LocalConfig` 改为委托门面；设置页保存改用 `Validation`。Android 单测 14 → 23 例（新增校验 4、镜像源文件 5），全部本机通过。
- **P1 起步：核心路径测试（0.1.9 开发中）**：`ControlPlaneBrokerImpl::with_client()` 支持注入 kube client；新增 19 个 Rust 用例（测试总数 26 → 45）——心跳条件 `lastTransitionTime` 稳定性、私有地址/tether 网卡分类、`AgentStatusReply` 序列化、镜像层 whiteout/软链/延迟硬链接、exposer `/healthz` 与 404（`tower::oneshot`）。注册/心跳（wiremock）、watch 缓存语义、假 proot 退出码也已覆盖；这些用例的运行时验证放在 Linux/CI 上统一做。`cargo check --tests --target aarch64-linux-android` 与 `cargo fmt --check` 通过。
- **P0：测试地基 + 止血（0.1.9）**：Android 增加 14 个 JVM 单测（kubeconfig 解析、PKCS#8 RSA/EC 私钥、非法输入、client 证书解码、server/镜像源规范化、kubeconfig 模板），`gradlew testDebugUnitTest` 通过；Rust 抽出 `want_restart` / `effective_grace` / `terminal_for_policy` 三个纯函数并补 6 个单测（由 CI 的 ubuntu job 执行）；`AgentApi` 改为只信任 `files/kubelet-ca.crt` 签发的证书，不再全信任 loopback；删除无入口的 v1 `ImagesActivity` / `DebugActivity` 与 `activity_debug.xml`；CI 增加 `cargo fmt --check`、Android 单测与 lint（暂不阻断）。APK 12.39 → 12.36 MB。
- **工具链位置**：改由 `toolchain.env.local`（gitignore）+ 环境变量提供；两个构建脚本不再写死本机路径，新增 `-CheckToolchain` 只解析不编译。
- **镜像源**：不再编译进 agent。运行时读 `$DROIDNODE_DATA_DIR/registry-mirrors.env` 或环境变量，空 = 直连上游；k3s 侧用 `deploy/k3s-registries.yaml`。两份真文件都在 `.gitignore`，仓库只留 `.example` 模板。
- **设置页**：新增「镜像源」三个可编辑主机名输入框（docker.io / registry.k8s.io / quay.io），保存写同一个 env 文件并重启 agent；启动日志新增 `registry mirrors configured|none`，界面据此显示生效值。
- `deploy/console/setup.sh` 的安装源与 apt 源改为变量（`K3S_INSTALL_URL` / `APT_MIRROR` 等），默认仍是公共源。
- **加入集群的流程**：新增 `deploy/console/make-kubeconfig.sh`，一条命令生成 token kubeconfig 并自检（apply RBAC → 建 token → 拼 CA + LAN IP → `kubectl get nodes` 验证）；RBAC 不再默认 apply，`--with-rbac` 才装，`--long-lived` 生成可多设备复用的长期 token。
- **手机端免文件加入**：设置页新增「用 token 加入集群」，只填地址 + token；CA 按 `k3s agent` 的做法从 `/cacerts` 自动获取并显示 SHA256 指纹，也可手动粘贴。
- **加入集群独立成二级页面**（`ui/v2/JoinActivity`）：三种方式单选 —— 地址 + token / 导入 kubeconfig 文件 / 粘贴 kubeconfig（含管理员证书，带警告）；先验证再落盘，失败不覆盖现有配置。设置页只显示结果（控制面地址、身份、文件路径）与入口。
- **原型**：补齐「加入集群」二级页与顶部安全区；新增三套配色（夜班 / 观测台 / 沙纸），顶部可切换。
- **外观**：APK 新增「观测台」亮色色板（默认）与「夜班」深色色板，`values` / `values-night` 两套；设置页「外观」可选 跟随系统 / 观测台 / 夜班，`BaseActivity` 用覆盖 `uiMode` 的方式切换并补系统栏安全区。
- **加入集群页**：按原型重做 —— 三个选项卡片、错误提示就近显示、底部固定主操作、appbar 返回键；逻辑仍是先验证再落盘。
- **设置页双向对齐**：实际代码已有的能力（镜像源 ×3、保存并重启、agent 上报、kubeconfig 文件）补进原型；原型已有的设计（开机自启开关、导出诊断、外观）补进代码。`BootReceiver` 现在遵循「开机自启」开关；导出诊断走系统分享，只带版本/节点/外观/日志，不含 token 与 CA。
- **底栏切换去转场**：底栏五个页面改为 `FLAG_ACTIVITY_REORDER_TO_FRONT` + `ActivityOptions` 零动画（复用已打开实例，滚动/输入不丢）；底栏页的返回键改为退出应用，二级页仍是返回上一页。
- **节点 IP 选择**：`net_util::primary_ipv4` 跳过 Wi-Fi Direct / USB 共享 / 隧道网卡（`p2p*`、`rndis*`、`usb*`、`tun*`），避免把集群到不了的 `192.168.137.x` 当成 InternalIP；同时支持 `DROIDNODE_NODE_IP` 覆盖。
- **加入集群补两份文件**：`client-ca.crt`（F1，否则 `kubectl logs/exec` 被 kubelet 拒绝）与 `registry-mirrors.env`（否则 `ErrImagePull`）；`make-kubeconfig.sh` 现在一并产出 client CA 并打印 adb 命令。
- **私有 registry 支持**：镜像引用命中 `DROIDNODE_INSECURE_REGISTRIES`（环境变量或 `files/registry-mirrors.env`）时用明文 HTTP 拉取，便于局域网自建 registry 做部署实验。
- **网络接口偏好**：设置页新增「网络」一节，列出本机 IPv4 网卡（自动 / wlan0 / p2p 等），选中后写 `files/network-preference.env` 并重启 agent；agent 的 `primary_ipv4` 优先按网卡名（也接受 IP）解析，解析不到时回退到启发式并打一条告警。多网卡手机（Wi-Fi + Wi-Fi Direct/USB 共享）不用再靠猜。
- **构建自动递增版本**：`build-apk-win.ps1` 每次打包把 `versionName` 小版本 +1、`versionCode` +1（0.1.0 → 0.1.1），失败自动回滚；`-NoVersionBump` 可重打同版本。当前 APK 为 **0.1.2 (versionCode 3)**。
- **国际化（中/英）**：布局里的静态文案全部抽到 `values/strings.xml`（中文默认）与 `values-en/strings.xml`（英文），key 按界面分组、两边对齐；设置页新增「语言」一节（跟随系统 / 简体中文 / English），与外观共用 `attachBaseContext` 的配置覆盖，切换后所有页面自动重建。详见 [docs/i18n.md](i18n.md)。
- **代码文案也走资源**：`ui/v2/` 里 191 条动态文案（状态行、toast、连接诊断）改由 `Tr.s("中文原文", args…)` 查表生成，`Tr.kt` 维护 中文→资源 的映射；模板参数转成 `%1$s` 位置占位符。节点页键值标签从固定 64dp 改为自适应，避免英文折行。当前 APK 为 **0.1.4 (versionCode 5)**，已在 PGJM10 上验证中英切换。
- **主题改名**：外观选项统一为 **亮色（Light）/ 深色（Dark）/ 跟随系统**，去掉「观测台 / 夜班 / 沙纸」这些代号；原型与代码、文档、资源注释同步，原型顶部评审切换器只保留 Light / Dark 两个。
- **部署验证**：把 chatapp-coze（Java/Vert.x + Vue）打成 arm64 镜像，跑通「三台手机各一个副本 + Service 三端点」；踩坑记录见 `D:\opensource\chatapp-coze\deploy\PHONE-K3S.md`。
- **连接预检**：支持 PKCS#1 / SEC1（k3s 的 `BEGIN EC PRIVATE KEY`）私钥，不再静默丢客户端证书；结果区分 401（无凭据 / token 被拒 / 证书被拒）与 403（RBAC 不足），并显示实际使用的身份。
- 构建脚本：默认工具链固定为 GNU（MSVC 缺 `link.exe`），产物名兼容无 `.exe` 后缀的 Android ELF。

## 当前约束摘要

- 无 cgroup/netns/真 root；`<1024` 端口、Pod 独立 IP、NetworkPolicy、完整 DNS 不支持。
- TTY 是管道，不是 PTY。
- 仅 arm64 真机验证；targetSdk 35 需在更多 Android 版本/OEM 复验。
- CSR 自动批准仅适合可信集群。

详细限制见 [limitations.md](limitations.md)，功能状态见 [gap-list-feasibility.md](gap-list-feasibility.md)。
