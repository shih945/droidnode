# 版本化 kubelet 契约

命名：`kubelet-v<major>.<minor>.json`（例如 `kubelet-v1.33.json`）。

`scripts/kubelet-compat.py` 解析顺序：

1. `docs/api/contracts/kubelet-v<version>.json`
2. 旧路径 `docs/api/kubelet-contract-v<version>.json`（1.29 基线仍在这里）

```powershell
python scripts/kubelet-compat.py check --target <dir> --k8s-version 1.29
python scripts/kubelet-compat.py check --target <dir> --k8s-version 1.33 --fail-on-missing
```

新增一个版本时：

1. 复制上一版契约，按该版本 kubelet 的必填字段调整 `required`；
2. 把“该版本新增、但 DroidNode 暂不返回”的字段放进 `allowed_extra` 或明确留空；
3. 只把 DroidNode 真正承诺返回的字段写进 `required`；
4. 用该版本集群跑 `capture-kubectl` + `check --fail-on-missing`，把结果写进
   [`../compatibility.md`](../compatibility.md) 的矩阵。

当前状态：只有 **1.29** 基线契约（v1.29 L0 已通过）；更高版本按需补。
