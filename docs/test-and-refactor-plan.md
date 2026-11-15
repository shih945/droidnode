# 阶段计划：测试回归 + 优化整理

> 基于当前 `master`（0.1.8，6 个提交）的实际情况制定。
> 配套阅读：[架构与代码组织评审](architecture-review.md)（问题清单与测试矩阵在第 4、7 节）。
> 原则：**每个阶段同时交付「测试」和「清理」**——只加测试不改结构，测试写不动；只重构不加测试，
> 回归没保障。每阶段独立可验收、可回滚，结束打一个 `0.2.x` 版本。

## 0. 基线（今天的状态）

| 指标 | 当前值 | 备注 |
|---|---|---|
| Rust 文件 / 行数 | 31 / 9,937 | `agent/executor` + `node-agent` |
| Rust 测试 | **15** 个 `#[test]` | 集中在纯函数；无 `tests/` 集成测试 |
| Rust `env::var` | **39** 处 / 8 个文件 | `Config` 只有 5 个字段 |
| Rust `unwrap/expect` | 45 处（含测试） | exposer 12 处且无测试 |
| Rust > 600 行的文件 | 4 个 | 最大 `virtual_kubelet_exposer.rs` 2,266 行 |
| Kotlin 文件 / 行数 | 40 / 5,205 | 仅 `core-ktx` + coroutines |
| Kotlin 测试 | **0** | `src/` 下只有 `main/`、`debug/` |
| Kotlin > 600 行的文件 | 1 个 | `SettingsActivity.kt` 636 行 |
| CI | 3 个 job | Rust check/clippy/test、aarch64 交叉 check、Android Kotlin 编译 |
| APK | 12.39 MB | `0.1.8` debug |

## 1. 阶段总览

| 阶段 | 周期 | 主题 | 测试产出 | 清理产出 |
|---|---|---|---|---|
| **P0** | 第 1 周 | 止血 + 铺路 | Android 解析类 8 例、`plan()` 5 例 | 证书 pin、CI 补 fmt/lint 位、删死代码 |
| **P1** | 第 2 周 | 核心链路回归网 | 注册/心跳/CSR、watch、解包、假 proot、exposer、IPC（≥30 例） | 抽 `config`、`CommandRunner`、`plan()` |
| **P2** | 第 3–4 周 | Android 分层 | `data/` 层 ≥30 例、Repository 替身 | `LocalConfig` 拆分、`SettingsActivity` 拆分、Repository/StateFlow |
| **P3** | 第 5–6 周 | Rust 结构整理 | exposer/卷/探针/D3 ≥20 例、k3d e2e 1 条 | 拆 exposer/ControlPlaneBroker、错误结构化、退出生命周期 |
| **P4** | 第 7–8 周 | 硬化与门禁 | 覆盖率门禁、flaky 治理、真机脚本化冒烟 | 文档收尾、脚本去重、发布 0.2.0 |

总体目标（第 8 周末）：

| 指标 | 目标 |
|---|---|
| Rust 测试 | 15 → **≥ 70**（含 ≥ 8 个 wiremock 集成用例、1 条 k3d e2e）；P4 按覆盖率再补到 ≥ 85 |
| Android 测试 | 0 → **≥ 25**（纯 JVM，无设备）；P4 再补到 ≥ 35 |
| 最大文件 | 2,266 → **≤ 600 行** |
| `env::var` 分布 | 39 处 / 8 文件 → **≤ 8 处 / `config.rs` 1 文件**（其余为显式运行期覆盖） |
| CI | 3 job → **6 job**，PR 上 L0–L2 < 5 分钟，nightly 跑 k3d |
| 覆盖率 | 核心模块（`services/`、`brokers/`、`android/.../data`）**≥ 50%** |

---

## 2. P0（第 1 周）：止血 + 铺路

**目标**：把「测试写不进去」的几个口子先打开，同时处理一个安全问题和一个 CI 缺口。不改任何用户可见行为。

### 测试

