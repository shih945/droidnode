# 新控制台（ui.v2）实现说明

这次把原型落成了**一组新增页面**：旧 `MainActivity` / `DebugActivity` / `ImagesActivity` 一行没动，
`agent/` 没动，所以你那边修 agent 的 BUG 不受影响。验证用的 APK 已经装到测试机上，
`com.droidnode/.ui.v2.NodeActivity` 可以直接启动。

```
真机截图（2026-11-12 01:0x–01:13，OnePlus 10 Pro / 1080 × 2412）
├── v2-node.png            节点：运行中 + 心跳线 + 资源
├── v2-node-drained.png    节点：排空后（琥珀色，开关已开）
├── v2-pods.png            Pod 列表 + 过滤
├── v2-pod-sheet.png       Pod 详情弹层（含逐 Pod 内存 6.0 MB）
├── v2-images.png          镜像（按 rootfs 去重后 5 个 / 368.2 MB）
├── v2-image-sheet.png     镜像详情（digest、层数、rootfs）
├── v2-image-confirm.png   删除确认（写明释放 8.3 MB）
├── v2-logs.png            日志（级别/搜索/暂停/清空）
├── v2-logs-pod.png        从 Pod 详情跳进来，只看该 Pod
└── v2-settings.png        设置（kubeconfig / 只读配置 / 维护模式）
```

---

## 1. 加进来的文件

**Kotlin**（`app/src/main/java/com/droidnode/ui/v2/`）

| 文件 | 职责 |
|---|---|
| `NodeActivity.kt` | 节点页：状态、心跳线、身份、资源、维护模式、Pod 预览 |
| `PodsActivity.kt` | Pod 页：过滤（全部/运行中/异常）+ 详情弹层 |
| `ImagesActivity.kt` | 镜像页：占用汇总、列表、详情、删除二次确认 |
| `LogsActivity.kt` | 日志页：级别/关键字/按 Pod 过滤、暂停、清空 |
| `SettingsActivity.kt` | 设置页：kubeconfig 导入、连接测试、电池白名单、维护模式 |
| `AgentApi.kt` | 只跟 `127.0.0.1:10255` 说话的 HTTPS 客户端 |
| `Snapshot.kt` | 解析 `/pods`、`/stats/summary`、`/images`，合并逐 Pod 内存 |
| `PulseView.kt` | 签名元素：心跳线（HERO / MINI 两种音量，信号虚线会走） |
| `Ui.kt` / `Rows.kt` / `StatusPill.kt` | 格式化、列表行、统一的状态徽标 |
| `SheetHost.kt` / `Sheets.kt` | 底部弹层容器 + 四种弹层内容 |
| `BottomNav.kt` | 五格底栏与页面切换 |
| `LocalConfig.kt` / `ProcInfo.kt` | 排空状态持久化、`/proc` 进程采样 |

**资源**（全部新增，原 `res/` 只有两个布局）

`values/colors.xml`、`dimens.xml`、`styles.xml`；`color/switch_track.xml`、`switch_thumb.xml`；
`drawable/` 下 13 个形状 + 15 个矢量图标；`layout/` 下 5 个页面 + 4 个弹层 + 3 个列表行 + 底栏。

**唯一改动的现有文件**：`AndroidManifest.xml`，只追加了 5 个 `<activity>`（`exported="false"`）。
另加 `src/debug/AndroidManifest.xml`，只让 debug 包把这几页开放给 adb 启动，release 不受影响。

---

## 2. 现在能做什么（都是真接口，不是假数据）

| 页面 | 已接通 | 说明 |
|---|---|---|
| 节点 | `/pods`、`/stats/summary`、`/healthz`，每 2 秒 | 内存/存储用 summary，CPU 用 `/proc` 采样，电池走系统广播 |
| 节点 | `POST /drain` / `/undrain` | 真机上验证过：开关 → 状态变「已排空」、心跳线转琥珀、Pod 停止 |
| Pod | 容器名、重启次数、启动时间、Pod IP、逐 Pod 内存 | 逐 Pod 内存来自 `/stats/summary` 的 `pods[].containers[].memory.rssBytes` |
| Pod | 详情弹层 →「查看日志」 | 带 Pod 名跳到日志页，日志页顶部出现该 Pod 的筛选项 |
| 镜像 | 列表、占用合计、digest、rootfs、删除 | 删除有二次确认，409 翻成「这个镜像正被 Pod 使用」 |
| 日志 | 级别、搜索、暂停（含新行计数）、清空、按 Pod | 自动去掉每行重复的 `时间 级别` 前缀 |
| 设置 | kubeconfig 导入（粘贴 / 系统文件选择）、连接测试、电池白名单 | 连接测试 = `/healthz` + 控制面端口 TCP 探测 |

