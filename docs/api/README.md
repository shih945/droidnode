# API 文档

这个目录的文档由 `docs/api/routes.json` 生成，不要直接手改生成文件。

| 文件 | 说明 |
|---|---|
| `routes.json` | **单一来源**：端点、参数、响应示例、IPC 字段 |
| `agent-http.md` | 人读的 HTTP API 参考（含 curl 示例） |
| `agent-openapi.json` | OpenAPI 3.1，可导入 Postman / Insomnia / Swagger UI |
| `compatibility.md` | kubelet / Kubernetes 1.29 兼容性（唯一入口） |
| `kubelet-contract-v1.29.json` | v1.29 契约；检查脚本的输入 |

## 重新生成

```powershell
python scripts/export-api-docs.py          # 生成 Markdown + OpenAPI
python scripts/export-api-docs.py --check  # CI 用：路由/文档是否同步
```

`--check` 会做两件事：

1. 解析 `agent/executor/src/exposers/virtual_kubelet_exposer.rs` 里的 `.route(...)`，
   与 `routes.json` 对比；代码加了路由但没写文档就会失败；
2. 重新生成内容并与现有文件比较；改了 `routes.json` 但忘了重新生成也会失败。

CI 的 `rust` job 已加入这一步。

## 预览 OpenAPI

```powershell
npx @redocly/cli preview-docs docs/api/agent-openapi.json
# 或导入 Swagger Editor / Postman / VS Code OpenAPI 插件
```

## 访问 agent API

agent 的 10255 端口在**非 loopback** 上只允许 `/healthz`、`/metrics`、`/metrics/resource`
匿名访问；`/pods`、`/images`、`/stats` 需要客户端证书。因此：

```powershell
# 推荐：走 apiserver proxy（自动带客户端证书）
kubectl get --raw /api/v1/nodes/dn-k10/proxy/pods
kubectl get --raw /api/v1/nodes/dn-k10/proxy/stats/summary

# 或 adb forward 到 loopback（loopback 允许匿名）
adb -s <serial> forward tcp:10255 tcp:10255
curl -k https://127.0.0.1:10255/pods

# 直接访问 https://<node-ip>:10255/pods 会 403，这是设计行为
```

exec/attach/port-forward 由 kubectl 通过 apiserver 自动完成，不需要手工证书。

## Kubelet 兼容性对比

```powershell
# 抓 DroidNode（apiserver proxy）
python scripts/kubelet-compat.py capture-kubectl `
  --kubeconfig D:\doc\k3s-1.29.yaml --node dn-k10 --out build-cache/kubelet-compat/target

# 按 v1.29 契约检查
python scripts/kubelet-compat.py check --target build-cache/kubelet-compat/target `
  --k8s-version 1.29 --fail-on-missing

# 可选：同版本参考 kubelet
python scripts/kubelet-compat.py capture-kubectl `
  --kubeconfig D:\doc\k3s-1.29.yaml --node k3s-main --out build-cache/kubelet-compat/reference
python scripts/kubelet-compat.py compare `
  --reference build-cache/kubelet-compat/reference --target build-cache/kubelet-compat/target
```

契约：[`kubelet-contract-v1.29.json`](kubelet-contract-v1.29.json)；差异与多版本计划：
[`compatibility.md`](compatibility.md)。

## Rust 内部 API 文档

```powershell
cd agent
cargo doc --workspace --no-deps --document-private-items
# 打开 agent/target/doc/executor/index.html
```

## Android 代码文档（可选）

仓库目前没有配置 Dokka；需要时在 `android/app/build.gradle` 加 Dokka 插件后执行
`./gradlew dokkaHtml`。

## 控制面 API

`k3s-main` 的 Kubernetes API 是标准 Kubernetes API，不由本项目定义；用 `kubectl explain`
或 `kubectl get --raw /openapi/v2` 查看。
