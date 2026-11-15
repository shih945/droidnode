#!/usr/bin/env bash
# Generate a phone-ready DroidNode kubeconfig -- one command, no YAML editing.
# Run on the k3s control plane (as root).
#
#   sudo bash deploy/console/make-kubeconfig.sh                  # fresh 1-year token
#   sudo bash deploy/console/make-kubeconfig.sh --long-lived     # reusable token Secret (never expires)
#   sudo bash deploy/console/make-kubeconfig.sh --with-rbac      # apply deploy/rbac.yaml first (first install only)
#   sudo IP=192.168.2.7 OUT=/tmp/phone.yaml bash deploy/console/make-kubeconfig.sh
#
# RBAC is NOT applied unless you pass --with-rbac: deploy/rbac.yaml is a one-time
# cluster component (like installing kube-proxy), not something you touch for
# every phone. If the ServiceAccount is missing, this script tells you to apply
# it once and exits.
#
# --long-lived exists for the "many phones, zero control-plane work" case: it
# creates the classic never-expiring token Secret, so the resulting kubeconfig
# can be copied to any number of devices. Tradeoff: revoking one device means
# rotating the shared secret (delete it and re-run). Without the flag each run
# mints a per-device, time-limited token -- safer default.
#
# English only: the VM console has no CJK font (same reason as setup.sh).

set -euo pipefail

WITH_RBAC=0
LONG_LIVED=0
for arg in "$@"; do
  case "$arg" in
    --with-rbac) WITH_RBAC=1 ;;
    --long-lived) LONG_LIVED=1 ;;
    -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
    *) printf 'unknown argument: %s\n' "$arg" >&2; exit 2 ;;
  esac
done

NS="${NS:-kube-system}"
SA="${SA:-droidnode-agent}"
DURATION="${DURATION:-8760h}"
PORT="${PORT:-6443}"
OUT="${OUT:-$PWD/droidnode-kubeconfig.yaml}"
K3S_YAML="${K3S_YAML:-/etc/rancher/k3s/k3s.yaml}"
DN="$(cd "$(dirname "$0")" && pwd)"
SECRET_NAME="${SECRET_NAME:-droidnode-agent-token}"

say() { printf '\n=== %s ===\n' "$*"; }
die() { printf 'FATAL: %s\n' "$*" >&2; exit 1; }

command -v kubectl >/dev/null 2>&1 || die "kubectl not found"
[ -r "$K3S_YAML" ] || die "cannot read $K3S_YAML (run as root)"

IP="${IP:-}"
if [ -z "$IP" ]; then
  IP="$(hostname -I 2>/dev/null | awk '{print $1}')"
fi
[ -n "$IP" ] || die "cannot determine the LAN IP; pass IP=<addr>"

say "control plane https://$IP:$PORT, identity $NS/$SA"

# 1. RBAC only on request: it is a one-time install, not a per-join step.
if [ "$WITH_RBAC" = "1" ]; then
  for cand in "$DN/rbac.yaml" "$DN/../rbac.yaml" "${RBAC:-}"; do
    [ -n "$cand" ] && [ -f "$cand" ] || continue
    kubectl apply -f "$cand" >/dev/null && echo "applied $cand (one-time install)"
    break
  done
fi
if ! kubectl -n "$NS" get sa "$SA" >/dev/null 2>&1; then
  die "serviceaccount $NS/$SA does not exist. Run this once on the control plane:
       kubectl apply -f deploy/rbac.yaml
     then re-run this script (or pass --with-rbac)."
fi

# 2. Credential.
if [ "$LONG_LIVED" = "1" ]; then
  say "long-lived token secret $NS/$SECRET_NAME (reusable across devices)"
  kubectl -n "$NS" apply -f - >/dev/null <<EOF
apiVersion: v1
kind: Secret
metadata:
  name: $SECRET_NAME
  namespace: $NS
  annotations:
    kubernetes.io/service-account.name: $SA
