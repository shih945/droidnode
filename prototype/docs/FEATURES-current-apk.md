# 现有 APK 界面功能总结

检查对象：`dist/droidnode-debug.apk`（versionName 0.1.0 / versionCode 1，2026-11-12 00:21 构建），已安装在
`SWLRGUAAKBEIDULZ`（OnePlus PGJM10，1080 × 2412，density 480）上运行。

> 检查过程中仓库被并行重建：`dist/droidnode-debug.apk` 在 00:30 被重写并重新安装（版本号不变）。
> 重新截图对比，界面布局一致，只有运行时长与内存等数值不同（原始截图见
> [`_reference/current-ui/`](../_reference/apk-baseline/current-ui/)）。
> 下面的描述与截图对应 00:21 那版，对 00:30 版同样成立。

检查方法：

1. 逐文件读界面源码（3 个 Activity + 2 个布局 + Manifest）。
2. 真机截图 + `uiautomator dump` 取控件坐标，确认实际渲染结果。
3. `adb forward tcp:11255 tcp:10255` 拉取 agent 的真实响应，确认界面拿得到、但没用的数据。
4. 只做读取和点按，没有改动 APK、没有删除镜像或重启服务。

截图与原始数据在 [`_reference/current-ui/`](../_reference/apk-baseline/current-ui/)。

---

## 1. 页面地图

```
通知栏（常驻，点击 → DebugActivity）
   │
   └── MainActivity  「节点仪表盘」  —— launcher，也是唯一入口
         ├── [电池优化白名单] → 系统设置
         ├── [镜像] → ImagesActivity
         └── [详细日志] → DebugActivity
                          ├── LOGS    实时日志流（最近 200 行）
                          ├── STATUS  运行状态
                          └── PODS    日志推断出的 Pod 列表
```

| 页面 | 文件 | 打开方式 | 有返回栈 |
|---|---|---|---|
| 节点仪表盘 | `MainActivity.kt` + `activity_main.xml` | 桌面图标 / 开机自启后不打开 | 是（根） |
| 镜像 | `ImagesActivity.kt`（纯代码布局） | 仪表盘按钮 | 是 |
| 调试台 | `ui/DebugActivity.kt` + `activity_debug.xml` | 仪表盘按钮、通知点击 | 是 |
| 通知 | `ForegroundServiceExposer.buildNotification()` | 前台服务自动出现 | —— |

