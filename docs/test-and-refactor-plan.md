# 测试与架构整改计划（当前）

> 更新于 `0.1.26`（USB 真机 PGJM10 验证后）。只保留**还没解决的问题、要做的修改和下一轮测试计划**；
> 已完成的阶段（P0–P3、真机修复）只列在「已关闭」里，不再展开。

## 0. 当前基线

| 指标 | 当前值 | 备注 |
|---|---|---|
| Rust 文件 / 行数 | 47 / 11,082 | 含测试 |
| Rust 测试 | **58**（39 `#[test]` + 19 `#[tokio::test]`） | 本机只能 `cargo check --tests --target aarch64-linux-android`，未实际执行 |
| Rust `env::var` | 39 处 / 8 个文件 | `Config` 仍只有 5 个字段 |
| Rust 最大文件 | `reconciliation_service.rs` 1,040 行 | 其次 `workload_lifecycle_service.rs` 872、`workload_execution_service.rs` 642 |
| Kotlin 文件 / 行数 | 46 / 5,123 | |
| Android 单测 | **31**（JVM） | 全部本机通过 |
| Android 仪器测试 | **2**（真机） | `KubeconfigCryptoTest`，PGJM10 通过 |
| 最大 Kotlin 文件 | `SettingsActivity.kt` **569 行** | 目标 ≤250 |
| CI | 4 job | rust（fmt/check/clippy/test）、rust-android、android（单测/lint/编译）、coverage（仅出报告） |
| APK | 12.38 MB | `0.1.26` debug |

## 1. 已关闭（不再跟踪）

- P0–P3 主体：CI 三件套、v1 死代码、`data/` 层（`AgentApi`/`KubeconfigCrypto`/`KubeConnectionTest`/`AppPrefs`/`KubeconfigStore`/`NetworkInterfaces`/`RegistryMirrorStore`/`Validation`）、`NodeRepository` 单一数据源、`LocalConfig` 拆成 128 行门面、`SettingsController`/`SettingsText`、exposer 2266→214 行 + handlers 模块化、`ControlPlaneBroker` 拆成 4 个窄 trait、`DroidError` 统一 HTTP 映射、SIGINT/SIGTERM 单次退出。
- 测试 T1–T12、T14–T19（含 D3 hosts、卷、探针、handler、IPC、Repository、i18n key 一致性）。
- 真机修复：CA pin → CA 集合信任（0.1.25）、D3 特权端口只提示一次（0.1.24）、端口标签随 IPC 刷新（0.1.26）、冒烟脚本（bash + PowerShell）。
- T11 加密落盘：真机仪器测试 2/2 通过。

## 2. 待解决问题（按优先级）

### R1（阻塞）CI 还没真正跑过 Rust 测试和覆盖率
- 现状：58 个 Rust 测试、`cargo llvm-cov` job 都只在 CI 配置里，本机 Windows/无 Linux 环境无法执行；覆盖率 baseline 为空。
- 影响：无法确认 wiremock/探针/卷这些用例运行时是否真通过；无法设覆盖率门禁。
- 动作：推送后跑一次 CI；失败项逐个修；把 `cargo llvm-cov report` 数字记录到本文档。
- 验收：CI 全绿，Rust 测试实际通过数 = 58；覆盖率数字入库。

### R2（流程）仪器测试会卸载 app、清空数据
- 现状：`connectedDebugAndroidTest` 结束后 app 被卸载，kubeconfig/节点名丢失（本轮已用 `/sdcard/Download/droidnode-kubeconfig` + 手写 prefs 恢复）。
- 动作：加 `scripts/device-restore.ps1/.sh`：检查/导入 `/sdcard/Download/droidnode-kubeconfig`、写回节点名/端口/语言/自启；跑仪器测试前先执行。
- 验收：清空数据后一条命令恢复 dn-k10；文档写明流程。

### R3 C9：`SettingsActivity` 569 → ≤250 行
- 现状：`render()`、`renderRunControl()`、`renderInterfaces()`/`addIfaceOption()`、`agentReportText()`、`registryFromLogs()` 仍在 Activity 里，直接持有 30+ 个 View 引用。
- 动作：`SettingsViews`（View 引用）→ `SettingsRenderer`（渲染 + `suppress*` 标志）；Activity 只留绑定、监听和 controller 调用。
- 验收：≤250 行；亮/暗、中/英、网卡选择、镜像源提示在真机上与现在一致（截图对比）。

### R4 D3 无法代理 <1024 端口
- 现状：Android 非 root 不能 bind 53/80/443；D3 已只提示一次并建议 NodePort，但 DNS/HTTPS ClusterIP 实际不可用；UDP 完全不支持。
- 动作（二选一，需拍板）：
  1. 接受限制：文档 + 设置页/诊断里显示「N 个 Service 因特权端口被跳过」；
  2. 另做方案（需要 root 或系统权限，当前不可行）。
- 验收：诊断可看到跳过数量，或明确记录为已知限制。

### R5 T13：`Tr` 运行时一致性
- 现状：中英 `strings.xml` key 集合一致（JVM 已测），但 `Tr.s("…")` 找不 key 时会静默回退中文，没有运行时测试。
- 动作：`androidTest` 里遍历 `Tr` 的 map（加 `internal fun entriesForTest()`），断言每个 key 都有资源、en 下不等于 key。
- 验收：仪器测试覆盖全部 Tr 条目。

