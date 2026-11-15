# 测试与优化计划（当前）

> 更新于 `0.1.30`。只保留**还没做完的事**：覆盖率门禁、Rust 巨文件、请求路径 panic 风险、
> 错误类型收敛、端口/轮询的小结构问题，以及对应的测试方案。已完成项见第 1 节，不再展开。

## 0. 基线

| 指标 | 当前值 | 备注 |
|---|---|---|
| Rust 文件 / 行数 | 47 / 11,082 | 含测试 |
| Rust 测试 | **62**（42 `#[test]` + 20 `#[tokio::test]`） | 真机 62/62（`scripts/rust-test-device.ps1`），不依赖 CI |
| Rust `env::var` | **1 处**（`config.rs`）+ 2 处测试 | 全部经 `executor::config::RuntimeConfig` |
| Rust 最大文件 | `reconciliation_service.rs` 964（policy/watch 已拆出） | 其次 872 / 640 / 561 / 498 / 470 |
| `unwrap/expect` | 116 处（含测试） | 非测试占比待审计（R12） |
| Kotlin 文件 / 行数 | 46 / 5,123 | |
| Android 单测 | **32**（JVM） | 本机通过（含 `PortDefaultsTest`） |
| Android 仪器测试 | **7**（真机） | 加密 2 + Tr 1 + 语言 1 + Pod 解析/标签 3 |
| 最大 Kotlin 文件 | `SettingsActivity.kt` **216 行** | 已拆出 Renderer/Binder/Actions |
| CI | 4 job | rust、rust-android、android、coverage（只出报告，无门禁） |
| APK | 12.73 MB | `0.1.38` debug |

## 1. 已完成（不再跟踪）

- **测试执行**：Rust 62 例真机全过；Android JVM 32、仪器 7；真机修复 5 个 Rust 测试问题、3 个 docker.io 用例加跳过开关。
- **结构**：`SettingsActivity` 569→216（Renderer/Binder/Actions）；exposer 2266→214；`ControlPlaneBroker` 拆 4 个窄 trait；`LocalConfig` 128 行门面；`data/` 层与 `NodeRepository`。
- **错误/配置**：`DroidError::IntoResponse` + `Conflict`；`env::var` 39→1。
- **真机**：CA 集合信任、D3 特权端口提示一次、端口标签随 IPC、SIGTERM 0.35s 退出、恢复脚本、冒烟脚本（bash/PowerShell）。
- **流程**：仪器测试改手动 `am instrument`（不卸载 app）；`build-apk-win.ps1` 打包前清理旧 APK。

## 2. 未解决项

### R9 覆盖率 baseline 与门禁（唯一阻塞项）
- 现状：CI 有 `cargo llvm-cov` job 但只出报告；本机缺 `llvm-tools-preview`（50MB 下载慢），真机 `.profraw` 已能生成。
- 动作（二选一）：
  1. 装好 llvm-tools 后跑 `scripts/rust-coverage-device.ps1`；
  2. push 后跑一次 CI coverage job。
- 后续：把数字写进本文档，对 `services/`、`brokers/` 设 40% 起阈值，PR 低于阈值失败；flaky 策略（网络用例已可用 `DROIDNODE_SKIP_NETWORK_TESTS` 跳过，e2e 允许 1 次重试 + quarantine 清单且设到期时间）。
- 验收：baseline 入库；门禁生效；quarantine 有条目和期限。

### R11 Rust 巨文件拆分（进行中：1,040 → 964）
- 目标：`reconciliation_service.rs` 1,040 → ≤500；`workload_lifecycle_service.rs` 872 → ≤500；`workload_execution_service.rs` 640 → ≤500。
- 动作：
  - reconciliation：拆出 `plan`（决策）、`backoff/quota`、`watch` 三个模块，主文件只留编排；
  - lifecycle：按阶段拆（create/start/stop/cleanup），保留现有 phase 测试；
  - execution：配置目录（configmap/secret/hostPath）与进程启动分开。
- 约束：每一步先跑真机 59 例，再继续下一个；不改行为。
- 验收：行数达标，59/59 仍过，节点真机运行正常。

### R12 请求路径的 `unwrap/expect`（baseline 18，已加 CI 预算）
- 现状：116 处含测试，handler/exposer 路径仍有 `unwrap`（`virtual_kubelet_exposer` 历史遗留）。
- 动作：先加脚本统计非测试文件中的 `unwrap/expect`（排除 `#[cfg(test)]`），从 exposer/handlers/brokers 开始改成 `?` + `DroidError`；必要时加 `debug_assert`。
- 验收：非测试 `unwrap` 数量下降并记录基线；clippy 对请求路径无 `panic` 风险项。

### R13 错误类型继续收敛
- 现状：`Config`/`Unsupported`/`Workload(String)` 仍是字符串。
- 动作：把高频分支改成带字段的变体（如 `InvalidQuery{field}`、`ImageInUse{id}`），HTTP 映射测试同步扩展；保留少数确实无结构的消息。
- 验收：新增变体有单测；映射表覆盖 400/404/409/500/502/503。