| # | 项 | 说明 | 预估 |
|---|---|---|---|
| T1 | 抽 `KubeconfigParser` | 从 `ui/v2/KubeConnectionTest.kt` 剥出纯 Kotlin 解析（kubeconfig YAML → server/CA/token/客户端证书；PKCS#1/SEC1 → PKCS#8）。不依赖 `android.*` | 0.5 天 |
| T2 | 补 8 个 JUnit | token / 客户端证书 / 无凭据 / 缺 server / 非法 YAML / PKCS#1 / EC SEC1（P-256、P-384）/ 证书 base64 解码 | 0.5 天 |
| T3 | 抽 `plan()` | 从 `reconciliation_service.rs:722` 剥出纯函数：`plan(desired, actual, flags) -> Vec<Action>` | 0.5 天 |
| T4 | 补 5 个单测 | 新增启动、幂等、镜像变化重建、退出码 137 重启、排空后可恢复 | 0.5 天 |

### 清理

| # | 项 | 说明 |
|---|---|---|
| C1 | `AgentApi` 证书 pin | 至少改成校验 `kubelet-ca.crt` 签发的 serving 证书，或在 `files/agent.sock` 上走 unix socket（更彻底） |
| C2 | 删 v1 死代码 | `com.droidnode.ImagesActivity`、`ui.DebugActivity` 及对应布局/字符串（`MainActivity` 已还原为无界面） |
| C3 | CI 补位 | Rust 加 `cargo fmt --check`；Android 加 `lintDebug`（先不设阻断，产出 baseline）；新建 `android-unit` job 占位（此时可只跑空测试） |

### 验收

- `cargo test` 全绿，测试数 15 → ≥ 25；`./gradlew testDebugUnitTest` 有 ≥ 8 个用例并通过。
- `AgentApi` 不再全信任任意证书（代码评审 + 抓包自签证书被拒）。
- APK 体积不增加（删死代码后略降），真机 smoke：启动、节点页、设置页正常。

### P0 完成情况（2026-11-15，0.1.9）

| 项 | 状态 | 证据 |
|---|---|---|
| T1/T2 kubeconfig 解析与私钥包装 | ✅ | `KubeConnectionTestTest` 8 例全过（`gradlew testDebugUnitTest`，JVM，无需设备） |
| T3/T4 reconcile 决策 | ✅（先抽三个纯函数） | `want_restart` / `effective_grace` / `terminal_for_policy` + 6 例；`cargo check --tests --target aarch64-linux-android` 通过，用例由 CI 的 ubuntu job 执行 |
| C1 证书 pin | ✅ | `AgentApi.configure()` 只信任 `files/kubelet-ca.crt`；没有 CA 文件（全新安装）时才退回旧的宽松策略 |
| C2 删 v1 死代码 | ✅ | 删除 `ImagesActivity`(v1)、`ui/DebugActivity`、`activity_debug.xml` 与 Manifest 条目；APK 12.39 → 12.36 MB |
| C3 CI 补位 | ✅ | `cargo fmt --check`、Android `testDebugUnitTest`、`lintDebug`（暂不阻断） |

**与计划的偏差**：

- `plan()` 没有一步到位抽成完整函数，先抽了三个「策略判断」纯函数（重启策略/删除宽限/终态），
  完整 `plan(desired, actual) -> Vec<Action>` 留到 P1 和 watch 测试一起做，避免一次改动过大。
- `KubeconfigParser` 仍留在 `ui/v2`，只是把 Base64 换成 JVM 版、把两个函数放开到 `internal`；
  搬到 `data/` 放 P2 与 Repository 一起做。
- Rust 测试在 Windows 本机跑不了（`libc`/proot 只在 Linux/Android 编译），本地只能用
  `cargo check --tests --target aarch64-linux-android` 做类型检查，实际执行交给 CI。

---

## 3. P1（第 2 周）：核心链路回归网

**目标**：k3s 连接、Pod 收敛、镜像解包、容器启动这四条主链路第一次拥有自动化回归。

### 测试

