# 架构与代码组织评审

> 评审对象：当前 `master`（6 个提交之后，`0.1.8`）。
> 方法：读结构、读关键模块、量指标（LOC / 测试 / 体积 / 依赖），对照 Rust、Android 与
> Kubernetes 节点代理的常见组织方式。文中引用均为 `文件:行`。

## 0. 结论摘要

**总体判断：底层（Rust agent）的分层是对的，上层（Android）的架构债最重，工程化（测试/CI）明显偏薄。**

| 维度 | 评分 | 说明 |
|---|---|---|
| 模块边界与依赖方向 | ★★★★☆ | `brokers → services → exposers` 是标准的端口/适配器雏形，trait 齐全，依赖方向没有反向 |
| 命名与一致性 | ★★★☆☆ | `brokers/services/exposers` 命名统一；但 `ServiceImpl` 后缀、`ui/v2` 里放网络层、`object` 单例过多 |
| 可测试性 | ★★☆☆☆ | Rust 15 个单测 / ~10k 行，无集成测试；Android **0** 测试（整改方案见第 7 节） |
| UI 架构 | ★★☆☆☆ | 5 个 Activity + 每页各自轮询 + 16 个全局 `object`，没有 ViewModel/Repository |
| 可观测性与运维 | ★★★★☆ | 结构化日志、状态 IPC、`/stats/summary`、诊断导出都有；但 agent 自身无 metrics |
| 工程化 | ★★☆☆☆ | CI 只做 Rust check/clippy/test + Android Kotlin 编译，无 lint、无 APK 构建、无 Android 测试 |
| 文档 | ★★★★★ | `docs/` 9 篇 + 原型 + CHANGELOG，信息密度和准确性都高于一般项目 |

最值得立刻处理的三件事：

1. `AgentApi` 对 loopback 的 TLS **全信任**（`ui/v2/AgentApi.kt:28-36,60`）。
2. Android 端 **零测试 + 零 lint**，而它承担了 kubeconfig 解析、TLS 校验、加密落盘这些高风险逻辑；Pod 调度/运行这条核心链路在 Rust 侧也没有回归网（方案见第 7 节）。
3. `SettingsActivity`（636 行）和 `LocalConfig`（325 行）各自是 God Class，改一处要读全局。

---

## 1. 现状架构

### 1.1 Rust agent（`agent/`）

```
node-agent/main.rs            ← composition root：读 env、建 broker/service、spawn 监督循环
  └── executor
        ├── brokers/          ← 外部系统适配器（trait + Impl）
        │     control_plane_broker(17 个方法) / oci_registry / filesystem / proot
        ├── services/
        │     ├── foundation/ ← 单一能力：pull / unpack / execute / probe / store / ipc
        │     └── orchestration/ ← 编排：reconcile / lifecycle / registration / image pipeline
        ├── exposers/         ← 入口：virtual_kubelet_exposer（kubelet HTTPS API）
        ├── models.rs         ← OCI + k8s 领域类型
        ├── status_ipc.rs     ← 给 App 的 unix socket 状态通道
        ├── net_util / proc_metrics / registry_mirror
        └── error.rs          ← thiserror 统一错误
```

依赖方向是干净的：`exposer → services → brokers`，broker 之间不互相调用，`main.rs` 是唯一
composition root。`ReconciliationService` 采用 **watch + 60s 全量 LIST 兜底 + 10s→5min 指数退避**
（`reconciliation_service.rs:18-21,644-690`），这是 kubelet/controller 的标准做法，比单纯轮询成熟。

### 1.2 Android（`android/`）

```
Application(DroidNodeApp) ──设置 AgentApi.port（时序耦合）
Activity ×5（Node/Pods/Images/Logs/Settings + Join）
   └── 直接调 object 单例：Snapshot / AgentApi / LocalConfig / Tr / AgentStatus / LogBuffer
          ├── AgentApi      HTTPS 127.0.0.1:10255（全信任证书）
          └── LocalConfig   SharedPreferences + 文件 + kubeconfig 模板 + Keystore 加密
ForegroundServiceExposer  ← 组装 agent 环境变量、拉起 Rust 进程、前台通知
```

没有 ViewModel、没有 Repository、没有 DI；页面通过 `object` 拿数据，数据层是「同步 HTTP +
Activity 里的 `while (isActive) { delay(...) }` 轮询」。

### 1.3 其它

