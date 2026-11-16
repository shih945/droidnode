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

- **Kubelet 兼容性对比**：新增 `docs/api/kubelet-contract-v1.29.json`（kubelet v1.29 契约 + APK 扩展白名单）和 `scripts/kubelet-compat.py`（capture-adb / capture-kubectl / check / compare）；`docs/api/compatibility.md` 列出当前不兼容项：`/images` 结构、`/pods` 缺 8 个字段、5 个未实现端点，以及仅 APK 使用的 4 个端点与 10 个镜像字段。
- **API 文档导出**：新增 `docs/api/routes.json` 作为单一来源，`scripts/export-api-docs.py` 生成 `agent-http.md`（人工可读）和 `agent-openapi.json`（OpenAPI 3.1），并校验代码路由与文档是否一致；CI `rust` job 增加 `--check`，代码加了路由但没写文档会失败。
- **Pod 筛选可测 + 批量安装自动启动（0.1.43）**：把 `isBad`/`isRunning`/`severity` 抽到 `PodFilters`，新增 6 个 JVM 单测（Android 单测 32 → 38）；`adb-install-all.ps1/.sh` 增加 `-Start`/`--start`，安装后自动 `am start`（之前批量安装会把 app 停掉，导致节点 NotReady）；修复 `rust-test-device.ps1` 把 `0 failed` 误判成 `FAILED` 而返回 1 的问题；三台手机节点均 Ready。
- **异常筛选修复 + Pending 真机验证（0.1.42）**：`waitingReason=ErrImagePull` 的 Pending Pod 现在会计入「异常」筛选；用管理员 kubeconfig 部署 `deploy/test-pull-fail.yaml` 验证：Pods 页显示「全部 8 / 异常 1 / 拉取失败」，删除后恢复「全部 7 / 异常 0」。同时确认 dn-20pro 的 NotReady 只是批量安装后 app 未启动，重新打开后已 Ready。
- **R13 结构化错误（0.1.41）**：新增 `DroidError::ImageInUse { id }`（409），`/images/delete` 改用它；错误映射测试覆盖该变体。R15 轮询聚合决定不做（会把设置页变成全量轮询）。
- **R11 完成（0.1.40）**：`reconciliation_service.rs` 1,040 → 383（policy/watch/start/reconcile）、`workload_lifecycle_service.rs` 872 → 403（probes/trait_impl）、`workload_execution_service.rs` 640 → 254（env/volumes），三个目标文件全部 ≤500 行；62 个 Rust 测试真机通过。
- **R9 覆盖率 baseline + 门禁**：用 `llvm-tools`（rsproxy 镜像）在真机跑出项目源码覆盖率 **27.07% 行 / 26.62% 函数**；CI `coverage` job 增加 `--fail-under-lines 25` 并排除依赖/std，本地脚本同步只统计项目源码。
- **R12 unwrap 预算（0.1.38）**：新增 `scripts/count-unwraps.py`，剥离测试模块后统计非测试 `unwrap/expect`，当前 **18** 处；CI 增加 `--max 18` 门禁，防止新增 panic 点。
- **R11 起步 + R14 端口统一（0.1.38）**：reconciliation 的纯策略（backoff/restart/grace/terminal）与 pod-watch 缓存拆到 `reconciliation_service/{policy,watch}.rs`，主文件 1,040 → 964 行；Rust 新增 `DEFAULT_KUBELET_PORT = 10255`，`NodeReadinessService` 默认值改为 `AppPrefs.DEFAULT_KUBELET_PORT`，两边单测锁定同一端口；Rust 测试 62/62 真机通过。
- **不再自动拉取 alpine（0.1.37）**：启动时的 proot 冒烟测试默认关闭，只有 `DROIDNODE_PROOT_SMOKE=1` 才运行。此前每个节点每次启动都会 prepare 一个 alpine rootfs（与 Pod 共用镜像目录），所以镜像页总能看到一个没被使用的 `alpine:latest`。真机验证：删除 alpine 后重启 agent，镜像仍为 2 个，不再重新下载。
- **镜像占用检测修复（0.1.36）**：`mark_in_use` 记录的是每 Pod 的可写 rootfs，而 `/images/delete` 检查的是共享镜像 rootfs，两者永远不相等，导致正在运行 Pod 的镜像也能删掉。`RunningContainer` 现在保留 `image_rootfs` 并在启停时成对标记/解除；新增 handler 级测试「删除使用中镜像返回 409」。
- **Pod 调度即显示 / 失败可见（0.1.35）**：Pod 一被绑定到本机就上报 `Pending`（不再等镜像拉完才出现在列表）；容器处于 `ErrImagePull`/`ImagePullBackOff` 时界面显示「拉取失败」并归入「异常」筛选，详情里的「最近事件」给出原因；删除尚未启动的 Pod 会同步清掉本地缓存（新增 `WorkloadLifecycleService::forget_status`）。真机 0.1.35 待 PGJM10 重新连接后验证。
- **私有仓库与 Pod 状态修复（0.1.34）**：私有/环回 IP 上的镜像仓库默认走 HTTP（显式 `DROIDNODE_INSECURE_REGISTRIES` 仍然优先，主机名仓库不自动降级），解决了 `192.168.2.7:5000` 的 chatapp 镜像一直按 HTTPS 拉取失败的问题；拉取/创建阶段的 Pod 状态从错误的 `Running` 改为 `Pending`，不会再出现「显示运行中但镜像列表里没有该镜像」。真机验证：移除手写 insecure 配置后两个 chatapp 容器正常拉取并启动，镜像页显示 3 个镜像（alpine + backend + gateway）。
- **应用图标（0.1.32）**：使用桌面提供的 `android-k8s-node` 图作为启动图标——自适应图标（白底 + 前景 78% 安全区）覆盖 `mipmap-anydpi-v26`，并生成 mdpi…xxxhdpi 的传统方形图标；Manifest 增加 `android:icon`/`android:roundIcon`。真机安装确认资源被系统解析（`aapt2 badging` 指向自适应 XML），应用正常启动。
- **真机跑 Rust 测试（无需 CI）**：新增 `scripts/rust-test-device.ps1`，交叉编译 aarch64 测试二进制并推到设备执行，59/59 通过；修正真机上暴露的测试问题（`/images` 断言键、kube rustls 在无系统信任库时崩溃、docker.io 用例可用 `DROIDNODE_SKIP_NETWORK_TESTS` 跳过）。
- **R6/R7：配置收口与错误统一（0.1.30）**：新增 `executor::config::RuntimeConfig`（OnceLock），执行器与 node-agent 的 `env::var` 从 39 处降到 1 处（`config.rs`）；`DroidError` 增加 `Conflict`（409），`/images/delete` 改为 `Result<_, DroidError>`，JSON 错误体统一由 `IntoResponse` 生成；SPDY 流式 handler 保留纯文本错误以兼容 kubectl。真机 0.1.30 验证节点运行正常。
- **C9 收尾：SettingsActivity 216 行（0.1.29）**：视图绑定/监听移到 `SettingsBinder`（100 行），保存/维护/agent 启停移到 `SettingsActions`（150 行），渲染仍在 `SettingsRenderer`（184 行）；Activity 从 569 降到 216 行。真机验证设置页渲染与「连接测试」按钮正常。
- **C9 设置页渲染拆分（0.1.28）**：`render()`/`renderRunControl()`/`agentReportText()`/`registry*`/网卡列表抽到 `SettingsRenderer`（184 行），`SettingsActivity` 569 → 389 行；真机截图确认渲染一致（镜像源、节点、端口、连接测试入口）。
- **真机流程与测试补强（0.1.27）**：新增 `scripts/device-restore.ps1/.sh`（仪器测试后一条命令恢复 dn-k10：导入 `/sdcard/Download/droidnode-kubeconfig` + 写回节点名/端口/语言/自启）；仪器测试改为手动安装 APK + `am instrument`，不再卸载 app 清数据；新增 `TrTest` 遍历全部 `Tr` 条目（真机 3 个仪器测试通过）；`SIGTERM` 优雅退出在真机验证（deregister、0.35s 退出）；修复恢复脚本导入明文后加密文件已存在导致明文残留的问题——`KubeconfigStore.read` 现在优先迁移并删除明文。
- **真机冒烟脚本（Windows）**：新增 `scripts/smoke-device.ps1`，与 bash 版一致（安装→启动→进程/前台 Activity/按包名过滤崩溃→截图），已在 PGJM10 上跑通；bash 版同步改为清空 crash 缓冲并按 `Process: com.droidnode` 过滤。注意 `connectedDebugAndroidTest` 会卸载 app 并清空数据。
- **T11 真机仪器测试**：新增 `androidTest` 的 `KubeconfigCryptoTest`（AES-GCM 往返、随机 nonce），在 PGJM10 上 2/2 通过；Gradle 增加 `testInstrumentationRunner` 与 androidx.test 依赖。
- **真机发现：端口显示不刷新（0.1.26）**：修改 kubelet 端口并重启后，节点页仍显示旧端口（`tvKubelet` 只在 onCreate 设置）。改为每次渲染读取 agent 自报端口（IPC），回退到本地配置；真机把 10255→10256→10255 验证通过。
- **真机修正：证书固定改回 CA 信任（0.1.25）**：TOFU 指纹在 agent 每次重启后失效（serving 证书每次重签），导致第二次启动后节点页再次「无响应」。改为信任签发 CA 的集合：`files/kubelet-ca.crt` + `files/client-ca.crt` + kubeconfig 的 `certificate-authority-data`；证书轮换不再影响连接，同时仍拒绝无关证书。真机连续两次 force-stop/重启验证通过（无 SSL 错误，节点页 2/2 Pod）。
- **真机发现：D3 特权端口（0.1.24）**：非 root 应用无法绑定 <1024 端口，ClusterIP 代理对 53/80/443 会失败并每 10 秒刷日志。改为每个 (IP,端口) 只提示一次，并建议该 Service 改用 NodePort；限制写入 `docs/limitations.md`（仅 TCP、仅本机容器可见）。
- **真机验证修复：agent 证书固定改为 TOFU（0.1.23）**：0.1.22 的「只信任 `files/kubelet-ca.crt`」在真机上把正常的 agent 拒掉了（serving 证书可能由集群 CSR 签发，与本地 CA 不同链），节点页显示「agent 无响应」。改为首次连接记录 serving 证书 SHA-256 指纹、之后只接受同一张证书；同时对请求失败打日志。真机验证：节点运行中、2/2 Pod 就绪、Pod/设置页正常、中英/亮暗切换即时生效。
- **P4 起步：设备冒烟与覆盖率（2026-11-15）**：新增 `scripts/smoke-device.sh`（安装 APK → 启动控制台 → 检查进程/前台 Activity/崩溃缓冲区 → 截图）；CI 增加 `coverage` job，用 `cargo llvm-cov` 生成 lcov 并上传，暂不设覆盖率门禁。
- **P3：Rust 结构整理（0.1.20–0.1.22）**：`virtual_kubelet_exposer.rs` 2266 → 214 行，拆出 `exposers/state.rs`、`exposers/tls.rs` 与 `handlers/` 9 个模块；`ControlPlaneBroker` 的 17 个方法拆成 `NodeApi` / `PodWatch` / `EventRecorder` / `PvcApi` 四个窄 trait（超 trait + blanket impl，调用点不变）；`DroidError` 增加统一 `IntoResponse` 映射；退出路径改为单次 SIGINT/SIGTERM 并只 deregister 一次；Android 侧端口以 agent IPC 自报为准；新增 D3 `hosts_block`、`/pods`/`/images` handler、卷（configMap/secret/hostPath）与探针（tcp/http/exec）测试。
- **P2：Android 数据层、单一数据源与设置页拆分（0.1.16–0.1.19）**：`AgentApi` 与 `KubeconfigCrypto` 移到 `data/`；`LocalConfig` 拆成 `AppPrefs` / `KubeconfigStore` / `NetworkInterfaces` / `RegistryMirrorStore` 并只保留 128 行门面；新增 `data/Validation`（节点名/端口/日志级别），设置页保存改用它。node/pods/images 三页改由 `NodeRepository` + `StateFlow` 共享一个 2s 轮询，图片每 3 tick 刷新；新增 Repository 单测 3 例（首帧、失败保留旧值、图片节流）。`KubeConnectionTest` 移入 `data/`；`SettingsActivity` 的保存/诊断/维护逻辑抽到 `SettingsController`，镜像源日志解析抽成纯函数 `SettingsText`（3 例）；新增 `I18nParityTest` 保证中英 key 集合一致（2 例）。Android 单测 14 → 23 例（新增校验 4、镜像源文件 5），全部本机通过。
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
