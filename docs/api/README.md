# API 文档

这个目录的文档由 `docs/api/routes.json` 生成，不要直接手改生成文件。

| 文件 | 说明 |
|---|---|
| `routes.json` | **单一来源**：端点、参数、响应示例、IPC 字段 |
| `agent-http.md` | 人读的 HTTP API 参考（含 curl 示例） |
| `agent-openapi.json` | OpenAPI 3.1，可导入 Postman / Insomnia / Swagger UI |

## 重新生成

```powershell
python scripts/export-api-docs.py          # 生成 Markdown + OpenAPI
python scripts/export-api-docs.py --check  # CI 用：路由/文档是否同步
```

`--check` 会做两件事：

1. 解析 `agent/executor/src/exposers/virtual_kubelet_exposer.rs` 里的 `.route(...)`，
   与 `routes.json` 对比，代码加了路由但没写文档就会失败；
2. 重新生成内容并与现有文件比较，改了 `routes.json` 但忘了重新生成也会失败。

CI 的 `rust` job 已加入这一步。

## 预览 OpenAPI

任选其一：

```powershell
# Redocly 本地预览（需要 Node）
npx @redocly/cli preview-docs docs/api/agent-openapi.json

# 或直接导入
# - Swagger Editor: https://editor.swagger.io
# - Postman: Import → File → agent-openapi.json
# - VS Code: OpenAPI (Swagger) Editor 插件
```

## 访问 agent API

```powershell
# 只读端点（kubelet 只读端口惯例，允许匿名）
curl -k https://192.168.2.5:10255/pods
curl -k https://192.168.2.5:10255/images

# 通过 adb 转发，源地址变成 127.0.0.1（同样允许匿名）
adb -s <serial> forward tcp:10255 tcp:10255
curl -k https://127.0.0.1:10255/pods

# exec/attach/port-forward 需要客户端证书（kubectl 自动带）
```

## Rust 内部 API 文档

面向代码而不是 HTTP：

```powershell
cd agent
cargo doc --workspace --no-deps --document-private-items
# 打开 agent/target/doc/executor/index.html
```

包含 `ReconciliationService`、`ControlPlaneBroker`、`KubeletState` 等模块的 Rustdoc。
`--document-private-items` 会导出 `pub(crate)` 的内部接口，适合重构时查依赖。

## Android 代码文档（可选）

仓库目前没有配置 Dokka（会引入 Gradle 插件依赖）。需要时：

```kotlin
// android/app/build.gradle
plugins { id("org.jetbrains.dokka") version "1.9.20" }
```

```powershell
cd android
./gradlew dokkaHtml
# 输出 android/app/build/dokka/html/index.html
```

Android 侧是应用而不是 SDK，`data/` 与 `ui/v2/` 的 KDoc 注释可直接用 Dokka 导出。

## 控制面 API

`k3s-main` 的 Kubernetes API（节点注册、Pod watch、状态上报）是标准 Kubernetes API，
不由本项目定义；用 `kubectl explain` 或 Kubernetes OpenAPI 查看：

```powershell
kubectl explain node.status
kubectl get --raw /openapi/v2 > k8s-openapi.json
```
