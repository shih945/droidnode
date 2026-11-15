# 开销实测（G4）

> 实测设备：OnePlus PGJM10，Android 14，8 核，11.7 GB 内存；
> 节点 `droidnode-37e62e0fe8ce0057`，k3s v1.36.5，agent 2026-11-13 构建。
> 所有数字都是单次/短时采样，用于量级判断，不是 benchmark 承诺。

## 1. 常驻开销

| 项 | 实测 |
|---|---|
| `libnode_agent.so` RSS | **~15.9 MiB**（16,256 KiB） |
| agent 空闲 CPU | **0.0%**（`top -b -n 1 -p <pid>`；累计 13.3s CPU / 数小时运行） |
| 一个 proot 容器（alpine + sleep） | proot **~4.5 MiB** + 容器进程 ~0.6 MiB |
| 节点可分配内存 | 11.2 GiB（`kubectl top` 显示手机整体用量 ~5.9 GiB / 57%） |

没有 cgroup：以上是进程 RSS，不是容器内存上限；`kubectl top` 的数字是按进程树估算的（见 A11 限制）。

## 2. 调度/启动

从 Pod 对象 `creationTimestamp` 到容器 `running.startedAt`：

```
bench-shell（alpine，镜像已缓存）: 10.0 s
```

这 10s 主要是 reconcile 周期 + 镜像准备/每 Pod rootfs 复制；不是调度器延迟。
冷镜像（首次拉取/解包）另计，取决于网络与镜像大小。

## 3. 多 Pod syscall 开销（G4 重点）

负载：每个 Pod 跑
`dd if=/dev/zero of=/dev/null bs=1 count=20000`（约 4 万次 read/write syscall），
`time` 取 `real`。同一台手机，镜像已缓存。

| 并发 Pod 数 | 每个 Pod 的 real | 单 Pod 相对 solo | 聚合吞吐（Pod/s） |
|---|---:|---:|---:|
| 1（solo） | **5.44 s** | 1.00× | 0.184 |
| 3（同时） | **21.50 / 21.79 / 21.97 s** | ~3.95× | **0.138**（约为 solo 的 75%） |

结论：

- proot 的 ptrace 开销在并发时明显放大：3 个 syscall 密集 Pod 的单 Pod 耗时约 **4×**；
- 总吞吐反而下降约 **25%**，所以 **syscall 密集负载不建议超过 2 个 Pod**；
- CPU 核数不是瓶颈（8 核），瓶颈是每次 syscall 的两次陷入；
- 对 IO/网络等待型负载影响小得多；`pods` 上限（B22）应按负载类型区分，而不是只看 CPU。

## 4. exec / attach / port-forward

| 操作 | 实测 |
|---|---|
| `kubectl exec ... -- true` × 20 | **9.277 s**，平均 **~464 ms/次** |
| `kubectl attach` 输出 | 立即持续输出（无额外轮询） |
| `kubectl port-forward 18080:8080` | 建立后转发正常，curl 返回页面 |
| `kubectl cp` 小文件 | 正常，耗时与一次 exec 同量级 |

exec 的 ~464ms 包含 kubectl→apiserver、apiserver→SPDY sidecar、proot 启动三部分；
不是单次 syscall 开销。

## 5. Service 代理（D3）

- 每个 Service 一个 loopback listener + 10s 一次 Service/EndpointSlice 同步；
- 空闲时无连接，CPU 可忽略；
- 新建 Pod 的 `/etc/hosts` 最多等 10s 才有 Service 名称。

## 6. 复现命令

```bash
# agent RSS / CPU
adb shell ps -A -o PID,RSS,NAME | grep libnode_agent
adb shell top -b -n 1 -p <pid>

# 容器进程
adb shell ps -A -o PID,RSS,NAME | grep -E 'libproot|sleep'

# 启动延迟（creationTimestamp → startedAt）
kubectl get pod bench-shell -o json

# exec 延迟
TIMEFORMAT=%R; time (for i in $(seq 1 20); do kubectl exec bench-shell -- true; done)

# 节点指标
kubectl top node droidnode-37e62e0fe8ce0057
```

## 7. 结论

- agent 常驻内存约 16 MiB、空闲 CPU ≈0，适合长期跑在手机上；
- 单容器额外内存约 5 MiB（proot 进程树）；
- Pod 启动受 10–15s reconcile 节奏限制，exec 约 0.5s；
- 真正瓶颈仍是**没有 cgroup / netns**，不是 CPU 或内存。
