#!/usr/bin/env bash
# List every adb device and install one APK on all of them
# (Linux/macOS counterpart of scripts/adb-install-all.ps1).
#
#   scripts/adb-install-all.sh                      # latest dist APK
#   scripts/adb-install-all.sh path/to/app.apk
#   scripts/adb-install-all.sh path/to/app.apk --dry-run
set -uo pipefail

ADB="${ADB:-adb}"
APK="${1:-}"
DRY_RUN=0
if [[ "${2:-}" == "--dry-run" ]]; then DRY_RUN=1; fi

if [[ -z "$APK" ]]; then
    APK=$(ls -t "$(dirname "$0")"/../dist/droidnode-debug-*.apk 2>/dev/null | head -n1 || true)
fi
if [[ -z "$APK" || ! -f "$APK" ]]; then
    echo "APK not found; pass an apk path or build one into dist/" >&2
    exit 2
fi
APK=$(cd "$(dirname "$APK")" && pwd)/$(basename "$APK")

echo "== adb devices =="
"$ADB" devices -l

mapfile -t rows < <("$ADB" devices | tail -n +2 | awk 'NF >= 2 { print $1 "\t" $2 }')
if [[ ${#rows[@]} -eq 0 ]]; then
    echo "no adb devices connected" >&2
    exit 1
fi

echo "== install $APK on ${#rows[@]} device(s) =="
installed=0
failed=()
for row in "${rows[@]}"; do
    serial="${row%%$'\t'*}"
    state="${row#*$'\t'}"
    if [[ "$state" != "device" ]]; then
        echo "skip $serial: $state" >&2
        continue
    fi
    if [[ "$DRY_RUN" == "1" ]]; then
        echo "dry-run: $ADB -s $serial install -r -d \"$APK\""
        continue
    fi

    echo "== $serial =="
    out=$("$ADB" -s "$serial" install -r -d "$APK" 2>&1)
    echo "$out"
    if grep -q "Success" <<<"$out"; then
        installed=$((installed + 1))
    else
        failed+=("$serial")
    fi
done

if [[ "$DRY_RUN" == "1" ]]; then
    echo "dry-run complete"
    exit 0
fi
if [[ ${#failed[@]} -gt 0 ]]; then
    echo "install failed on: ${failed[*]}" >&2
    exit 1
fi
echo "OK: installed on $installed device(s)"