实测数据：节点名 `droidnode-37e62e0fe8ce0057`、5 个 Pod、镜像 5 个共 368.2 MB、
内存 5.9 / 11.2 GB、排空后 4 个容器被停止。

---

## 3. 这一版故意没做的

| 项 | 原因 |
|---|---|
| 改节点名 / kubelet 端口 / 镜像源 / 日志级别 | 这些值由 `RustProcessBroker` 的环境变量写死，界面改成可编辑只会得到一个假的输入框；需要 agent 侧先支持读配置 |
| 「重启 agent」按钮 | 现有 `ForegroundServiceExposer` 没有 stop/restart 动作，`stopService` 会留下孤儿 agent；要做得先补服务侧接口 |
| 每 Pod 的容器日志（`/containerLogs` 流式） | 现在按日志文本里的 pod 名过滤；真正的逐容器日志要加 HTTP 流式读取，放到下一步 |
| 删除 Pod | agent 没有删除接口，Pod 生命周期归控制面管 |
| 启动镜像拉取 | agent 没有 `/images/pull`，界面上不给假按钮 |
| 浅色主题 | 与旧界面一致，先只做深色 |

这些都在设置页用一句话写清楚了（「端口、镜像源、日志级别由 agent 侧决定，这一版只读」），
没有假装可以改。

> 更新（2026-11-14）：前两项已经落地 —— 节点名 / 端口 / 日志级别可由环境变量注入，
> 镜像源改成设置页写 `files/registry-mirrors.env`（agent 启动时读，没有编译进去的地址），
> 保存后走「重启 agent」。上表保留当时的决策记录。

---

## 4. 怎么预览

debug 包已经装到测试机，直接起Activity：

```bash
adb shell am start -n com.droidnode/.ui.v2.NodeActivity
adb shell am start -n com.droidnode/.ui.v2.PodsActivity
adb shell am start -n com.droidnode/.ui.v2.ImagesActivity
adb shell am start -n com.droidnode/.ui.v2.LogsActivity
adb shell am start -n com.droidnode/.ui.v2.SettingsActivity
```

页面之间用底栏切换；弹层打开时返回键先关弹层。

构建（离线；JDK / Gradle / SDK 位置由 `toolchain.env.local` 提供，不要写死本机路径）：

```powershell
cd android
.\gradlew.bat assembleDebug --offline
```

如果依赖缓存不在默认位置，用环境变量指向它（这个变量同样不要提交）：

```powershell
$env:GRADLE_USER_HOME = '<你的依赖缓存目录>'
```

---

## 5. 怎么正式切过来

只改 `AndroidManifest.xml` 一处：把 `MAIN` / `LAUNCHER` 从 `.MainActivity` 挪到 `.ui.v2.NodeActivity`，
并把它的 `exported` 改成 `true`（作为入口必须导出）。

```xml
<!-- 改后 -->
<activity android:name=".MainActivity" android:exported="false" />

<activity
    android:name=".ui.v2.NodeActivity"
    android:exported="true"
    android:label="DroidNode"
    android:theme="@style/AppTheme">
    <intent-filter>
        <action android:name="android.intent.action.MAIN"/>
        <category android:name="android.intent.category.LAUNCHER"/>
    </intent-filter>
</activity>
```

切换后建议顺手补两处（都要动现有文件，所以这次没碰）：

1. `ForegroundServiceExposer.buildNotification()` 里点击目标从 `DebugActivity` 改成
   `ui.v2.NodeActivity`，否则通知还是打开旧调试台。
2. `BootReceiver` 不需要改：它只拉服务，不拉界面。

旧页面保留着随时可以切回去，删掉只是移除 Manifest 里的三行。

---

## 6. 过程中发现的两个 agent 侧问题

1. **`/images` 重复列条目**：同一个 rootfs 目录被返回 4 次（`alpine:latest` ×4），
   界面上按 rootfs 去重后才得到真实的 5 个镜像 / 368.2 MB。建议在 `image_store::list` 里按目录去重。
2. **排空状态不对外暴露**：`/drain` 之后没有任何接口能读到「节点已排空」，
   `/healthz` 只有 `ok`。现在界面用「本地记录 + Pod 的 `status.message` 里是否含 drained」来判断，
   够用但不够准。建议加一个 `GET /node/state` 或在 `/healthz` 里带上 `drained` 字段。

---

## 7. 还差的打磨

- 日志页首行在滚到底时会半截露出来（ListView 的边界），可以加一个顶部渐隐遮罩。
- 「按 Pod 过滤」是文本匹配，不是真正的容器日志流。
- 镜像删除没在真机上试过（避免删掉你正在用的镜像），逻辑上 409 已经处理。
- 底栏切换会 `finish()` 当前页，退栈里只留一层；如果以后要保留滚动位置，改成
  `FLAG_ACTIVITY_REORDER_TO_FRONT`。
