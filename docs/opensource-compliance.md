# 开源说明

本项目以 **MIT** 发布。下文说明上游来源、本项目做了哪些改动，以及随 APK 分发的第三方
组件及其协议。

## 一、本项目协议

```text
MIT License

Copyright (c) 2026 Jamiu Tijani
Copyright (c) 2026 SHIH (fork modifications)
```

即：可以自由使用、修改、再分发、商用（包括闭源），只需保留上面的版权声明与许可正文。

完整正文见仓库根目录 [`LICENSE`](../LICENSE)，它是唯一权威文本。同一份文本还存在于两处：
[`LICENSES/MIT.txt`](../LICENSES/MIT.txt)（随包分发）与
`android/app/src/main/assets/licenses/MIT.txt`（打进 APK 的副本）。

## 二、上游与本项目的改动

- 上游：**droidnode**（作者 Jamiu Tijani），基线提交
  `25c125f5428c11a213d27655a5fb9653563e2a21`（2026-06-10，"Add MIT LICENSE file"），
  当时 58 个文件。
- 本仓库：<https://github.com/shih945/droidnode>，基线之后 **93 个提交、257 个文件变更**
  （`git diff --shortstat 25c125f..HEAD`）。

主要改动：

| 领域 | 内容 |
|---|---|
| agent（Rust） | 分层重构；对齐 kubelet v1.29 的十几个端点；Service 名回环代理；镜像层缓存与 rootfs 复用；心跳与优雅关停 |
| android | v2 控制台（节点 / Pod / 镜像 / 日志 / 设置）；设置拆分；加入集群与 kubeconfig 加密存储；中英双语 |
| deploy | RBAC 精简；生成手机 kubeconfig 的脚本；Bookinfo 演示清单 |
| docs / prototype / scripts | 现状说明、限制清单、API 参考、交互原型、构建与测试脚本 |

## 三、第三方组件

### 3.1 Rust（`agent/Cargo.lock`，317 个包）

全部为宽松许可，没有 GPL / AGPL：`MIT OR Apache-2.0` 152 个、`MIT` 56 个、
`Unicode-3.0` 18 个，其余为 `Apache-2.0`、`ISC`、`BSD`、`Zlib`、`Unlicense` 等。

| crate | 版本 | 许可 |
|---|---|---|
| tokio / tokio-util / tokio-stream | 1.54.1 / 0.7.20 / 0.1.19 | MIT |
| reqwest | 0.12.28 | MIT OR Apache-2.0 |
| serde / serde_json | 1.0.229 / 1.0.154 | MIT OR Apache-2.0 |
| axum | 0.7.9 | MIT |
| kube | 0.88.1 | Apache-2.0 |
| k8s-openapi | 0.21.1 | Apache-2.0 |
| thiserror / tracing / tracing-subscriber | 1.0.x / 0.1.x | MIT 或 MIT OR Apache-2.0 |
| rcgen / tokio-rustls / hyper / tower / libc | — | MIT 或 MIT OR Apache-2.0 |
| tar / flate2 / sha2 / hex / base64 / uuid / chrono / anyhow / hostname | — | MIT OR Apache-2.0 |
| wiremock、http-body-util（仅测试） | 0.6.5 / 0.1.5 | MIT/Apache-2.0 |

### 3.2 Android / Kotlin

| 依赖 | 版本 | 许可 |
|---|---|---|
| Kotlin stdlib | 1.9.22 | Apache-2.0 |
| androidx.core:core-ktx | 1.12.0 | Apache-2.0 |
| kotlinx-coroutines-android | 1.7.3 | Apache-2.0 |
| junit（仅测试） | 4.13.2 | EPL-1.0 |
| androidx.test（仅测试） | 1.5.x | Apache-2.0 |

### 3.3 Go

| 依赖 | 版本 | 许可 | 用途 |
|---|---|---|---|
| github.com/moby/spdystream | v0.5.1 | Apache-2.0 | `kubectl exec` / `port-forward` 的 SPDY 桥 |

### 3.4 随 APK 分发的原生库

| 文件 | 上游 | 许可 |
|---|---|---|
| `libproot.so`、`libproot_loader.so` | [termux/proot](https://github.com/termux/proot) 5.1.107.102 | **GPL-2.0** |
| `libtalloc2x.so` | Samba talloc（Termux `libtalloc` 包） | **LGPL-3.0** |
| `libandroid-shmem.so` | [termux/libandroid-shmem](https://github.com/termux/libandroid-shmem) | **BSD-3-Clause** |
| `libdroidnode_spdy.so` | 本仓库 `agent/spdy-sidecar` | MIT |

proot / talloc 是独立二进制、随包分发，不与本项目代码链接，因此本项目代码仍为 MIT。
许可全文随 APK 附带；proot 的改动（`libtalloc.so.2` → `libtalloc2x.so` 的等长改名）
与复现方式见 [`scripts/build-proot-win.py`](../scripts/build-proot-win.py)（Windows）和
[`scripts/build-android.sh`](../scripts/build-android.sh)（Linux/CI）。两者都从 Termux
仓库取当前最新的 `_aarch64.deb`，因此具体版本号随上游重建而变化。

### 3.5 其他

| 项 | 许可 | 说明 |
|---|---|---|
| `istio/examples-bookinfo-*:1.20.3` | Apache-2.0 | 演示负载，不进 APK |
| `alpine` | 镜像内各组件各自许可 | 仅 proot 冒烟测试 |
| PyYAML | MIT | `deploy/bookinfo/check-ports.py` |
| Rust、Gradle/AGP、NDK、adb、kubectl、k3s | 各自许可 | 仅构建与运维使用，不随产物分发 |

### 3.6 许可全文

| 位置 | 内容 |
|---|---|
| [`LICENSES/`](../LICENSES/) | MIT、Apache-2.0、GPL-2.0、LGPL-3.0、BSD-3-Clause 原文 |
| `android/app/src/main/assets/licenses/` | 同上，随 APK 分发（共 38.7 KB） |

## 四、在 App 里查看

设置 → **关于** → 版本行 → **开源许可**，列出 13 个随包组件（名称 / 协议 · 源码地址）。
清单代码在 `android/app/src/main/java/com/droidnode/ui/v2/ThirdPartyNotices.kt`，
与本文件第三节保持一致。

## 五、维护

依赖变化时更新第三节与 `ThirdPartyNotices.kt`，并重新生成数据：

```bash
git diff --shortstat 25c125f5428c11a213d27655a5fb9653563e2a21..HEAD
cd agent && cargo metadata --format-version 1 \
  | jq -r '.packages[] | "\(.name) \(.version) \(.license // "?")"' | sort -u
cd android && ./gradlew :app:dependencies --configuration releaseRuntimeClasspath
```

许可文本改动后同步进 APK：

```powershell
Copy-Item LICENSES\*.txt android\app\src\main\assets\licenses\ -Force
```
