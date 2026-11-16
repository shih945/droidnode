# 工具链与构建环境

> 最后核对：2026-11-14。**本文不记录任何本机绝对路径。** 工具链位置一律由变量提供，
> 脚本只认变量、不认机器；谁在哪台机器上构建，就由谁在本地填一次。

## 1. 原则

| 规则 | 说明 |
|---|---|
| 仓库里不出现本机路径 | 代码、脚本、文档都不写死 `X:\...\rustup-home` 这类位置 |
| 位置来自本地文件或环境变量 | 优先 `toolchain.env.local`（已 gitignore），其次进程环境变量 |
| 脚本可以提交 | 脚本本身是通用的，路径是运行期解析出来的 |
| 工具链不必搬进仓库 | 放在原处即可；若确实想放进检出目录，`toolchain/` 也已被 gitignore |

## 2. 解析顺序（脚本统一遵守）

1. `toolchain.env.local`（仓库根目录，`KEY=VALUE`，**覆盖**同名环境变量）
2. 进程环境变量
3. 自动探测：`PATH`、`%LOCALAPPDATA%\Android\Sdk`、rustup 默认目录、标准 JDK 安装目录

任何一步都找不到时，脚本**直接失败并指名缺哪个变量**，不会去猜目录。

## 3. 变量表

| 变量 | 用途 | 未设置时 |
|---|---|---|
| `RUSTUP_HOME` | rustup 工具链与 target | 由 `CARGO_HOME` 同级 `rustup-home` 推断，或 `%USERPROFILE%\.rustup` |
| `CARGO_HOME` | cargo、registry 缓存 | 由 `PATH` 上的 `cargo` 反推 |
| `JAVA_HOME` | JDK（推荐 17） | 扫 `C:\Program Files\Java`、`Eclipse Adoptium`、`Amazon Corretto`、`Microsoft` 的标准安装目录 |
| `ANDROID_HOME` | Android SDK | `%LOCALAPPDATA%\Android\Sdk` |
| `ANDROID_NDK_HOME` | NDK（可选） | `<ANDROID_HOME>\ndk\` 下版本号最大的一个 |
| `GRADLE_HOME` | 本地 Gradle（可选） | 用 `android\gradle\wrapper` 下载 |
| `GO` / `GOPATH` / `GOCACHE` | Go sidecar 构建 | `PATH` 上的 `go` |
| `MINGW_HOME` | MinGW 链接器（可选） | 当前构建链用 NDK clang 链接，不需要它；保留给其他场景 |
| `PYTHON` | proot 打包脚本 | `PATH` 上的 `python` / `py` |
| `ADB` | 装机命令 | `PATH` 上的 `adb` |
| `DROIDNODE_TOOLCHAIN_ROOT` | 可选：所有工具链的公共根 | 见下文 |

`DROIDNODE_TOOLCHAIN_ROOT` 只在**单个变量缺失时**兜底，按固定子目录查找：
`<root>\rust\rustup-home`、`<root>\rust\cargo-home`、`<root>\jdk`、`<root>\android-sdk`、
`<root>\go`、`<root>\gradle`、`<root>\mingw64`。单独设置的变量永远优先于它。

### 版本要求（与路径无关）

| 组件 | 版本 | 说明 |
|---|---|---|
| JDK | 17 | Gradle 8.4 / AGP 的要求；用 21 时 wrapper 会切到 Gradle 8.7 |
| Gradle | 8.4（wrapper 默认） | 设了 `GRADLE_HOME` 才用本地那份 |
| Android SDK | `platforms/android-35`、`build-tools` 34 以上 | 对应 `compileSdk/targetSdk 35` |
| NDK | 28.x | 提供 `aarch64-linux-android35-clang`；脚本取 `ndk\` 下版本号最大的 |
| Rust | stable + `aarch64-linux-android` target | 宿主用 GNU 或 MSVC 工具链都能编 |
| Go | 1.27.x | 只用于 SPDY sidecar |
| Python | 3.x | 只用于 proot 下载/打包脚本 |

## 4. 生成本机配置（一次性）

```powershell
Copy-Item toolchain.env.example toolchain.env.local
notepad toolchain.env.local
```

`toolchain.env.local` 已在 `.gitignore` 内，可以自己验证：

```powershell
git check-ignore -v toolchain.env.local
```

两条注意：

- **保持纯 ASCII**：Windows PowerShell 5.1 按 ANSI 读脚本与配置文件，非 ASCII 字符会把解析搞坏。
- 文件里只写 `KEY=VALUE`，`#` 开头是注释，引号会被去掉；不支持命令替换，文件名里有空格也不需要转义。

## 5. 构建

先确认解析结果（不编译）：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-apk-win.ps1 -CheckToolchain
```

输出里会列出 `SDK / NDK / CLANG / JDK / CARGO / RUSTUP / GRADLE / PYTHON`，缺项会指名变量。

完整构建（Rust agent → proot → APK）：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-apk-win.ps1
```

Linux / macOS 用 `scripts/build-android.sh`，读的是同一个 `toolchain.env.local`。
Go SPDY sidecar 不在上面两个脚本里，单独一步，见 [README](../README.md) 的构建小节。

### 版本号：每次打包自动 +1

`build-apk-win.ps1` 在 gradle 之前会把 `android/app/build.gradle` 的
`versionName` 小版本 +1（`0.1.0 → 0.1.1 → 0.1.2 …`），`versionCode` 同步 +1，
打包结束后打印：

```
versionName 0.1.1   versionCode 2
version : 0.1.1 (versionCode 2)
```

设置页「关于」读的是 `packageManager` 里的 `versionName`，所以和产物一致

gradle 失败时脚本会把版本号回滚，不会留下没发出去的版本。

只想重打同一个版本（比如修构建问题）：加 `-NoVersionBump`。

> 注意：Windows PowerShell 5.1 的 `Set-Content -Encoding UTF8` 会写 BOM，
> Gradle 无法解析带 BOM 的 `build.gradle`。脚本用 .NET 的
> `UTF8Encoding(false)` 写回，改这个文件时别换成 `Set-Content`。

## 6. 构建产物位置（全部在仓库内）

| 产物 | 路径 |
|---|---|
| Rust target | `agent\target\`（`CARGO_TARGET_DIR` 默认即仓库内） |
| 生成的 `.so` | `android\app\src\main\jniLibs\arm64-v8a\` |
| APK | `android\app\build\outputs\apk\debug\app-debug.apk` |
| 分发副本 | `dist\` |
| 临时脚本 | `build-cache\`（已 gitignore） |

`android\local.properties` 由构建脚本按解析出的 SDK 重写，本身也在 gitignore 里。

## 7. 硬性规则

1. **不要在磁盘根目录创建构建目录。** 历史上有过整仓库拷贝到根目录、工具链副本也堆在根目录的情况，已全部清理。
2. **不要复制工具链来绕过权限问题。** 遇到 `ACCESS_DENIED` 先查变量是否指对（最常见：`RUSTUP_HOME` 没设，rustc 去写默认位置被拒），确实解决不了再向用户申请权限。
3. **构建脚本原地构建。** 不 stage 到外部目录、不产生仓库外的中间产物。
4. **新增脚本一律走同一套变量**：读 `toolchain.env.local`，缺项报变量名；禁止在脚本里写死本机路径。

## 8. 相关文档

- [README](../README.md) —— 构建与使用
- [控制面接入](cluster-bootstrap.md) —— k3s、kubeconfig、CSR
