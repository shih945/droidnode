#!/usr/bin/env bash
# End-to-end smoke test against a connected phone.
#
#   scripts/smoke-device.sh [serial]
#
# Env: ADB (default: adb), APK (default: newest dist/droidnode-debug-*.apk)
#
# Installs the APK, starts the console, checks that the process stays up and
# captures a screenshot plus the crash buffer. Exits non-zero on the first
# failure so it can be used from CI or by hand.
set -euo pipefail

ADB="${ADB:-adb}"
APK="${APK:-$(ls -t dist/droidnode-debug-*.apk 2>/dev/null | head -n1 || true)}"
OUT_DIR="${OUT_DIR:-build-cache/smoke}"

if [[ -z "${APK}" || ! -f "${APK}" ]]; then
    echo "no APK found; build one first or set APK=..." >&2
    exit 2
fi

dev_args=()
if [[ -n "${1:-}" ]]; then
    dev_args=(-s "$1")
fi

adb_cmd() { "${ADB}" "${dev_args[@]}" "$@"; }

echo "== device =="
adb_cmd get-state
adb_cmd shell getprop ro.product.model

echo "== install ${APK} =="
adb_cmd install -r -d "${APK}"

echo "== start console =="
adb_cmd shell am force-stop com.droidnode
adb_cmd shell am start -W -n com.droidnode/.ui.v2.NodeActivity >/dev/null
sleep 6

echo "== process =="
if ! adb_cmd shell pidof com.droidnode >/dev/null; then
    echo "FAIL: com.droidnode is not running" >&2
    adb_cmd logcat -d -b crash | tail -n 50 || true
    exit 1
fi

if ! adb_cmd shell dumpsys activity activities | grep -q "com.droidnode/.ui.v2.NodeActivity"; then
    echo "FAIL: NodeActivity is not the resumed activity" >&2
    exit 1
fi

echo "== crash buffer =="
mkdir -p "${OUT_DIR}"
adb_cmd logcat -d -b crash >"${OUT_DIR}/crash.txt" || true
if grep -q "FATAL EXCEPTION" "${OUT_DIR}/crash.txt"; then
    echo "FAIL: crash found:" >&2
    tail -n 40 "${OUT_DIR}/crash.txt" >&2
    exit 1
fi

echo "== screenshot =="
adb_cmd exec-out screencap -p >"${OUT_DIR}/node.png"
echo "saved ${OUT_DIR}/node.png"

echo "OK: com.droidnode is up on ${1:-the default device}"