- `deploy/console/*.sh`：k3s 引导、kubeconfig 生成（幂等、参数化，质量不错）。
- `scripts/`：Linux/Win 两套构建脚本，逻辑重复（env 文件解析、工具链探测各写一遍）。
- `prototype/`：纯静态 HTML/CSS/JS，无构建步骤，评审友好；但设计稿与实现是两份独立文案。
- `docs/`：能力矩阵、限制、基准、集群引导、i18n、CHANGELOG，覆盖到位。

---

## 2. 与业界常见组织方式对照

| 维度 | 业界常见做法 | 本项目 | 评价 |
|---|---|---|---|
| Rust 工程结构 | workspace 按「领域 / 适配器 / 二进制」拆 crate，`tests/` 放集成测试 | 单个 `executor` crate，按模块分层；无 `tests/` | 模块分层合格，缺少编译期边界与集成测试 |
| Rust 配置 | 一个 `Config`（figment/config crate）注入到各组件，深层模块不读 env | `Config` 只有 5 个字段（`main.rs:326`），其余 39 处 `env::var` 散落在 8 个文件（exposer 一个文件 12 处） | 最大的一处「隐式输入」问题，测试时无法替换 |
| Rust 错误 | 每个 crate 自己的 `Error`，带结构化字段 + `IntoResponse` 映射 | 单一 `DroidError`，一半变体是 `String`（`error.rs`） | 能用，但调用方拿不到结构化信息 |
| K8s 节点代理 | controller-runtime 风格：workqueue、rate limiter、context 取消、优雅退出 | watch+LIST+固定 5s 重启循环，无 CancellationToken；退出路径要求按两次 Ctrl-C | 核心逻辑对，生命周期管理偏「脚本化」 |
| Android 架构 | 单 Activity + Compose/Fragment + ViewModel + Repository + DI（Hilt/Koin） | 5 个 Activity 直接访问单例，手写轮询 | 与主流差距最大的一块 |
| Android 数据层 | DataStore/Flow、缓存 + 去重、后台单一数据源 | SharedPreferences + 文件 + 每页自行拉取 | 状态重复请求、页面切换闪一下 |
| 质量门禁 | detekt/ktlint + lint + 单测 + 仪器测试 + APK 构建 | 仅 `compileDebugKotlin` | CI 只能防「编译不过」 |

---

## 3. 设计模式使用评估

**用得好的**

- **端口/适配器（Hexagonal）**：`brokers/*` 定义 trait，`*Impl` 实现，service 只依赖 trait。
  这是本项目最正确的一个决定，也是未来写集成测试的抓手。
- **Composition Root**：所有依赖在 `main.rs` 里手工装配，没有隐藏的全局容器。
- **Reconciler（level-triggered）**：`reconciliation_service` 以「期望状态 vs 实际状态」收敛，
  带 watch 缓存 + 全量 LIST 兜底，符合 K8s 控制循环范式。
- **策略/模板**：镜像拉取按 `imagePullPolicy` 分支、凭据按 token/证书分支，逻辑收敛在 service 内。
- **Facade**：`status_ipc` 对 App 暴露一个稳定 JSON 形状，内部状态变化不影响 UI 协议。

**用得有问题的**

| 反模式 | 位置 | 影响 |
|---|---|---|
| God Object | `ui/v2/LocalConfig.kt`（325 行，50 个成员：偏好、文件格式、kubeconfig 模板、加密、网卡枚举） | 任何 UI 都依赖它，改一处牵全身；无法替换实现 |
| God Activity | `ui/v2/SettingsActivity.kt`（636 行，25 个方法：agent 生命周期、配置、镜像源、网卡、连接测试、诊断导出、i18n 渲染） | 难测、难改、多人同时改必冲突 |
| Service Locator / 全局单例 | 16 个 `object`（`AgentStatus`、`LogBuffer`、`Snapshot`、`AgentApi`、`LocalConfig`、`Tr`…） | 隐式依赖、生命周期不清、测试无法隔离 |
| 临时耦合 | `DroidNodeApp` 启动时设置 `AgentApi.port` | 若 Application 未先跑（测试/工具），端口就是默认值 |
| Stringly-typed | `Tr` 以中文原文为 key（`Tr.kt:217` 找不到时**静默回退中文**） | 改文案忘改 map，英文界面悄悄冒出中文 |
| 分层泄漏 | `KubeConnectionTest`（网络 + TLS + kubeconfig 解析）放在 `ui/v2/`；`AgentApi`（HTTP 客户端）也在 UI 包 | 业务逻辑和界面混在一起，无法在无 Android 环境单测 |
| 重复实现 | v1 `ImagesActivity`、`DebugActivity` 与 v2 并存；两套构建脚本各自解析环境文件 | 死代码、维护双倍 |
| 原始类型偏执 | `DroidError::X(String)`、`Pair<Boolean, String>` 作为结果 | 调用方只能靠字符串判断 |

