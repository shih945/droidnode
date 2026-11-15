#!/usr/bin/env bash
# Run the Rust unit tests on an Android device (Linux/macOS counterpart of
# scripts/rust-test-device.ps1).
#
#   scripts/rust-test-device.sh [serial]
#
# Env: ADB, ANDROID_NDK_HOME, CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER.
# Network tests are skipped unless WITH_NETWORK=1.
set -euo pipefail

ADB="${ADB:-adb}"
WITH_NETWORK="${WITH_NETWORK:-0}"

dev_args=()
if [[ -n "${1:-}" ]]; then dev_args=(-s "$1"); fi
adb_cmd() { "${ADB}" "${dev_args[@]}" "$@"; }

NDK="${ANDROID_NDK_HOME:-${ANDROID_HOME:-$HOME/Android/Sdk}/ndk/28.2.13676358}"
TC="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$TC/aarch64-linux-android35-clang"
export CC_aarch64_linux_android="$CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER"
export AR_aarch64_linux_android="$TC/llvm-ar"

cd "$(dirname "$0")/../agent"
cargo test --workspace --no-run --target aarch64-linux-android

BIN=$(ls -t target/aarch64-linux-android/debug/deps/executor-* | grep -v '\.d$' | head -n1)
echo "== push $(basename "$BIN") =="
adb_cmd push "$BIN" /data/local/tmp/executor-tests >/dev/null
adb_cmd shell chmod 755 /data/local/tmp/executor-tests

envs="TMPDIR=/data/local/tmp"
if [[ "$WITH_NETWORK" != "1" ]]; then envs="$envs DROIDNODE_SKIP_NETWORK_TESTS=1"; fi
echo "== run ($envs) =="
out=$(adb_cmd shell "$envs /data/local/tmp/executor-tests --test-threads=1" 2>&1)
echo "$out" | tail -n 15
echo "$out" | grep -q "test result: ok" || exit 1
