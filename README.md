**English** · [简体中文](README.zh-CN.md)

# DroidNode

Turn an Android phone into a Kubernetes worker node — **no root, no custom kernel, no k3s patch**.

`kubectl apply`, `logs`, `exec`, `attach`, `cp`, `port-forward`, Service names and metrics all work
against a normal k3s control plane. From the scheduler's point of view the phone is an ordinary
Linux worker; on the device the agent is a user-space mini-kubelet plus a proot-based OCI runtime.

```
kubectl ──▶ k3s apiserver ──▶ Rust agent on Android (HTTPS kubelet, :10255)
                                  ├─ reconcile Pods assigned to this node
                                  ├─ pull / verify / unpack OCI images
                                  ├─ proot container processes (fake root, host netns)
                                  └─ Go SPDY sidecar for exec / attach / port-forward
```

## Current capabilities

| Area | Works today |
|---|---|
| Pod lifecycle | Multi-container Pods, initContainers (serial), restartPolicy Always/OnFailure/Never with kubelet-style backoff, probes (exec/HTTP/TCP), graceful delete, events, `CrashLoopBackOff` |
| Images | OCI pull with registry mirror, linux/arm64 manifest selection, sha256 verification, layer cache, `imagePullPolicy`, `imagePullSecrets`, image list/delete |
| Storage | emptyDir, hostPath, configMap, secret, projected ServiceAccount token, and `droidnode-local` PVCs (auto-created hostPath PV) |
| Networking | Shared device netns (hostNetwork semantics); `podIP = nodeIP`; selector Services publish EndpointSlice; ClusterIP DNAT works; Service **names** resolve through the service-name proxy (loopback) (`svc`, `svc.ns`, FQDN) |
| Debug | `kubectl exec`, `attach`, `port-forward`, `cp`, `logs -f/--tail`; SPDY/3.1 handled by [agent/spdy-sidecar](agent/spdy-sidecar/main.go) |
| Security | Remote kubelet calls require the cluster client CA; kubeconfig is AES-GCM encrypted with Android Keystore; kubelet serving cert is issued by a `kubernetes.io/kubelet-serving` CSR |
| Ops | Maintenance mode (cordon + drain), low-battery drain, WakeLock/WifiLock, battery whitelist, `kubectl top`, node registration/heartbeat/re-register |
| Android | `compileSdk/targetSdk 35`, `specialUse` foreground service, runtime notification permission, agent + proot binaries from `nativeLibraryDir`, Light/Dark appearance (Light default), system-bar safe area |

Known limits and physical constraints: [docs/limitations.md](docs/limitations.md).

## Repository layout

```
agent/
├── executor/src/
│   ├── brokers/                 # k8s API, OCI registry, filesystem, proot
│   ├── services/foundation/     # image pull/unpack, execution, probes, telemetry,
│   │                            # cluster_ip_proxy (service-name proxy), pull secrets, image store
│   ├── services/orchestration/  # reconcile, workload lifecycle, image pipeline,
│   │                            # node registration
│   ├── exposers/                # virtual kubelet HTTPS API
│   └── status_ipc.rs            # Unix-socket status/telemetry for the APK
├── node-agent/src/main.rs       # entry point, task wiring
└── spdy-sidecar/main.go         # Go SPDY server for exec/attach/port-forward
android/app/src/main/java/com/droidnode/
├── ui/v2/                       # Node / Pods / Images / Logs / Settings console
├── exposers/                    # ForegroundServiceExposer
├── brokers/                     # process, battery, network, wake/wifi locks, status IPC
└── services/                    # lifecycle + readiness/backoff
deploy/rbac.yaml                 # ServiceAccount + ClusterRole for the agent
deploy/registry-mirrors.env.example   # phone mirror template -> copy to registry-mirrors.env (gitignored)
toolchain.env.example            # toolchain path template -> copy to toolchain.env.local (gitignored)
docs/                            # current capability, limits, build, API
docs/images/                     # console screenshots referenced from the docs
prototype/                       # interactive HTML design prototype (not built into the APK)
```

Architecture rule (The Standard): brokers touch external systems; foundation services do one
thing; orchestration services coordinate; exposers are thin entry points.

## Build

Toolchain locations are never hardcoded. They come from a git-ignored `toolchain.env.local`
(template: [`toolchain.env.example`](toolchain.env.example)); anything unset falls back to your
environment and then to auto-detection. Variable table: [docs/dev-environment.md](docs/dev-environment.md).

```powershell
# 1) one-off: tell the scripts where your toolchains are
Copy-Item toolchain.env.example toolchain.env.local   # then edit the copy
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-apk-win.ps1 -CheckToolchain

# 2) Rust agent + proot + APK
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-apk-win.ps1

# 3) Go SPDY sidecar (separate step; uses `go` from PATH or GO= in the env file)
Push-Location agent\spdy-sidecar
$env:GOOS='android'; $env:GOARCH='arm64'; $env:CGO_ENABLED='0'
go build -trimpath -ldflags '-s -w' -o libdroidnode_spdy.so .
Copy-Item libdroidnode_spdy.so ..\..\android\app\src\main\jniLibs\arm64-v8a\
Pop-Location
```

On Linux/macOS use `scripts/build-android.sh`, which reads the same env file.

## Install and use

Install the APK, then join the phone to the cluster: **Settings → Cluster → Join cluster**,
and paste either a token or a kubeconfig.

The control-plane side — minting a token, generating a kubeconfig, where the app stores the
credential, why you should not import the admin kubeconfig, and the 401/403 troubleshooting
table — is in the join guide:

→ **[docs/cluster-bootstrap.md](docs/cluster-bootstrap.md)** (in Chinese)

Opening the app starts the node; Settings has stop/start/restart and maintenance controls.

```bash
kubectl get nodes
kubectl apply -f workload.yaml
kubectl logs -f <pod>
kubectl exec <pod> -- sh -c 'id; ls /'
kubectl cp ./file <pod>:/tmp/file
kubectl port-forward <pod> 18080:8080
```

For Services, use a name (`http://<svc>`, `<svc>.<ns>`, `<svc>.<ns>.svc`) or the numeric
ClusterIP; both work. Service and container ports must be **≥1024** and should be different
because the service-name proxy listener owns the Service port on `127.10.x.y`.

## Docs

- [Cluster bootstrap](docs/cluster-bootstrap.md) — control plane, kubeconfig, CSR
- [Agent API](docs/api/README.md) — HTTP endpoints, OpenAPI, IPC schema
- [Kubelet compatibility](docs/api/compatibility.md) — kubelet/Kubernetes 1.29 contract and current results
- [Limitations](docs/limitations.md) — physical/kernel limits
- [Phone storage layout](docs/storage-layout.md) — node files and clear-data behavior
- [Dev environment](docs/dev-environment.md) — toolchains and build paths
- [Agent runtime config](docs/agent-config.md) — environment variables and config files
- [Open-source notices](docs/opensource-compliance.md) — license, third-party components and their terms

## License

MIT — upstream code © 2026 Jamiu Tijani, fork modifications © 2026 SHIH.
Third-party components and their licenses: [LICENSES/](LICENSES/),
[docs/opensource-compliance.md](docs/opensource-compliance.md).