---

## 4. 问题清单（按严重度）

### P1 — 安全 / 正确性 / 可维护性阻断项

**4.1 `AgentApi` 对 loopback 的证书全信任（0.1.9 已修：改为只信任 `files/kubelet-ca.crt`；无 CA 时退回宽松策略）**
`ui/v2/AgentApi.kt:28-36` 的 `X509TrustManager` 两个方法都是空实现，`:60` 的 hostname verifier
恒为 true。注释说「仅限本进程」，但 `127.0.0.1:10255` 是**设备级**的：其它 App 也能连，
本 App 也无法区分「agent」与「冒充 agent 的本地进程」。
建议：pin kubelet serving 证书（或至少 pin `kubelet-ca.crt` 的 SPKI），或用 unix socket
（agent 已经有 `files/agent.sock` 了，状态通道完全走它更合理）。

**4.2 Android 零测试**
`android/app/src` 下只有 `debug/` 和 `main/`，没有 `test/`、`androidTest/`。
而高风险逻辑都在 Android 侧：kubeconfig 解析与私钥包装（`KubeConnectionTest.kt`，336 行）、
kubeconfig 加密（`KubeconfigCrypto`）、端口/节点名校验、i18n 回退。
建议：先把纯函数抽到 `data/` 包，用 JUnit 覆盖 kubeconfig 解析、PKCS#1/SEC1 包装、校验规则、
`Tr` 查表；这几块不需要设备。

**4.3 `SettingsActivity` 是 God Activity**
636 行、25 个方法，同时管：前台服务启停、节点配置保存、镜像源读写、网卡枚举与选择、
连接测试、诊断导出、外观/语言渲染。任何一个需求改动都要进这个文件。
建议：抽出 `SettingsViewModel` + 按 section 拆成 4 个视图/绑定器（控制面、节点、运行、外观与诊断）。

**4.4 `LocalConfig` 是 God Object**
325 行、50 个成员，混合了偏好（appearance/language/autostart）、文件格式（registry-mirrors.env、
network-preference.env）、kubeconfig 模板与加密、网卡枚举。
建议拆成 `AppPrefs`、`RegistryMirrorStore`、`KubeconfigStore`（含 crypto）、`NetworkInterfaces`。

**4.5 配置读取散落，`Config` 形同虚设**
`main.rs:326` 的 `Config` 只有 5 个字段，实际 `env::var` 共 39 处，分布在 8 个文件，
其中 `virtual_kubelet_exposer.rs` 一个文件 12 处。这导致：
测试无法注入配置、`--help` 式的配置文档不存在、同一变量在不同模块有不同默认值
（见 4.7 的端口默认值漂移）。

### P2 — 结构性技术债

**4.6 Activity-per-tab + 每页自行轮询**
Node 2s、Pods 2s、Images 5s、Settings 3s，各自 `while (isActive) { ...; delay() }`，
每次 `AgentApi` 都新建一条 HTTPS 连接（无连接池），页面切换会重复拉取同一份数据。
建议：单一数据源（Repository + `StateFlow`，2s 一个 tick 拉一次，多个页面共享），
或让前台服务持有数据、UI 只订阅。

**4.7 端口默认值漂移**
Rust `Config::from_env` 默认 `10250`（`main.rs:361`），Android `LocalConfig.DEFAULT_KUBELET_PORT`
是 `10255`。正常安装路径上 `ForegroundServiceExposer` 会传 env，所以没炸；但手工运行 agent
或用工具直连时会错位。建议：默认值只定义在一处（agent 输出实际端口、App 从 IPC 读取）。

**4.8 Rust 巨文件（0.1.22 已拆：exposer 2266→214 行，broker trait 拆成 4 个窄接口）**
| 文件 | 行数 | 内容 |
|---|---|---|
| `exposers/virtual_kubelet_exposer.rs` | 2266 | 路由 + logs/exec/attach/port-forward + stats + images + health + log store + TLS 配置 |
| `brokers/control_plane_broker.rs` | 1092 | Node 对象、注册、Pod watch、事件、PVC、镜像列表 |
| `services/orchestration/reconciliation_service.rs` | 894 | 主循环 + watch 缓存 + 每个 Pod 的收敛逻辑 |
| `services/orchestration/workload_lifecycle_service.rs` | 869 | 启动/停止/探针/卷 |

