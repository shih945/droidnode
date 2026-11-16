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
| Networking | Shared device netns (hostNetwork semantics); `podIP = nodeIP`; selector Services publish EndpointSlice; ClusterIP DNAT works; Service **names** resolve through the D3 loopback proxy (`svc`, `svc.ns`, FQDN) |
| Debug | `kubectl exec`, `attach`, `port-forward`, `cp`, `logs -f/--tail`; SPDY/3.1 handled by [agent/spdy-sidecar](agent/spdy-sidecar/main.go) |
| Security | Remote kubelet calls require the cluster client CA (F1); kubeconfig is AES-GCM encrypted with Android Keystore (F2); kubelet serving cert is issued by a `kubernetes.io/kubelet-serving` CSR (A16) |
| Ops | Maintenance mode (cordon + drain), low-battery drain, WakeLock/WifiLock, battery whitelist, `kubectl top`, node registration/heartbeat/re-register |
| Android | `compileSdk/targetSdk 35`, `specialUse` foreground service, runtime notification permission, agent + proot binaries from `nativeLibraryDir`, Light/Dark appearance (Light default), system-bar safe area |

Known limits and physical constraints: [docs/limitations.md](docs/limitations.md).

## Repository layout

```
agent/
├── executor/src/
│   ├── brokers/                 # k8s API, OCI registry, filesystem, proot
│   ├── services/foundation/     # image pull/unpack, execution, probes, telemetry,
│   │                            # cluster_ip_proxy (D3), pull secrets, image store
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
deploy/k3s-registries.example.yaml    # k3s mirror template -> copy to k3s-registries.yaml (gitignored)
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

## Control-plane setup

Baseline: **k3s v1.29.15+k3s1** (go1.23.6).

1. Apply [deploy/rbac.yaml](deploy/rbac.yaml).
2. Create a token for `kube-system/droidnode-agent` and a kubeconfig whose server is the
   control-plane LAN IP (not `127.0.0.1`). Publish the client CA for token-only joins:
   `kubectl -n kube-system create configmap droidnode-client-ca --from-file=ca.crt=/var/lib/rancher/k3s/server/tls/client-ca.crt --dry-run=client -o yaml | kubectl apply -f -`
3. Keep `egress-selector-mode: disabled` in `/etc/rancher/k3s/config.yaml`; k3s's tunnel does
   not contain DroidNode's kubelet.
4. For `kubectl top`, run metrics-server with `--kubelet-insecure-tls` (the phone's serving
   cert is issued by the cluster CA, but metrics-server's CA bundle may not include it).
5. No `kubelet-certificate-authority` bundle is needed. The agent requests a
   `kubernetes.io/kubelet-serving` CSR and auto-approves it (see [docs/cluster-bootstrap.md](docs/cluster-bootstrap.md)).

## Install and use

The control plane only needs one of these:

```bash
# option 1: mint a token, the app fills in address + token (CA comes from /cacerts)
kubectl -n kube-system create token droidnode-agent --duration=8760h

# option 2: generate a full kubeconfig and push it to the phone
sudo env KUBECONFIG=/etc/rancher/k3s/k3s.yaml IP=192.168.2.7 \
  OUT=/tmp/droidnode-kubeconfig.yaml bash deploy/console/make-kubeconfig.sh
```

In the app, **Settings → 控制面 → 加入集群** opens a dedicated page with three ways in:
address + token, import a kubeconfig file, or paste a kubeconfig. On success it is persisted as
`files/node/config/kubeconfig.enc`; the foreground service materializes a private `kubeconfig.runtime`
only while the agent runs. The settings page itself shows the result (server, identity, file path).
Opening the app starts the node; Settings has stop/start/restart and maintenance controls.

Do not import the admin `/etc/rancher/k3s/k3s.yaml` unless you are debugging: it carries `system:masters`
and cannot be revoked per device. 401/403 handling and the troubleshooting table:
[docs/cluster-bootstrap.md](docs/cluster-bootstrap.md#导入后报-401--403-怎么区分).

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
because the D3 loopback listener owns the Service port on `127.10.x.y`.

### After joining: handle svclb (ServiceLB)

k3s ships `svclb-*` as a **DaemonSet** for `LoadBalancer` Services (Traefik by
default). It has no node selector/affinity, so it also lands on the phone; the
proot runtime cannot bind privileged ports 80/443 and the pod stays in
`PrivilegedPort` (0/2). The `droidnode/proot=true` label does **not** exclude it
on k3s 1.29 — patch the DaemonSet once:

```bash
# 1. find the svclb DaemonSet (hash varies)
kubectl -n kube-system get ds | grep svclb

