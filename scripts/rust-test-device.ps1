param(
    [string]$Serial = "",
    [switch]$WithNetwork
)

# Run the Rust unit tests on an Android device without CI.
#
# The executor crate is Linux/Android-only, so it cannot be tested on a Windows
# host. This cross-compiles the test binary for aarch64-linux-android and runs it
# through adb (the device has the same libc/Android runtime the agent targets).
#
#   scripts/rust-test-device.ps1 -Serial <serial>
#
# Network tests (docker.io pulls) are skipped unless -WithNetwork is passed.
$ErrorActionPreference = "Continue"
$adb = if ($env:ADB) { $env:ADB } else { "adb" }
if (-not $env:RUSTUP_HOME) { $env:RUSTUP_HOME = "D:\app\rust\rustup-home" }
if (-not $env:CARGO_HOME) { $env:CARGO_HOME = "D:\app\rust\cargo-home" }
if (-not $env:RUSTUP_TOOLCHAIN) { $env:RUSTUP_TOOLCHAIN = "stable-x86_64-pc-windows-gnu" }
$env:PATH = "$env:CARGO_HOME\bin;D:\app\mingw64\bin;$env:PATH"

$ndk = if ($env:ANDROID_NDK_HOME) { $env:ANDROID_NDK_HOME } else {
    "C:\Users\SHIH\AppData\Local\Android\Sdk\ndk\28.2.13676358"
}
$tc = Join-Path $ndk "toolchains\llvm\prebuilt\windows-x86_64\bin"
$env:CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = Join-Path $tc "aarch64-linux-android35-clang.cmd"
$env:CC_aarch64_linux_android = $env:CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER
$env:AR_aarch64_linux_android = Join-Path $tc "llvm-ar.exe"

$agent = Join-Path $PSScriptRoot "..\agent"
Push-Location $agent
try {
    & cargo test --workspace --no-run --target aarch64-linux-android 2>&1 |
        Select-String -Pattern "Executable unittests src\\lib.rs" |
        ForEach-Object { Write-Host $_.Line }
    if ($LASTEXITCODE -ne 0) { throw "cargo test --no-run failed" }
} finally {
    Pop-Location
}

$bin = Get-ChildItem (Join-Path $agent "target\aarch64-linux-android\debug\deps") -Filter "executor-*" |
    Where-Object { $_.Extension -eq "" } |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1
if (-not $bin) { throw "executor test binary not found" }

$devArgs = @()
if ($Serial) { $devArgs = @("-s", $Serial) }
function Invoke-Adb { & $adb @devArgs @args }

Write-Host "== push $(Split-Path $bin -Leaf) ($([math]::Round($bin.Length/1MB,1)) MB) =="
Invoke-Adb push $bin.FullName /data/local/tmp/executor-tests | Out-Null
Invoke-Adb shell chmod 755 /data/local/tmp/executor-tests | Out-Null

$envs = "TMPDIR=/data/local/tmp"
if (-not $WithNetwork) { $envs += " DROIDNODE_SKIP_NETWORK_TESTS=1" }
Write-Host "== run (android, $envs) =="
$output = & $adb @devArgs shell "$envs /data/local/tmp/executor-tests --test-threads=1" 2>&1
$output | Select-Object -Last 15
$result = ($output | Select-String -Pattern "test result: (ok|FAILED)\. (\d+) passed; (\d+) failed") | Select-Object -Last 1
if (-not $result) { throw "could not parse test result" }
if ($result.Line -match "test result: FAILED") { exit 1 }

exit 0