建议：`exposers/kubelet/{logs,exec,stats,images}.rs`；`ControlPlaneBroker` 按资源拆成
`NodeApi` / `PodWatch` / `EventRecorder` / `PvcApi`（当前 17 个方法违反接口隔离，
service 为了一个方法也不得不 fake 整个 trait）。

**4.9 错误类型过于字符串化**
`DroidError` 一半是 `X(String)`，HTTP 层只能靠字符串前缀映射状态码。
建议：给关键失败（ImagePull、Exec、ContainerStart）定义带字段的变体，并实现
`IntoResponse`，把「错误 → 状态码」集中在一个地方。

**4.10 退出路径要求按两次 Ctrl-C（0.1.20 已修：SIGINT/SIGTERM 单次退出）**
`main.rs:296-318`：`select!` 收到第一次 SIGINT 后打印「shutting down」，随后又
`tokio::signal::ctrl_c().await`，再才 `deregister()`。结果是第一次 Ctrl-C 不会退出。
另外 Android 侧只会收到 SIGTERM/进程被杀，`deregister` 实际很少执行。
建议：统一 `CancellationToken`，SIGINT/SIGTERM 都触发一次优雅退出（并在 Android 服务
`onDestroy` 里给 agent 留时间 deregister）。

**4.11 `unwrap/expect` 出现在请求处理路径**
`virtual_kubelet_exposer.rs` 有 12 处（该文件没有测试模块），包括
`Response::builder()...unwrap()` 和 `RwLock ... .expect("lock poisoned")`。
HTTP 构建失败虽不常见，但请求路径 panic 会打断 exposer 任务（虽然有 supervise 重启）。
建议：锁用 `parking_lot` 或返回 500，builder 用 `unwrap_or_else(|_| internal_error())`。

**4.12 `Tr` 以中文原文作为资源 key**
`Tr.kt` 215 行 map，`Tr.s("中文")` 查表；`Tr.kt:217` 找不到就返回中文。
好处是调用处可读，坏处是：改一个字就静默丢翻译；key 不是编译期检查；191 条映射集中在一个文件。
建议：改成短 id（`R.string.node_status_running` 直接调用），或至少加一个 CI 脚本校验
「代码里出现的 `Tr.s("…")` 都在 map 里」。

### P3 — 整洁度 / 一致性

- **死代码**：`com.droidnode.ImagesActivity`（v1）已无入口；`ui.DebugActivity` 只能 adb 打开；
  `MainActivity` 已还原为无界面启动器（正确）。建议删 v1 两个页面及其布局。
- **构建**：`android/app/build.gradle` 的 release 是 `minifyEnabled false`，没有 signingConfig
  与 proguard 规则；`useLegacyPackaging = true`（打包 .so 需要，但要写清原因）。
- **CI**：`.github/workflows/ci.yml` Android job 只跑 `compileDebugKotlin`；没有 lint、没有单测、
  不产出 APK；Rust 有 clippy+test，但没有 `cargo fmt --check`。
- **脚本重复**：`build-apk-win.ps1`（414 行）与 `build-android.sh`（254 行）各自实现 env 文件解析、
  工具链探测、proot staging；建议共享一份「检测逻辑」（脚本或 Rust 小工具）。
- **shell 健壮性**：`deploy/console/setup.sh` 只有 `set -u`，没有 `-e`/`pipefail`；失败步骤靠
  人工看日志（有 `LOG` 文件，算半个补救）。
- **i18n 资源命名**：`v2_settings_38` 这类编号 key 对翻译者不友好；建议至少按语义命名，
  或在 strings.xml 里保留注释说明所在页面。
- **协议版本**：`status_ipc` 的 JSON 没有 `version` 字段，App 与 agent 必须同包升级；
  加一个 `proto: 1` 成本极低。

---

## 5. 建议的目标结构

### 5.1 Android

```
app/
├── data/                 ← 新增
│   ├── AgentApi.kt            （从 ui/v2 移出，pin 证书 / unix socket）
│   ├── KubeconfigParser.kt    （从 KubeConnectionTest 拆出，纯 Kotlin）
│   ├── KubeconfigStore.kt     （加密落盘 + 运行时解密）
│   ├── SettingsStore.kt       （替代 LocalConfig 的偏好部分）
│   └── Repository.kt          （轮询/缓存/StateFlow，单一数据源）
├── ui/
│   ├── node/NodeViewModel.kt  + NodeActivity（只做渲染）
│   ├── pods/PodsViewModel.kt  …
│   └── settings/SettingsViewModel.kt + 按 section 的 binder
└── service/ForegroundServiceExposer.kt（保持）
```

收益：Activity 不知道 HTTP；Repository 可以单测；页面共享同一份快照；以后换 Compose 只动 UI 层。

