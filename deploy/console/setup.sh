#!/bin/bash
# DroidNode control-plane bootstrap -- run as root in the VM console:
#
#     bash ~shih/dn/setup.sh
#
# English only (the VM console has no CJK font, Chinese renders as boxes).
# Every step is idempotent, so it is safe to re-run after a failure.
#
#   1. /etc/resolv.conf        (this image ships none -> no DNS at all)
#   2. apt sources -> mirror   (the image only had a commented-out cdrom entry)
#      install sudo / iptables / curl / ca-certificates
#   3. install k3s            (rancher CN mirror, full components, no trimming)
#   4. registry mirrors        (/etc/rancher/k3s/registries.yaml) + wait Ready
#   5. RBAC + kubeconfig for the phone (~/dn/kubeconfig)
#   6. summary
#
# Log: /var/log/droidnode-setup.log

set -u
export LC_ALL=C
export DEBIAN_FRONTEND=noninteractive

IP="${IP:-192.168.2.7}"
IFACE="${IFACE:-ens33}"
NODE_NAME="${NODE_NAME:-k3s-main}"
OWNER="${OWNER:-shih}"
DN="$(cd "$(dirname "$0")" && pwd)"
LOG=/var/log/droidnode-setup.log

# Mirror endpoints are configurable, never baked in beyond public defaults.
# Point these at your own mirror if you have one; anything private stays out of git.
K3S_INSTALL_URL="${K3S_INSTALL_URL:-https://rancher-mirror.rancher.cn/k3s/k3s-install.sh}"
K3S_INSTALL_FALLBACK_URL="${K3S_INSTALL_FALLBACK_URL:-https://get.k3s.io}"
K3S_INSTALL_MIRROR="${K3S_INSTALL_MIRROR:-cn}"
APT_MIRROR="${APT_MIRROR:-http://mirrors.aliyun.com}"
APT_MIRROR_FALLBACK="${APT_MIRROR_FALLBACK:-http://deb.debian.org}"
DNS_CHECK_HOST="${DNS_CHECK_HOST:-mirrors.aliyun.com}"

say()  { echo; echo "===== $* ====="; }
info() { echo "  [ok] $*"; }
warn() { echo "  [!!] $*"; }
die()  { echo "  [FATAL] $*"; exit 1; }

exec > >(tee -a "$LOG") 2>&1

[ "$(id -u)" = "0" ] || die "must run as root (this box has no sudo yet: log in as root)"

say "STEP 1/5  DNS"
if grep -qs '^nameserver' /etc/resolv.conf; then
  info "/etc/resolv.conf already has nameservers"
else
  [ -f /etc/resolv.conf ] && cp -a /etc/resolv.conf /etc/resolv.conf.bak.$(date +%s)
  printf 'nameserver 223.5.5.5\nnameserver 119.29.29.29\noptions timeout:2 attempts:2\n' > /etc/resolv.conf
  info "wrote /etc/resolv.conf"
fi
cat /etc/resolv.conf
getent hosts "$DNS_CHECK_HOST" >/dev/null 2>&1 && info "DNS works" || die "DNS still broken"

