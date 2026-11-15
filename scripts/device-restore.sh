#!/usr/bin/env bash
# Restore the app after instrumentation tests / a wipe (Linux/macOS counterpart
# of scripts/device-restore.ps1).
#
#   scripts/device-restore.sh [serial]
#
# Env: ADB, NODE_NAME (dn-k10), PORT (10255), LANGUAGE (zh), APPEARANCE (light),
#      FORCE=1 to rewrite an already-configured install.
set -euo pipefail

ADB="${ADB:-adb}"
NODE_NAME="${NODE_NAME:-dn-k10}"
PORT="${PORT:-10255}"
LANGUAGE="${LANGUAGE:-zh}"
APPEARANCE="${APPEARANCE:-light}"
FORCE="${FORCE:-0}"

dev_args=()
if [[ -n "${1:-}" ]]; then dev_args=(-s "$1"); fi
adb_cmd() { "${ADB}" "${dev_args[@]}" "$@"; }

adb_cmd get-state >/dev/null

has_enc=0
if adb_cmd shell run-as com.droidnode test -f files/kubeconfig.enc >/dev/null 2>&1; then
    has_enc=1
fi
if [[ "$has_enc" == "1" && "$FORCE" != "1" ]]; then
    echo "app already configured; set FORCE=1 to rewrite"
    exit 0
fi

echo "== stop app =="
adb_cmd shell am force-stop com.droidnode >/dev/null

tmp=/data/local/tmp/droidnode-restore-kubeconfig
if adb_cmd shell test -f /sdcard/Download/droidnode-kubeconfig >/dev/null 2>&1; then
    adb_cmd shell "cp /sdcard/Download/droidnode-kubeconfig $tmp && chmod 644 $tmp"
    adb_cmd shell run-as com.droidnode cp "$tmp" files/kubeconfig
    adb_cmd shell rm -f "$tmp"
    echo "kubeconfig imported (will be encrypted on next app start)"
elif [[ "$has_enc" == "1" ]]; then
    echo "keeping the existing encrypted kubeconfig"
else
    echo "no kubeconfig on the device: push it to /sdcard/Download/droidnode-kubeconfig" >&2
    exit 1
fi

prefs_local=$(mktemp)
cat > "$prefs_local" <<EOF
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <string name="appearance">$APPEARANCE</string>
    <string name="node_name">$NODE_NAME</string>
    <string name="log_level">info</string>
    <long name="drained_requested_at" value="0" />
    <int name="kubelet_port" value="$PORT" />
    <string name="language">$LANGUAGE</string>
    <boolean name="autostart" value="true" />
</map>
EOF
adb_cmd shell run-as com.droidnode mkdir -p shared_prefs
adb_cmd push "$prefs_local" /data/local/tmp/droidnode-restore-prefs.xml >/dev/null
adb_cmd shell "chmod 644 /data/local/tmp/droidnode-restore-prefs.xml"
adb_cmd shell run-as com.droidnode cp /data/local/tmp/droidnode-restore-prefs.xml shared_prefs/droidnode_console.xml
adb_cmd shell rm -f /data/local/tmp/droidnode-restore-prefs.xml
rm -f "$prefs_local"

echo "== start app =="
adb_cmd shell am start -W -n com.droidnode/.ui.v2.NodeActivity >/dev/null
sleep 12
adb_cmd shell pidof com.droidnode >/dev/null

enc=0
if adb_cmd shell run-as com.droidnode test -f files/kubeconfig.enc >/dev/null 2>&1; then
    enc=1
fi
echo "restored: node=$NODE_NAME port=$PORT kubeconfig.enc=$enc"