### 5.2 Rust

```
executor/src/
├── config.rs          ← 唯一 Config（env 只在这里读）
├── domain/            ← models 拆成 oci.rs / k8s.rs
├── brokers/           ← 拆细：node_api / pod_watch / event_recorder / pvc_api
├── services/…         ← 不变（分层是对的）
├── exposers/kubelet/  ← logs.rs / exec.rs / stats.rs / images.rs / health.rs
└── error.rs           ← 结构化变体 + IntoResponse
```

收益：单文件回到 300–500 行；`ControlPlaneBroker` 拆分后每个 service 只依赖自己需要的能力，
mock 成本大幅下降；Config 集中后可以写「配置 → 行为」的集成测试。

---

## 6. 分阶段路线（按收益/成本排序）

> 逐阶段排期、测试项与验收标准见 [阶段计划](test-and-refactor-plan.md)。

| 阶段 | 内容 | 预估 | 验收 |
|---|---|---|---|
| **1. 止血**（1 天） | `AgentApi` 证书 pin 或改 unix socket；`setup.sh` 加 `set -euo pipefail`；CI 加 `cargo fmt --check` + `./gradlew lint`；删 v1 死代码 | 1 天 | 无安全告警、CI 三件套绿 |
| **2. 测试地基**（按第 7 节执行） | 可测性改造（Client/Config 注入、`plan()` 纯函数、假 proot）+ L0/L1 用例 | 3–4 天 | 核心模块单测 ≥ 60 条，L3 至少 1 条 e2e |
| **3. Android 分层**（3–5 天） | `data/` 包 + Repository + StateFlow；5 个 Activity 改 ViewModel；`LocalConfig` 拆分 | 3–5 天 | 页面切换不重复请求；`SettingsActivity` < 250 行 |
| **4. Rust 拆模块**（3–4 天） | exposer 拆 5 个文件；`ControlPlaneBroker` 拆 4 个 trait；Config 收口；错误结构化 | 3–4 天 | 无 > 600 行文件；`env::var` 只在 `config.rs` |
| **5. 生命周期**（1–2 天） | `CancellationToken` + SIGTERM/SIGINT 单次优雅退出；Android `onDestroy` 调用 stop 并等待 | 1–2 天 | kill -TERM 后节点主动 deregister，日志无 panic |

不建议现在做的：引入 Compose、Hilt、多模块 Gradle、把 agent 拆成多个 crate。当前规模
（Rust ~10k 行、Kotlin ~5k 行）还没到必须上这些重武器的时候，先把测试和边界补齐收益更大。

---

## 7. 测试策略与改造方案

> 执行排期（P0–P4、工作量与验收标准）见 [阶段计划](test-and-refactor-plan.md)。
> 目标：把「k3s 连接、Pod 调度与运行、镜像、卷、探针、IPC、Android 加入集群」这些主功能
> 从「只能真机手测」变成「能在 CI 里回归」。下面先盘点现状，再给分层方案、改造点和可落地样例。

### 7.1 现状盘点

Rust 侧共 15 个 `#[test]`，集中在纯函数和构造逻辑：

| 文件 | 测试 | 覆盖了什么 |
|---|---|---|
| `models.rs` | 5 | 数量解析（memory/cpu）、fmt、nice 值换算 |
| `registry_mirror.rs` | 4 | docker.io/k8s/quay 改写、幂等、未命中 |
| `brokers/control_plane_broker.rs` | 4 | `local_ip`/`self_ip`、Node 对象构造 |
| `brokers/oci_registry_broker.rs` | 3 | pull-secret 的 Basic 头、无凭据不加头 |
| `brokers/proot_broker.rs` | 2 | 命令行拼装（`build_command`） |
| `brokers/filesystem_broker.rs` | 1 | 层路径拼装 |
| `services/foundation/pull_secret_service.rs` | 7 | dockerconfigjson / dockercfg / 端口 / 首匹配 |
| `services/orchestration/image_orchestration_service.rs` | 1 | rootfs 路径 |

**完全没有测试的主功能**（按风险排序）：