say "STEP 2/5  apt sources + base tools"
# Always rewrite: the image's only entry is a commented-out cdrom line, and a
# conditional check already failed once. Keep a timestamped backup.
cp -a /etc/apt/sources.list /etc/apt/sources.list.bak.$(date +%s) 2>/dev/null || true
write_sources() {
  local base="$1"
  cat > /etc/apt/sources.list <<EOF
deb $base/debian/ trixie main contrib non-free-firmware
deb $base/debian/ trixie-updates main contrib non-free-firmware
deb $base/debian-security/ trixie-security main contrib non-free-firmware
EOF
}
write_sources "$APT_MIRROR"
for f in /etc/apt/sources.list.d/*.list /etc/apt/sources.list.d/*.sources; do
  [ -e "$f" ] || continue
  grep -q 'cdrom:' "$f" && mv "$f" "$f.disabled" && info "disabled $f"
done
cat /etc/apt/sources.list

apt-get clean
if ! apt-get update; then
  warn "apt-get update failed on $APT_MIRROR, falling back to $APT_MIRROR_FALLBACK"
  write_sources "$APT_MIRROR_FALLBACK"
  apt-get update || die "apt-get update failed on both mirrors"
fi

# Fail fast instead of letting the next steps fail obscurely.
if ! apt-cache policy sudo 2>/dev/null | grep -q 'Candidate: [0-9]'; then
  warn "no candidate for sudo after update -- dumping apt state"
  ls -l /var/lib/apt/lists/ | head
  apt-get update -o Debug::Acquire::http=true 2>&1 | tail -20
  die "apt repository unusable, k3s cannot be installed"
fi

apt-get install -y -o Acquire::Retries=3 sudo iptables curl ca-certificates \
  || die "package installation failed"
for b in sudo iptables curl; do
  command -v "$b" >/dev/null 2>&1 && info "$b -> $(command -v "$b")" || warn "$b still missing"
done
if id -nG "$OWNER" | grep -qw sudo; then
  info "$OWNER already in sudo group"
else
  usermod -aG sudo "$OWNER" && info "added $OWNER to sudo group (takes effect on next login)"
fi

say "STEP 3/5  install k3s"
if [ -x /usr/local/bin/k3s ]; then
  info "k3s already installed: $(/usr/local/bin/k3s --version | head -1)"
else
  SH=/tmp/k3s-install.sh
  if ! wget -q --timeout=30 -O "$SH" "$K3S_INSTALL_URL"; then
    warn "install mirror failed, trying $K3S_INSTALL_FALLBACK_URL"
    wget -q --timeout=30 -O "$SH" "$K3S_INSTALL_FALLBACK_URL" || die "cannot download the k3s install script"
  fi
  INSTALL_K3S_MIRROR="$K3S_INSTALL_MIRROR" sh "$SH" server \
    --node-name="$NODE_NAME" \
    --tls-san="$IP" \
    --node-ip="$IP" \
    --flannel-iface="$IFACE" \
    --write-kubeconfig-mode 644 \
    || die "k3s installer returned non-zero"
  info "k3s installed"
fi
systemctl enable --now k3s >/dev/null 2>&1 || true
systemctl is-active k3s || die "k3s service is not active"

say "STEP 4/5  registry mirrors + wait for Ready"
# The mirror list is NOT part of this repo: copy deploy/k3s-registries.example.yaml
# to deploy/k3s-registries.yaml (gitignored) on the build machine, fill in the
# endpoints, and ship it next to this script as registries.yaml.
REG_SRC=""
for cand in "$DN/registries.yaml" "$DN/k3s-registries.yaml"; do
  [ -f "$cand" ] && REG_SRC="$cand" && break
done
if [ -n "$REG_SRC" ]; then
  install -m 644 "$REG_SRC" /etc/rancher/k3s/registries.yaml
  systemctl restart k3s
  info "registries.yaml installed from $REG_SRC, k3s restarted"
else
  warn "no registries.yaml next to this script; skipping mirror config"
  warn "(copy deploy/k3s-registries.example.yaml -> deploy/k3s-registries.yaml and fill it in)"
fi

echo "  waiting for node Ready (max 4 min)..."
for i in $(seq 1 48); do
  kubectl get nodes 2>/dev/null | grep -q ' Ready' && break
  sleep 5
done
kubectl get nodes -o wide || warn "kubectl get nodes failed"

echo "  waiting for system pods (max 5 min, image pulls can be slow)..."
for i in $(seq 1 60); do
  pending=$(kubectl get pods -A --no-headers 2>/dev/null | grep -vc 'Running\|Completed')
  [ "${pending:-0}" = "0" ] && break
  sleep 5
done
kubectl get pods -A

say "STEP 5/5  RBAC + kubeconfig for the phone"
[ -f "$DN/rbac.yaml" ] && kubectl apply -f "$DN/rbac.yaml" || warn "$DN/rbac.yaml not found"

CA=$(grep -m1 'certificate-authority-data:' /etc/rancher/k3s/k3s.yaml | awk '{print $2}')
[ -n "$CA" ] || die "cannot read cluster CA from /etc/rancher/k3s/k3s.yaml"
TOKEN=$(kubectl -n kube-system create token droidnode-agent --duration=8760h 2>/dev/null)
[ -n "$TOKEN" ] || die "cannot create a token for droidnode-agent (is rbac.yaml applied?)"

KC="$DN/kubeconfig"
cat > "$KC" <<EOF
apiVersion: v1
kind: Config
clusters:
- name: default
  cluster:
    certificate-authority-data: $CA
    server: https://$IP:6443
contexts:
- name: droidnode
  context:
    cluster: default
    user: droidnode-agent
current-context: droidnode
users:
- name: droidnode-agent
  user:
    token: $TOKEN
EOF
chown "$OWNER:$OWNER" "$KC"
chmod 600 "$KC"
info "kubeconfig -> $KC (server https://$IP:6443)"

if [ ! -e "/home/$OWNER/.kube/config" ]; then
  mkdir -p "/home/$OWNER/.kube"
  sed "s|https://127.0.0.1:6443|https://$IP:6443|" /etc/rancher/k3s/k3s.yaml > "/home/$OWNER/.kube/config"
  chown -R "$OWNER:$OWNER" "/home/$OWNER/.kube"
  chmod 600 "/home/$OWNER/.kube/config"
  info "also wrote /home/$OWNER/.kube/config"
fi

say "DONE"
kubectl get nodes -o wide
echo
kubectl get pods -A
echo
echo "node-token (for future Linux worker nodes):"
cat /var/lib/rancher/k3s/server/node-token 2>/dev/null
echo
echo "phone kubeconfig: $KC"
echo "log:              $LOG"
echo "NEXT: go back to Windows and run push_kubeconfig.py"