### R14 端口默认值漂移 ✅
- 现状：Rust 默认 `10250`（`main.rs`），Android `AppPrefs.DEFAULT_KUBELET_PORT = 10255`，`NodeReadinessService` 还硬编码 `10255`。
- 动作：统一以 Android 的 10255 为准（Rust `Config` 默认改 10255，`NodeReadinessService` 读 `AppPrefs`），加一个默认值单测。
- 验收：三处一致，有测试锁住。

### R15 轮询聚合（可选优化）
- 现状：`SettingsActivity` 每 3 秒直接 `Snapshot.health()`；Node/Pods/Images 已走 `NodeRepository` 的 2 秒共享快照。
- 动作：把 health 并入 `NodeRepository` 快照或独立的 `StatusRepository`，设置页只订阅。
- 验收：设置页无独立轮询；后台时自动停。

### R16 `Tr` 以中文原文作 key ✅ 已决策
- 现状：`values-en` 与中文 key 一一对应，运行时校验已补；新增文案若忘翻译会静默回退中文。
- 动作：维持现状 + 保留 `TrTest`/`TrLocaleTest` 校验；在文档记录取舍（迁移到符号 key 成本高、收益有限）。
- 验收：决策写入文档，不回退。

### R10 已知限制（不修）
- D3 不能代理 <1024 端口（53/80/443 用 NodePort）；D3 只支持 TCP。
- fresh install 无 CA 时 `AgentApi` 短暂 accept-all；`KubeConnectionTest` 仍依赖 `ui/v2/Tr`。
- proot 首次启动需要网络拉取（有镜像/缓存回退）。

## 3. 下一轮测试计划

| ID | 层级 | 用例 | 状态 |
|---|---|---|---|
| R9-1 | 真机/CI | `-C instrument-coverage` → `.profraw` → `llvm-cov report`，记录 baseline | 待工具 |
| R9-2 | CI | `services`/`brokers` 覆盖率阈值 40%，低于则失败 | 待 baseline |
| R11-1 | 单元 | reconciliation 拆分后：plan/backoff/watch 现有用例全过 | 待做 |
| R11-2 | 单元 | lifecycle 拆分后：phase 用例全过 | 待做 |
| R12-1 | 单元 | 非测试 `unwrap/expect` 基线脚本（grep 排除测试模块） | 待写 |
| R12-2 | 单元 | exposer 路径错误返回 500 而非 panic（wiremock/axum 测试） | 待写 |
| R13-1 | 单元 | 新错误变体 → 状态码映射矩阵 | 待写 |
| R14-1 | 单元 | 端口默认值三处一致（Rust/AppPrefs/NodeReadinessService） | 待写 |
| R15-1 | 真机 | 设置页 3s 刷新改订阅后，页面数据仍更新 | 可选 |
| 回归 | 真机 | `rust-test-device` 59 例 + `am instrument` 4 例 + 冒烟脚本 | 每次发版 |

## 4. 优化顺序（按 ROI）

| 顺序 | 项 | 收益 | 风险 | 预估 |
|---|---|---|---|---|
| 1 | R9 baseline + 门禁 | 防止后续重构悄悄掉覆盖率 | 低（只加 CI） | 0.5 天 |
| 2 | R14 端口统一 | 消除真机「端口不一致」一类问题 | 低 | 0.5 天 |
| 3 | R11-1 reconciliation 拆分 | 最大文件、后续改动风险最高 | 中（需真机回归） | 1.5 天 |
| 4 | R12 unwrap 审计 | 减少请求路径 panic | 中 | 1 天 |
| 5 | R11-2/3 lifecycle、execution | 结构可读性 | 中 | 2 天 |
| 6 | R13 错误变体 | 可观测性/一致性 | 低 | 1 天 |
| 7 | R15 轮询聚合 | 省电、单一数据源 | 低 | 0.5 天 |

## 5. 目标（完成上述后）

| 指标 | 现在 | 目标 |
|---|---|---|
| 覆盖率 baseline | 无 | 有数字 + 40% 阈值 |
| Rust 最大文件 | 1,040 | ≤500 |
| 非测试 `unwrap/expect` | 未审计 | 有基线且下降 |
| 端口默认值 | 3 处不一致 | 1 处常量 |
| Rust 测试 | 62 | ≥70 |
| Android 测试 | 31 + 4 | ≥35 + 6 |
| CI | 报告 | 报告 + 门禁 |

## 6. 每次发版的真机回归清单

1. `scripts/rust-test-device.ps1 -Serial <serial>`：59 例全过。
2. 手动 `am instrument`：`TrLocaleTest`、`TrTest`、`KubeconfigCryptoTest` 4 例全过（数据不丢）。
3. `scripts/smoke-device.ps1`：安装→启动→进程/前台/崩溃/截图。
4. 节点页运行中、Pod 数正确、kubelet 端口与 IPC 一致。
5. 设置 → 连接测试返回集群版本；语言/主题切换正常。
6. 改端口→重启→端口跟随→改回；`kill -TERM` 0.35s 退出。
7. 记录版本、APK sha256、设备型号。