| 功能 | 现状 | 风险 |
|---|---|---|
| Pod watch → reconcile（增/删/改/重启/排空） | 0 测试 | 最高：这是 agent 的核心循环 |
| 节点注册 / CSR / 心跳 / 401-403 处理 | 只测了 Node 对象构造 | 高：k3s 连接的主路径 |
| 容器启动（spawn、env、mounts、退出码、日志采集） | 只测了命令行拼装 | 高：pod 能不能跑起来 |
| 镜像拉取与解包（digest、层、whiteout、符号链接） | 0 | 高：装不上的第一现场 |
| 卷（configMap/secret/emptyDir/hostPath） | 0 | 中 |
| 探针（http/tcp/exec） | 0 | 中 |
| D3 ClusterIP 代理 / `/etc/hosts` | 0 | 中 |
| `status_ipc` 协议 | 0 | 中 |
| exposer 的 20+ 个 kubelet handler | 0 | 中（有 `tower` 依赖，可直接 oneshot 测） |
| Android：kubeconfig 解析、私钥包装、校验、Tr 查表 | 0 | 高（安全相关，且是纯逻辑，最好测） |

CI 现状：`cargo check/clippy/test`（host）+ aarch64 交叉 `check` + `./gradlew compileDebugKotlin`，
**没有** Android 单测、没有集成测试、没有覆盖率。

### 7.2 分层测试模型

| 层 | 名称 | 跑在哪 | 依赖 | 速度 | 目标占比 |
|---|---|---|---|---|---|
| L0 | 纯单元 | `cargo test` / JVM | 无网络、无进程 | ms | ~50% 用例 |
| L1 | 适配器（mock HTTP / 临时目录 / Router oneshot） | `cargo test` | wiremock、tempfile、tower | 100ms~1s | ~35% |
| L2 | 进程执行（假 proot / 假命令） | `cargo test` | 本地 shell 脚本 | 1s | ~10% |
| L3 | 集群集成（k3d + 假 runtime） | CI nightly | Docker、k3d | 分钟 | ~5% |
| L4 | 真机（现有手工 + adb 脚本） | 手机 | 设备 | 分钟 | 关键路径 |

原则：**默认只跑 L0–L2**（快、稳定、无需 root/设备）；L3 放 nightly/manual label；L4 保持现有
真机验证流程（`prototype/_reference` 那套 adb 取证可以继续用）。

### 7.3 关键功能的测试矩阵

| 功能 | 层级 | 代表性用例 | 替身 |
|---|---|---|---|
| 节点注册 | L1 | `POST /api/v1/nodes` 201；409 时 PATCH 更新；401 → `NodeRegistration` 错误；body 里 capacity/labels/taints 正确 | wiremock + `Client::try_from(uri)` |
| CSR 与 kubelet 证书 | L1 | 提交 CSR → 批准模拟 → 生成 serving cert；SAN 含全部网卡 IP、不含 p2p | wiremock + rcgen |
| 心跳与 Ready | L1 | 周期 PATCH status；apiserver 5xx 时重试且不 panic | wiremock |
| Pod watch | L1 | 返回 `ADDED/MODIFIED/DELETED` 流 → 缓存一致；流中断 → 回退 LIST | wiremock streaming body |
| Reconcile 决策 | **L0** | `plan(desired, actual)`：新增/删除/重启/排空/未就绪；幂等（跑两次无变化） | 纯函数 |
| 容器启动 | L2 | 假 proot 记录 argv/env/cwd；退出码 0/1/137 映射到 Pod phase；日志落盘 | 假 `proot` 脚本 |
| 镜像拉取 | L1 | 匿名/manifest list（多架构选择）/digest 校验失败 | 本地 httptest registry |
| 层解包 | L0 | tar 夹具：普通文件、目录、符号链接、硬链接、whiteout、路径穿越防护 | tempdir |
| 卷挂载 | L0 | configMap/secret 生成文件与权限；emptyDir 跨容器共享；hostPath 越界拒绝 | tempdir |
| 探针 | L1 | http 200/500、tcp 拒绝、exec 超时；失败次数 → NotReady | 本地 server + `tokio::time::pause` |
| D3 代理 | L1 | ClusterIP → 本地端口映射；`/etc/hosts` 内容；端口冲突处理 | 本地 socket |
| 状态 IPC | L1 | 请求/响应 JSON 往返；客户端断开不影响 agent；非法 JSON 不崩 | unix socket |
| exposer handlers | L1 | `/healthz`、`/pods`、`/containerLogs`（tail/since）、`/images` 删除 409 | `tower::ServiceExt::oneshot` |
| Android kubeconfig | L0 | token / client-cert / 无凭据；PKCS#1→PKCS#8、SEC1 曲线推断；非法 YAML | JUnit（纯 Kotlin） |
| Android 校验与 i18n | L0 | 端口 1024–65535、节点名正则、镜像源主机名；`Tr.s` 的 key 全部在 map 里 | JUnit + 反射遍历 map |
| Android Repository | L0 | 假 `AgentClient`：成功/超时/401；并发刷新去重 | 接口替身 |