| # | 项 | 层级 | 用例数 | 说明 |
|---|---|---|---|---|
| T5 | 注册 / 心跳 / CSR | L1 wiremock | ≥ 6 | 201 创建、409→PATCH、401→错误类型、CSR 提交与批准、status PATCH 周期 |
| T6 | Pod watch | L1 | ≥ 3 | ADDED/MODIFIED/DELETED 流 → 缓存一致；断流 → LIST 兜底；RESET 全量 |
| T7 | 层解包 | L0 tempdir | ≥ 5 | 普通文件、目录、符号链接、硬链接、whiteout、路径穿越拒绝 |
| T8 | 假 proot | L2 | ≥ 4 | argv（`-r` rootfs、绑定挂载）、env、退出码 0/1/137、日志采集 |
| T9 | exposer handlers | L1 `tower::oneshot` | ≥ 4 | `/healthz`、`/pods`、`/containerLogs`（tail/since）、`/images` 删除 409 |
| T10 | status IPC | L1 unix socket | ≥ 2 | 正常往返；非法 JSON / 客户端断开不崩 |

### 清理（为测试服务）

| # | 项 | 说明 |
|---|---|---|
| C4 | `executor::config::Config` | 全字段、`from_env()` 只在 `node-agent`；service 不再读 env（先改 exposer 的 12 处） |
| C5 | `CommandRunner` 抽象 | proot 的 spawn 收口；生产 `tokio::process`，测试替身 |
| C6 | `with_client()` | `ControlPlaneBrokerImpl` 支持注入 `kube::Client`（wiremock 前提） |
| C7 | 测试依赖 | `wiremock`、`tempfile`、`assert_cmd`、`tokio/test-util` 进 dev-dependencies |

### 验收

- L0–L2 测试总数 ≥ 45，`cargo test` < 60 秒；PR CI（含 Android 编译 + 单测）< 5 分钟。
- 覆盖率首次出数（`cargo llvm-cov`），记录 baseline 到 `docs/benchmark.md` 或 CI artifact。
- 真机 smoke 不变：k3s 节点 Ready、Pod 能起、日志可看。

### P1 进度（2026-11-15）

| 项 | 状态 | 说明 |
|---|---|---|
| C6 `with_client()` | ✅ | `ControlPlaneBrokerImpl::with_client(node, port, client)`，`new()` 复用；wiremock 的前提已就位 |
| T7 层解包 | ✅ 3 例 | whiteout 删除下层文件、软链原样解出、延迟硬链接回退拷贝 |
| T9 exposer | ✅ 2 例 | `/healthz` 返回 200 + `ok`；未知路由 404（`tower::ServiceExt::oneshot`） |
| T10 IPC | ✅ 2 例 | `AgentStatusReply` 序列化字段；遥测反序列化 |
| T5 注册/心跳 | ✅ 2 例（wiremock） | `POST /api/v1/nodes` 的 name/labels；心跳 PATCH `/status` 的 Ready 条件与 kubelet 端口。运行时待在 Linux/CI 上跑一次 |
| T6 watch 缓存 | ✅ 3 例 | `apply_watch_event()` 从 watch 循环抽出：Applied 插入、Deleted 删除、Restarted 全量替换并置 ready。真正的流式 watch 测试仍留给 CI |
| T8 假 proot | ✅ 1 例 | 假 `proot` 脚本记录 argv 并以 7 退出；断言 `-r <rootfs>` 与退出码透传。运行时待 Linux/CI |

其他进展：心跳条件构造抽成 `condition_patch()`（2 例，覆盖 `lastTransitionTime` 只在状态翻转时变化）；
网络分类 4 例（私有地址、公网地址、tether 接口、可用接口）；测试总数 21 → 39
（含 `#[tokio::test]`）。

**本机验证限制**：Rust 测试只能在 Linux/Android 上编译运行。Windows 上验证方式是
`cargo check --tests --target aarch64-linux-android`（已通过）+ `cargo fmt --check`（已通过）；
实际执行需要 CI 的 ubuntu job，或把分支推到远端跑一遍。

---

## 4. P2（第 3–4 周）：Android 分层 + 测试

**目标**：把 UI 从「Activity 直连单例」改成「Activity → ViewModel → Repository → AgentClient」，并让数据层可测。

### 测试