type: kubernetes.io/service-account-token
EOF
  TOKEN=""
  for _ in $(seq 1 20); do
    TOKEN="$(kubectl -n "$NS" get secret "$SECRET_NAME" \
      -o jsonpath='{.data.token}' 2>/dev/null | base64 -d 2>/dev/null || true)"
    [ -n "$TOKEN" ] && break
    sleep 1
  done
  [ -n "$TOKEN" ] || die "token secret was created but no token was issued yet; retry in a few seconds"
else
  say "per-device token, valid $DURATION"
  TOKEN="$(kubectl -n "$NS" create token "$SA" --duration="$DURATION")"
  [ -n "$TOKEN" ] || die "token creation failed"
fi

CA_B64="$(awk '/certificate-authority-data:/{print $2; exit}' "$K3S_YAML")"
[ -n "$CA_B64" ] || die "cannot read certificate-authority-data from $K3S_YAML"

# 3. Write the kubeconfig (0600, never world-readable).
umask 077
cat > "$OUT" <<EOF
apiVersion: v1
kind: Config
clusters:
- name: droidnode
  cluster:
    certificate-authority-data: $CA_B64
    server: https://$IP:$PORT
contexts:
- name: droidnode
  context:
    cluster: droidnode
    user: droidnode-agent
current-context: droidnode
users:
- name: droidnode-agent
  user:
    token: $TOKEN
EOF
chmod 600 "$OUT"
say "wrote $OUT"

# 3b. Client CA for the kubelet API (F1). Without it the phone's kubelet refuses
#     apiserver requests, so `kubectl logs/exec/port-forward` fail with
#     "client certificate required by DroidNode kubelet".
CLIENT_CA_SRC="${CLIENT_CA_SRC:-/var/lib/rancher/k3s/server/tls/client-ca.crt}"
CLIENT_CA_OUT="${CLIENT_CA_OUT:-$PWD/client-ca.crt}"
if [ -r "$CLIENT_CA_SRC" ]; then
  install -m 644 "$CLIENT_CA_SRC" "$CLIENT_CA_OUT"
  echo "client CA   -> $CLIENT_CA_OUT  (push as files/client-ca.crt)"
else
  echo "client CA   -> SKIPPED ($CLIENT_CA_SRC not readable); kubectl logs/exec will be rejected"
fi

# 4. Verify with exactly the credentials the phone will use: TLS + auth + RBAC.
if KUBECONFIG="$OUT" kubectl get nodes --request-timeout=10s >/dev/null 2>&1; then
  echo "verify: OK -- token authenticates and can list nodes"
else
  echo "verify: FAILED -- details:"
  KUBECONFIG="$OUT" kubectl get nodes --request-timeout=10s || true
  die "this kubeconfig does not work; the phone would fail the same way"
fi

cat <<EOF

Next:
  1) copy the kubeconfig to the phone (debug build):
       adb push "$OUT" /sdcard/Download/droidnode-kubeconfig.yaml
  2) copy the client CA (needed for kubectl logs / exec / port-forward):
       adb push "$CLIENT_CA_OUT" /data/local/tmp/client-ca.crt
       adb shell "cat /data/local/tmp/client-ca.crt | run-as com.droidnode sh -c 'cat > files/client-ca.crt'"
  3) registry mirror (CN networks; otherwise docker.io pulls fail with ErrImagePull):
       adb push deploy/registry-mirrors.env /data/local/tmp/
       adb shell "cat /data/local/tmp/registry-mirrors.env | run-as com.droidnode sh -c 'cat > files/registry-mirrors.env'"
  4) in the app: Settings -> Control plane -> import kubeconfig (or use "address + token"),
     then restart the agent.

Server in the file: https://$IP:$PORT
That address must be reachable from the phone -- not 127.0.0.1, not a docker IP.
The phone should sit on the same LAN as the control plane; USB tethering /
Wi-Fi Direct addresses (192.168.137.x) are not routable from the cluster.
EOF