### 7.4 先决条件：可测性改造

现在的结构有几处**挡住测试**，需要先动（都属于小改）：

1. **`Client` 注入**：`ControlPlaneBrokerImpl::new` 内部 `Client::try_default()`
   （`control_plane_broker.rs:171-173`），测试只能靠 `KUBECONFIG` 环境变量（串行、易污染）。
   加 `with_client(node, port, client)`，`new()` 保留为默认构造。
2. **配置注入**：把 `Config` 从 `main.rs` 提到 `executor::config`，全字段化，`from_env()`
   只在 `node-agent` 调用；测试直接构造。这是消掉 39 处 `env::var` 的前置条件。
3. **决策与 IO 分离**：`reconcile_once`（`reconciliation_service.rs:722`）目前把「算差异」和
   「执行动作」混在一起。抽出纯函数
   `fn plan(desired: &[Pod], actual: &PodCache, flags: Flags) -> Vec<Action>`，
   L0 直接测；`reconcile_once` 只负责按 `Action` 调 broker。
4. **命令执行抽象**：`proot` 调用收口到 `CommandRunner` trait（`run(argv, env, cwd) -> Exit`），
   生产用 `tokio::process`，测试用 `FakeRunner` 或假脚本。`build_command` 已有测试，顺着扩。
5. **时间抽象**：探针/退避用 `tokio::time::pause()` 就够，不必自己造 Clock。
6. **Android 拆纯逻辑**：`KubeconfigParser`（无 `android.*`，`java.util.Base64` 或注入解码器）、
   `AgentClient` 接口、`Repository` 接收接口；`KubeconfigCrypto` 定义 `Cipher` 接口便于假实现。
7. **dev-dependencies**（Rust）：`wiremock`（或 `httpmock`）、`tempfile`、`assert_cmd`、
   `tokio = { features = ["test-util"] }`；可选 `insta` 做 JSON 快照。

### 7.5 样例：k3s 注册（L1）

```rust
use wiremock::{Mock, MockServer, ResponseTemplate};
use wiremock::matchers::{method, path};

#[tokio::test]
async fn registers_node_and_maps_conflict_to_patch() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/v1/nodes"))
        .respond_with(ResponseTemplate::new(409))
        .expect(1)
        .mount(&server).await;
    Mock::given(method("PATCH")).and(path("/api/v1/nodes/dn-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(node_json()))
        .expect(1)
        .mount(&server).await;

    let cfg = kube::Config::new(server.uri()).build().unwrap();
    let broker = ControlPlaneBrokerImpl::with_client("dn-test".into(), 10255,
                                                    kube::Client::try_from(cfg).unwrap());
    broker.register(&profile()).await.unwrap();
}
```

同类写法可以覆盖：CSR 创建/批准、心跳 PATCH、Pod watch 的 chunked 流。

### 7.6 样例：Pod 决策（L0，最高性价比）

```rust
#[test]
fn plan_is_idempotent_and_recreates_on_restart() {
    let desired = vec![pod("p1", "sha256:aaa")];
    let actual = cache_with(vec![]);              // 本地还没有
    assert_eq!(plan(&desired, &actual, Flags::default()), vec![Action::Start("p1")]);

    let actual = cache_with(vec![ready("p1", "sha256:aaa")]);
    assert!(plan(&desired, &actual, Flags::default()).is_empty()); // 幂等

    let actual = cache_with(vec![exited("p1", "sha256:aaa", code = 137)]);
    assert!(matches!(plan(&desired, &actual, ..)[0], Action::Restart(_)));
}
```

这条测的是**调度/运行的核心判断**，不需要 k3s、不需要手机，却是最容易回归的部分。

### 7.7 样例：容器启动（L2，假 proot）

```rust
#[tokio::test]
async fn proot_receives_rootfs_and_env() {
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("proot");
    // 把 argv / env 写到文件，退出码可控
    std::fs::write(&fake, "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.args\"\nenv > \"$0.env\"\nexit ${FAKE_EXIT:-0}\n").unwrap();
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap(); }

    let broker = ProotBrokerImpl::new(fake.clone());
    let code = broker.run(spec()).await.unwrap();
    assert_eq!(code, 0);
    let args = std::fs::read_to_string(format!("{}.args", fake.display())).unwrap();
    assert!(args.contains("-r"));
    assert!(args.contains(spec().rootfs.to_str().unwrap()));
}
```

### 7.8 样例：Android kubeconfig（JUnit，无设备）