| # | 项 | 用例数 | 说明 |
|---|---|---|---|
| T11 | `KubeconfigStore` / 加密 | ≥ 5 | 加密落盘 → 解密还原；密文不含明文；Keystore 失败的降级路径（用假 Cipher） |
| T12 | 校验规则 | ≥ 6 | 端口 1024–65535、节点名正则、镜像源主机名、语言/外观枚举 |
| T13 | `Tr` 一致性 | ≥ 2 | 反射遍历 map：每个 `Tr.s("…")` 的 key 都有资源；中英文 key 对齐 |
| T14 | `Repository` | ≥ 6 | 假 `AgentClient`：成功/超时/401；并发刷新只发一次请求；缓存过期 |
| T15 | 文件格式 | ≥ 4 | `registry-mirrors.env` 读写、`network-preference.env` 读写、非法行忽略 |

### 清理

| # | 项 | 说明 |
|---|---|---|
| C8 | `LocalConfig` 拆分 | → `AppPrefs` / `RegistryMirrorStore` / `KubeconfigStore` / `NetworkInterfaces`（各 < 100 行） |
| C9 | `SettingsActivity` 拆 | → `SettingsViewModel` + 4 个 section binder（控制面、节点、运行、外观+诊断）；目标 ≤ 250 行 |
| C10 | 数据层搬家 | `AgentApi`、`KubeConnectionTest`（改名 `KubeconfigParser` + `ConnectionTester`）移到 `data/`；`ui/v2` 只留 UI |
| C11 | 单一数据源 | 5 个页面改为共享 `Repository.state: StateFlow`，删各自的 `while(isActive)` 轮询 |

### 验收

- `SettingsActivity` ≤ 250 行、`LocalConfig` 文件消失；最大 Kotlin 文件 ≤ 400 行。
- Android 单测 ≥ 30，`./gradlew testDebugUnitTest` < 90 秒。
- 行为验证：切 5 个 tab 时同一份数据只拉一次（日志计数/抓包），语言与外观切换仍即时生效。

---

## 5. P3（第 5–6 周）：Rust 结构整理 + 生命周期

**目标**：消掉四个巨文件，补齐剩余主功能的测试，修掉退出路径。

### 测试

| # | 项 | 用例数 | 说明 |
|---|---|---|---|
| T16 | 卷挂载 | ≥ 5 | configMap/secret 文件与权限、emptyDir 共享、hostPath 越界拒绝 |
| T17 | 探针 | ≥ 4 | http 200/500、tcp 拒绝、exec 超时；`tokio::time::pause` |
| T18 | D3 / hosts | ≥ 3 | ClusterIP→本地端口、`/etc/hosts` 内容、端口冲突 |
| T19 | 拆模块后的 handler | ≥ 8 | 按 logs/exec/stats/images 分组回归 |
| T20 | k3d e2e | 1 条 | 注册 → 调度 → 运行 → `kubectl logs` 拿到输出（nightly） |

### 清理

| # | 项 | 说明 |
|---|---|---|
| C12 | exposer 拆文件 | `exposers/kubelet/{logs,exec,stats,images,health}.rs` + `tls.rs`，单文件 ≤ 500 行 |
| C13 | `ControlPlaneBroker` 拆 trait | `NodeApi` / `PodWatch` / `EventRecorder` / `PvcApi`（17 个方法 → 4 个窄接口） |
| C14 | 错误结构化 | 关键变体带字段 + `IntoResponse` 集中映射状态码 |
| C15 | 退出生命周期 | `CancellationToken`，SIGTERM/SIGINT 单次优雅退出；修掉「按两次 Ctrl-C」 |
| C16 | 默认值单一来源 | kubelet 端口默认值只留一处（App 从 IPC 读实际端口） |

### 验收

- 无 > 600 行文件；`env::var` 只在 `config.rs`（≤ 8 处）。
- `kill -TERM` 后 2 秒内退出且完成 deregister（日志可见），无 panic。
- nightly e2e 连续 5 天绿。

---

## 6. P4（第 7–8 周）：硬化与门禁