没有「开始 / 停止」按钮：App 一启动就 `startForegroundService`，服务内部自动拉起 Rust agent
（[MainActivity.kt:108](../../android/app/src/main/java/com/droidnode/MainActivity.kt#L108)）。
开机自启走 `BootReceiver`。全仓没有设置页、没有 SharedPreferences / DataStore、没有 `strings.xml`。

---

## 2. 节点仪表盘（MainActivity）

整页只有两块内容：一个可滚动的等宽 `TextView`，底部三个按钮。每 2 秒重建一次整段文本。

### 2.1 显示内容

| 区块 | 字段 | 数据来源 | 备注 |
|---|---|---|---|
| 顶部 | `● RUNNING / ○ STOPPED` + 运行时长 | `ForegroundServiceExposer.isAgentRunning` / `startTime` 静态字段 | 静态字段，进程重建后可能短暂显示 STOPPED |
| 身份 | 节点名 | `ForegroundServiceExposer.nodeId`（`droidnode-` + ANDROID_ID） | 只读，长 ID 无截断、无复制 |
| 身份 | 控制面地址 | 读 `files/kubeconfig` 里的 `server:` | 未配置时显示「未配置 kubeconfig」 |
| 身份 | `kubelet 0.0.0.0:10255` | 硬编码字符串 | 与 `RustProcessBroker` 里的端口常量各写一份 |
| 身份 | 本机 IP | `NetworkInterface` 遍历取第一个 IPv4 | 不区分 Wi-Fi / 蜂窝 |
| 资源 | 内存 可用 / 总 | `ActivityManager.MemoryInfo` | |
| 资源 | 本应用 N 个进程、占用 MB | 扫 `/proc/*/status` 按 uid 累加 `VmRSS` | 包含 agent + 所有 proot 容器 |
| 资源 | CPU 8 核、本应用 x% | 采样 `/proc/*/stat` 的 utime+stime 差值 | `/proc/loadavg` 被 SELinux 拦，只能给「本应用占单核百分比」 |
| 资源 | 存储 可用 / 总 | `StatFs(filesDir)` | |
| 资源 | 电池 % / 充电状态 / 温度 | `ACTION_BATTERY_CHANGED` | |
| Pods | 名称、phase、ready、restarts | `GET https://127.0.0.1:10255/pods`，1.5 s 超时 | 失败时退回日志正则推断，并标注「日志推断」 |
| Pods | 正在拉取的镜像 | 日志正则 `pulling image image=…` | 只显示最后一条 |

### 2.2 交互

- 三个全宽按钮：`电池优化白名单 / BATTERY`（跳系统电池优化页，已在白名单时弹 Toast）、`镜像 / IMAGES`、`详细日志 / LOGS`。
- 文本可长按选择复制。
- 无下拉刷新，无点击 Pod 进详情，无停止/重启 agent。

### 2.3 实测问题

- 平台默认主题 + 硬编码颜色混搭：顶部 `DroidNode` ActionBar 是系统灰，内容区是 `#0D1117`，按钮是浅色 Material 灰白球块，三套视觉同时出现。
- 中英文混排 + 等宽字体 + 中文全角：标签「节点名 / 控制面 / kubelet / 本机 IP」用空格对齐，中文字符宽度不一致，永远对不齐。
- Pod 行显示 `ready=true restarts=0`，是协议字段直出，不是中文，也没有「重启次数」的语义（0 次不必显示）。
- 页面底部大块空白（`ScrollView` weight=1），三个按钮悬在空白上。
- 没有「节点是否可调度」的概念：低电量 drain 后界面依然显示 RUNNING，看不出节点已排空。

---

## 3. 镜像页（ImagesActivity）

纯代码构建布局：标题「镜像」+ 汇总行 + 卡片列表 + 底部「刷新列表」按钮。

| 元素 | 内容 | 来源 |
|---|---|---|
| 汇总 | `5 个镜像 · 共 368.2 MB` | `GET /images` 的 `count` / `total_size` |
| 卡片 | 镜像名 | `name` |
| 卡片 | `digest: sha256:abbc135a7d39…` | `manifest_digest` 前 19 字符 |
| 卡片 | `层 N · 压缩 x · 解包 y` | `layers` 长度、`compressed_size`、`unpacked_size` |
| 卡片 | 删除按钮 | `POST /images/delete?id=<rootfs_dir>`，请求期间禁用按钮 |
| 失败 | Toast | agent 返回 409（镜像被使用中）时原样把 JSON 弹出来 |

实测问题：

- 标题重复两次（ActionBar 的 `label="镜像"` + 页内 `<TextView>镜像</TextView>`）。
- 删除按钮占满卡片宽度、浅色高对比，是最显眼的元素，但它是破坏性操作，且没有二次确认。
- 没有任何「这个镜像被哪个 Pod 使用」的信息，也没有「拉取镜像 / 导入镜像」入口。
- 卡片间距 20 px（≈6.7 dp），在同色底上几乎看不出卡片边界，只有按钮在跳。
- 空状态只有一行灰字，没有下一步动作。

---

## 4. 调试台（DebugActivity）

顶栏（标题 + 状态徽标）、节点名条、三个 Tab、三个可滚动文本区（同时只有一个可见）。

| Tab | 渲染内容 | 数据来源 | 刷新 |
|---|---|---|---|
| LOGS | 最近 200 行，`[HH:mm:ss]` 前缀，按关键字着色（ERROR/FAILED 红、WARN 黄、PASSED/Succeeded/started 绿） | `LogBuffer`（agent stdout 环形缓冲，上限 500 行） | `LogBuffer.flow` 推送 + 自动滚到底 |
| STATUS | Agent 状态 / uptime / node id / 日志条数 | 静态字段 + `LogBuffer.snapshot().size` | 2 s |
| PODS | Pod 名、image、phase、last seen | 正则解析 agent stdout 的 `POD_EVENT ...` | 日志流变化时 |

实测问题：

- STATUS 页用 `┌─ Agent ────` ASCII 画框，中文字体下框线错位、断行，实际渲染成一堆断裂横线；页面 80% 是空白。
- PODS 页与仪表盘数据不一致：同一时刻仪表盘 `/pods` 显示 2 个 Pod，PODS 页显示 3 个（多出 `m3-env`），因为一个读接口、一个读日志（gap-list 的 E11 / E16）。
- LOGS 页没有级别过滤、没有搜索、没有暂停、没有按 Pod 过滤、不能导出；行内没有独立的时间列，长行折行后时间戳和内容混在一起。
- 顶部状态徽标同一页面内先显示 `● STOPPED` + `node: starting...`（进入瞬间），2 秒后才变 RUNNING，状态来源是进程内静态字段。
- 三个 Tab 是 `TextView` 手写点击，没有选中态以外的可达性语义。

---

## 5. 通知与后台行为

| 项 | 现状 |
|---|---|
| 通知标题/正文 | `DroidNode` / `Compute node active — tap to debug`（英文，固定文案） |
| 图标 | `android.R.drawable.ic_dialog_info` 系统图标 |
| 动作 | 只有点击 → DebugActivity；没有停止、没有状态刷新、没有 Pod 数 |
| 常驻 | `setOngoing(true)`，低重要性渠道，无角标 |
| 前台服务 | `dataSync` 类型；`PARTIAL_WAKE_LOCK` + Wi-Fi 高性能锁常驻 |
| 就绪循环 | 每 10 s 评估「电量 < 20% 且未充电」或「无网络」→ `POST /drain`；恢复 → `/undrain`。结果**不显示在界面上** |
| 开机自启 | `BOOT_COMPLETED` → 启动服务（不打开界面） |
| 保活引导 | 仅仪表盘一个按钮跳系统设置 |

---

## 6. 界面能拿到的数据 vs 实际用到的

agent 在 `127.0.0.1:10255` 已经提供了完整的只读接口，Kotlin 侧只用了其中 3 个：

| 接口 | 返回 | 界面是否使用 |
|---|---|---|
| `GET /healthz` | `ok` | 未用（可做真正的健康检查） |
| `GET /pods` | 标准 `PodList`：namespace、uid、containerStatuses（name/ready/restartCount/state/startedAt）、podIP、hostIP、phase | 仪表盘部分使用（丢掉 namespace、container、startedAt、podIP） |
| `GET /images` | name、registry、repository、reference、manifest_digest、config_digest、layers[]（digest/size）、compressed_size、unpacked_size、rootfs_dir、pulled_at | 镜像页使用（丢掉 registry、pulled_at、单层明细） |
| `POST /images/delete` | `freed_bytes` / 409 使用中 | 镜像页使用（409 原样弹 JSON） |
| `GET /stats/summary` | 节点 CPU/内存/磁盘、逐 Pod 容器的 `rssBytes`、`usageCoreNanoSeconds`、`startTime` | **未用**（逐 Pod 资源就藏在这里） |
| `GET /metrics/resource` | Prometheus 文本指标 | 未用 |
| `GET /containerLogs/{ns}/{pod}/{container}` | 支持 follow / tailLines / timestamps | **未用**（Pod 日志只能混在全局日志里看） |
| `POST /drain` / `/undrain` | 排空 / 恢复 | 只有后台循环用，界面无开关、无状态显示 |

结论：**不做任何 Rust 侧改动，也能把「逐 Pod 资源、Pod 日志、排空状态、健康检查」做进界面**。

---

## 7. 问题归类

**功能缺口**

1. 没有设置页：kubeconfig 只能 `adb push`，节点名、kubelet 端口、镜像源、日志级别全硬编码（gap-list E13/E17）。
2. 没有启动 / 停止 / 重启 agent 的入口，服务一旦被系统杀掉只能靠 10 s 循环自我拉活。
3. 没有维护模式开关，`/drain` 只由电量触发；排空状态不可见。
4. Pod 无详情：看不到 namespace、容器名、启动时间、Pod IP、逐 Pod 资源、逐 Pod 日志。
5. 日志无过滤、无搜索、无导出，无法定位一次拉取失败的完整链路。
6. 镜像无使用关系、无拉取入口、删除无确认、无「释放了多少空间」的反馈。

**一致性 / 视觉**

7. 无主题：ActionBar、按钮、`#0D1117` 内容底、`#101418` 镜像底三套色同时存在；浅色按钮在深色底上最亮。
8. 全站等宽字体 + 中文：ASCII 画框、空格对齐、`ready=true` 之类协议字段直接给人看。
9. 无深色/浅色适配、无 `strings.xml`、无字号缩放考虑（11 sp 日志 + 13 sp 正文）。
10. 空状态、加载态、错误态全部缺省：没有骨架、没有空状态引导、409 直接弹 JSON。

**数据一致性**

11. 同一份 Pod 数据两个来源（`/pods` 与日志正则），同一时刻可以显示不同结果。
12. 状态徽标来自进程内静态字段，进程重建瞬间会显示 STOPPED。

---

## 8. 只改界面就能落地的五件事

1. 逐 Pod 资源：读 `/stats/summary`，在 Pod 行显示内存 / CPU（现成接口，无需改 Rust）。
2. 逐 Pod 日志：读 `/containerLogs/...`，在 Pod 详情里看容器输出，不再只靠全局流。
3. 排空开关 + 状态：调用 `/drain`、`/undrain`，顶部显示「已排空」，与后台循环共用同一状态。
4. 设置页：导入 kubeconfig（SAF / 粘贴）、kubelet 端口、镜像源、日志级别，保存后「优雅重启 agent」。
5. 日志页：级别过滤 + 搜索 + 暂停 + 按 Pod 过滤，时间与内容分列，保留原有 ANSI 清洗。

第 4 条需要「改配置 → 停旧 agent → 等子进程退出 → 起新 agent」的收尾（gap-list 的前置约束），其余四条是纯界面侧。

原型就是按这五条 + 现有能力重排的：见 [`index.html`](../index.html)。