```kotlin
class KubeconfigParserTest {
    @Test fun `token kubeconfig exposes server and token`() { … }
    @Test fun `pkcs1 private key is wrapped into pkcs8`() { … }   // 之前踩过 k3s 的 EC 私钥
    @Test fun `missing server line fails with a typed error`() { … }
}
```

`Tr` 的一致性也能用 JUnit 守：

```kotlin
@Test fun `every Tr.s key exists in the map`() {
    val keys = Tr.keysUnderTest()            // 反射或生成一份 key 列表
    keys.forEach { assertNotNull(Tr.idFor(it), "missing translation for $it") }
}
```

### 7.9 CI 改造

```yaml
jobs:
  rust-unit:        # 现有 job，补 cargo fmt --check 与 --all-features
  rust-integration: # cargo test --workspace（含 wiremock 用例），< 2 分钟
  rust-e2e-k3d:     # nightly / 手动 label：k3d + feature fake-runtime + 部署 hello pod
  android-unit:     # ./gradlew testDebugUnitTest
  android-lint:     # ./gradlew lintDebug
  android-build:    # 现有 compile → 改成 assembleDebug（stub jniLibs）
```

- 覆盖率：Rust 用 `cargo llvm-cov --workspace --lcov`，Android 用 JaCoCo；先只对
  `executor/src/services`、`executor/src/brokers`、`android/.../data` 设阈值（如 40%），
  不要求全仓库。
- 稳定性：L3 e2e 允许 1 次重试；失败用例进 quarantine 列表，不允许「静默跳过」。
- 缓存：沿用现有 cargo/gradle cache，k3d job 单独 cache k3s 镜像层。

### 7.10 落地顺序（按 ROI）

| 优先级 | 测试 | 为什么先做 | 工作量 |
|---|---|---|---|
| 1 | Android `KubeconfigParser` + 私钥包装 | 安全相关、纯逻辑、零依赖 | 0.5 天 |
| 2 | Reconcile `plan()` 决策函数 | 核心循环、纯函数、回归价值最高 | 1 天 |
| 3 | 注册/心跳/CSR 的 wiremock 用例 | k3s 连接主路径 | 1 天 |
| 4 | 层解包（whiteout/链接/穿越） | 装不上镜像的第一现场 | 0.5 天 |
| 5 | 假 proot 的启动/退出/日志 | pod 能不能跑起来 | 1 天 |
| 6 | exposer handlers（oneshot） | 20+ 路由，测起来便宜 | 1 天 |
| 7 | 卷/探针/`/etc/hosts` | 中等风险 | 1 天 |
| 8 | Android Repository（假 client） | 分层改造的验收 | 1 天 |
| 9 | k3d e2e（nightly） | 端到端信心 | 2 天 |
| 10 | 覆盖率门禁 | 防止回退 | 0.5 天 |

**完成标准**：上述 1–5 项合入后，核心模块单测 ≥ 60 条；L3 至少 1 条「注册 → 调度 → 运行 →
`kubectl logs` 拿到输出」的端到端用例；CI 在 PR 上跑 L0–L2（< 5 分钟），nightly 跑 L3。

---

## 8. 附录：度量

| 指标 | 数值 |
|---|---|
| Rust 源文件 / 行数 | 31 / 9,937 |
| Rust 单元测试 | 15 个 `#[test]`，分布在 7 个文件；无 `tests/` 集成测试 |
| Rust `unwrap/expect` | 45 处（含测试；`virtual_kubelet_exposer` 12 处且该文件无测试） |
| Rust 最大文件 | `virtual_kubelet_exposer.rs` 2,266 行 |
| Kotlin 源文件 / 行数 | 40 / 5,205 |
| Kotlin 测试 | 0 |
| Kotlin `object` 单例 | 16 |
| Kotlin 最大文件 | `SettingsActivity.kt` 636 行 |
| Android 依赖 | 仅 `core-ktx` + `coroutines-android`（无 lifecycle / compose / DI） |
| CI | Rust check+clippy+test、aarch64 交叉检查、Android Kotlin 编译；无 lint / 无 Android 测试 / 不产 APK |
| 文档 | `docs/` 9 篇 + `prototype/` + CHANGELOG，覆盖能力、限制、基准、引导、i18n |

---

## 9. 一句话总结

> 这是一个「底层工程判断不错、上层 UI 赶进度、测试严重欠账」的项目：Rust 侧的端口/适配器和
> 控制循环值得保留，Android 侧需要一次以「测试先行 + Repository/ViewModel」为核心的
> 结构性整理；安全上先解决 loopback 全信任，工程上先把 CI 从「能编译」升级到「能回归」。