| 项 | 内容 | 验收 |
|---|---|---|
| 覆盖率门禁 | `cargo llvm-cov` + JaCoCo，只对核心模块设阈值（50%） | 覆盖率下降的 PR 失败 |
| Flaky 治理 | e2e 允许 1 次重试；失败用例进 quarantine 清单，必须限期修复 | 30 天 quarantine 为空 |
| 真机冒烟脚本化 | 把现有 adb 步骤（安装 → 启动 → 加入集群 → 起 Pod → 看日志）写成 `scripts/smoke-device.sh` | 一条命令出结论 |
| 文档收尾 | 架构评审里的 P1/P2 逐条标记「已修/接受」；更新 README 的能力表 | 评审清单 100% 有结论 |
| 脚本去重 | `build-apk-win.ps1` / `build-android.sh` 共用一份工具链探测 | 两份脚本都变薄 |
| 发布 | `0.2.0`，CHANGELOG 汇总 | 版本号 + 说明 |

---

## 7. 测试矩阵 → 阶段映射

| 功能 | P0 | P1 | P2 | P3 | P4 |
|---|---|---|---|---|---|
| k3s 连接 / 注册 / CSR / 心跳 | | ✅ | | | e2e |
| Pod watch / reconcile 决策 | ✅ `plan()` | ✅ watch | | | e2e |
| 容器启动（proot） | | ✅ | | | e2e |
| 镜像拉取 / 解包 | | ✅ 解包 | | | |
| 卷 / 探针 / D3 | | | | ✅ | |
| status IPC | | ✅ | | | |
| exposer handlers | | ✅ 4 个 | | ✅ 8 个 | |
| Android kubeconfig / 加密 / 校验 | ✅ 解析 | | ✅ 加密+校验 | | |
| Android Repository / 页面 | | | ✅ | | 冒烟 |
| 真机端到端 | 手工 | 手工 | 手工 | 手工 | 脚本化 |

## 8. CI 演进

| 阶段 | jobs | PR 时长 | 门禁 |
|---|---|---|---|
| 现在 | rust、rust-android、android-compile | ~4 min | 编译通过 |
| P0 | + android-unit、rust-fmt | ~5 min | fmt + 单测 |
| P1 | + lint（非阻断） | ~5 min | 单测 ≥ 45 |
| P2 | + android-unit 扩容、jacoco | ~6 min | Android 单测 ≥ 30 |
| P3 | + nightly `rust-e2e-k3d` | PR ~6 min / nightly ~15 min | e2e 绿 |
| P4 | + 覆盖率门禁、flaky quarantine | ~7 min | 覆盖率不降 |

## 9. 风险与应对

| 风险 | 应对 |
|---|---|
| 功能需求插队，计划被打断 | 阶段拆成 2–3 个 PR；P0/P1 是不可跳过的地基，其余可顺延 |
| 测试改造触碰热路径引入回归 | 每次只改一个 seam（Config → Client → CommandRunner），改完先跑 L0–L2 + 真机 smoke |
| k3d e2e 不稳 | nightly + 1 次重试 + quarantine；e2e 失败不阻塞日常 PR |
| 覆盖率虚高（只测简单分支） | 只在核心模块设门槛，评审看用例意图；关键分支要求「失败路径」用例 |
| 设备不在线 | L0–L2 不依赖设备；L4 冒烟按需执行，结果记录在 CHANGELOG |
| 时间估算偏乐观 | 每阶段末复盘，按实际速度调整后续阶段；宁可缩减范围不缩减「测试+清理」的配对原则 |

## 10. 每阶段 Definition of Done

1. 新增测试数量达标，且覆盖该阶段列出的功能点（含失败路径）。
2. 清理项完成，无用户可见行为变化；真机 smoke 通过（安装、启动、节点页、起一个 Pod）。
3. CI 全绿；PR 时长不超过表中上限。
4. 版本号 +1（`0.1.x` → `0.2.x` 在 P2/P4），`docs/CHANGELOG.md` 有对应条目。
5. 架构评审文档里相关条目更新状态（已修 / 接受 / 推迟）。

## 11. 本周就能开始的 5 件事

1. `KubeconfigParser` 抽取 + 8 个 JUnit（T1/T2）。
2. `plan()` 抽取 + 5 个单测（T3/T4）。
3. `ControlPlaneBrokerImpl::with_client()` + 第一个 wiremock 注册用例（T5 的第一条）。
4. CI 加 `cargo fmt --check` 和 `android-unit` 占位（C3）。
5. 删 v1 `ImagesActivity` / `DebugActivity`（C2），顺带确认 APK 体积变化。