### R6 配置读取没有收口
- 现状：`env::var` 39 处 / 8 文件，`Config` 只有 5 个字段；`ReconciliationService` 1040 行里还有 env 读取。
- 动作：`executor::config::Config` 全字段化，`from_env()` 只在 `node-agent` 调用，深层模块不再读 env。
- 验收：`env::var` 只出现在 `config.rs` 与 `node-agent/main.rs`（≤10 处）。

### R7 C14 只做了一半
- 现状：`DroidError` 有 `IntoResponse`，但 handlers 仍手写 `(StatusCode, Json)` 响应。
- 动作：把 exec/images/stats 等 handler 的重复错误分支改成 `Result<_, DroidError>`；保留有自定义 body 的少数分支。
- 验收：handler 里不再重复拼错误 JSON。

### R8 SIGTERM 优雅退出未真机验证
- 现状：代码改成单次 SIGINT/SIGTERM 并只 deregister 一次，但没有在设备上验证过。
- 动作：`adb shell kill -TERM <pid>`，确认日志出现 deregister 且进程退出 <2s，不残留 socket。
- 验收：设备上验证通过，记录日志片段。

### R9 覆盖率门禁 / flaky 策略（依赖 R1）
- 动作：拿到 baseline 后对 `executor/src/{services,brokers}` 设阈值（先 40%）；e2e 允许 1 次重试 + quarantine 清单。
- 验收：覆盖率下降的 PR 失败；quarantine 有期限。

### R10 已知但暂不修
- D3 只支持 TCP；fresh install（无 CA）时 `AgentApi` 仍短暂 accept-all；`KubeConnectionTest` 在 `data/` 里仍引用 `ui/v2/Tr`。

## 3. 修改计划（按顺序）

| 阶段 | 内容 | 依赖 | 预估 | 验收 |
|---|---|---|---|---|
| **A** | 跑 CI（R1），修失败用例，记录覆盖率 | 需要推送 | 0.5–1 天 | CI 绿、baseline 入库 |
| **B** | 设备恢复脚本（R2）+ SIGTERM 验证（R8） | 真机 | 0.5 天 | 一条命令恢复；退出日志正确 |
| **C** | `SettingsRenderer` 拆分（R3） | 真机验证 | 1–1.5 天 | ≤250 行 + 截图对比 |
| **D** | `Tr` 仪器测试（R5） | 真机 | 0.5 天 | 全条目覆盖 |
| **E** | 配置收口（R6）+ handler 错误统一（R7） | CI | 2–3 天 | env ≤10 处、handler 无重复 JSON |
| **F** | 覆盖率门禁（R9）+ D3 决策落地（R4） | A | 1 天 | PR 门禁生效；诊断显示跳过数 |

## 4. 下一轮测试计划

| ID | 层级 | 用例 | 状态 |
|---|---|---|---|
| R1-1 | CI | `cargo test --workspace`（58 例，含 wiremock/proot/探针/卷） | 待跑 |
| R1-2 | CI | `cargo llvm-cov report --summary-only` 记录数字 | 待跑 |
| R2-1 | 脚本 | 卸载/清数据后用 `/sdcard/Download/droidnode-kubeconfig` 恢复 dn-k10 | 待写 |
| R8-1 | 真机 | `kill -TERM`：deregister 日志、<2s 退出、socket 清理 | 待跑 |
| R3-1 | 真机 | 设置页亮/暗、中/英、网卡选择、镜像源状态与拆分前一致 | 待跑 |
| R5-1 | 仪器 | 遍历 `Tr` 条目：每个 key 有资源、en 下不回退中文 | 待写 |
| R5-2 | 仪器 | 语言切换后 `Settings/Node/Pods` 页面无中文残留 | 待跑 |
| R4-1 | 真机 | D3 诊断显示「特权端口跳过 N 个」 | 待实现 |
| R6-1 | 单元 | `Config::from_env` 的默认值/覆盖矩阵 | 待写 |
| R7-1 | 单元 | handler 错误分支返回统一 JSON 形状 | 待写 |

## 5. 目标（下一轮结束）

| 指标 | 现在 | 目标 |
|---|---|---|
| Rust 测试实跑 | 0（仅类型检查） | 58 全绿 |
| 覆盖率 baseline | 无 | 有数字 + 40% 阈值 |
| `SettingsActivity` | 569 行 | ≤250 行 |
| `env::var` 分布 | 39 处 / 8 文件 | ≤10 处 / 2 文件 |
| 仪器测试 | 2 | ≥6（加密 + Tr + 语言回归） |
| 真机可恢复性 | 手工 | `device-restore` 一条命令 |

## 6. 每次发版的真机回归清单

1. `scripts/smoke-device.ps1 -Serial <serial>`（或 bash 版）：安装 → 启动 → 进程/前台/崩溃/截图。
2. 节点页：运行中、Pod 数、kubelet 端口与 `AgentStatus` 一致。
3. 设置 → 连接测试：返回集群版本。
4. 语言切 English 再切回简体中文；外观切深色再切回亮色。
5. 改端口 → 保存重启 → 页面端口跟随；改回。
6. `kill -TERM` agent 进程：deregister 日志、2 秒内退出。
7. 记录版本、APK sha256、设备型号到提交说明。