# 2. exclude nodes carrying the DroidNode label
kubectl -n kube-system patch ds <svclb-ds> --type=merge -p '{
  "spec": {
    "template": {
      "spec": {
        "affinity": {
          "nodeAffinity": {
            "requiredDuringSchedulingIgnoredDuringExecution": {
              "nodeSelectorTerms": [
                {
                  "matchExpressions": [
                    { "key": "droidnode/proot", "operator": "DoesNotExist" }
                  ]
                }
              ]
            }
          }
        }
      }
    }
  }
}'

# 3. remove the pod that is already on the phone
kubectl -n kube-system get pods -o wide | grep <phone-node>
kubectl -n kube-system delete pod <phone-svclb-pod>

# 4. verify: only the control-plane node should have svclb
kubectl -n kube-system get pods -o wide | grep svclb
```

The agents add `droidnode/proot=true` when they register, so the affinity keeps
future svclb pods off every phone. If Traefik is reinstalled/upgraded, the
DaemonSet is recreated and the affinity must be applied again. If ServiceLB is
not needed at all, install k3s with `--disable traefik --disable servicelb`.

## Agent environment variables

| Variable | Default | Meaning |
|---|---|---|
| `KUBECONFIG` | — | Runtime kubeconfig path (set by the APK) |
| `DROIDNODE_NODE_ID` | `droidnode-<ANDROID_ID>` | Node name |
| `DROIDNODE_KUBELET_PORT` | `10255` | kubelet HTTPS port |
| `DROIDNODE_PROOT_PATH` | `nativeLibraryDir/libproot.so` | proot binary |
| `DROIDNODE_SPDY_HELPER` | sibling `libdroidnode_spdy.so` | SPDY sidecar |
| `DROIDNODE_LAYERS_DIR` | `<DROIDNODE_DATA_DIR>/layers` | OCI layer cache |
| `DROIDNODE_ROOTFS_DIR` | `<DROIDNODE_DATA_DIR>/rootfs` | image/Pod rootfs cache |
| `DROIDNODE_DATA_DIR` | APK sets `<filesDir>/node` | node root (socket/volumes) |
| `DROIDNODE_CONFIG_DIR` | `<DROIDNODE_DATA_DIR>/config` | kubeconfig, CAs, registry/network env |
| `DROIDNODE_KUBELET_CLIENT_CA` | `<DROIDNODE_DATA_DIR>/config/client-ca.crt` | client CA for kubelet mTLS |
| `DROIDNODE_REGISTRY_MIRROR*` | — (unset = upstream directly) | `docker.io` / `registry.k8s.io` / `quay.io` rewrite |
| `RUST_LOG` | `info` | tracing filter |

Registry mirrors are deliberately **not** compiled in or committed: the agent reads
`$DROIDNODE_CONFIG_DIR/registry-mirrors.env` (template: [`deploy/registry-mirrors.env.example`](deploy/registry-mirrors.env.example)),
and k3s takes `deploy/k3s-registries.yaml` (template: [`deploy/k3s-registries.example.yaml`](deploy/k3s-registries.example.yaml)).
Both real files are gitignored. On the phone, edit them in **Settings → 节点 → 镜像源**
(three host fields, blank = pull upstream); saving restarts the agent.

## Docs

- [Cluster bootstrap](docs/cluster-bootstrap.md) — control plane, kubeconfig, CSR
- [Agent API](docs/api/README.md) — HTTP endpoints, OpenAPI, IPC schema
- [Kubelet compatibility](docs/api/compatibility.md) — kubelet/Kubernetes 1.29 contract and current results
- [Limitations](docs/limitations.md) — physical/kernel limits
- [Phone storage layout](docs/storage-layout.md) — node files and clear-data behavior
- [Dev environment](docs/dev-environment.md) — toolchains and build paths

## License

MIT
