param(
    [string]$Serial = "",
    [switch]$WithNetwork
)

# Device coverage: build the tests with -C instrument-coverage, run them on the
# phone, pull the .profraw files and merge them with the llvm-cov that ships in
# the rustup toolchain (same LLVM version as rustc, so the profile version
# matches).
#
#   scripts/rust-coverage-device.ps1 -Serial <serial>
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
$env:RUSTFLAGS = "-C instrument-coverage"

$llvm = Join-Path $env:RUSTUP_HOME "toolchains\$env:RUSTUP_TOOLCHAIN\lib\rustlib\x86_64-pc-windows-gnu\bin"
$profdata = Join-Path $llvm "llvm-profdata.exe"
$cov = Join-Path $llvm "llvm-cov.exe"
if (-not (Test-Path $profdata) -or -not (Test-Path $cov)) {
    throw "llvm-profdata/llvm-cov not found in $llvm (install llvm-tools-preview)"
}

$agent = Join-Path $PSScriptRoot "..\agent"
Push-Location $agent
try {
    & cargo test --workspace --no-run --target aarch64-linux-android 2>&1 |
        Select-String -Pattern "Executable unittests src\\lib.rs" |
        ForEach-Object { Write-Host $_.Line }
    if ($LASTEXITCODE -ne 0) { throw "instrumented build failed" }
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

Invoke-Adb shell "rm -f /data/local/tmp/executor-cov /data/local/tmp/*.profraw" | Out-Null
Invoke-Adb push $bin.FullName /data/local/tmp/executor-cov | Out-Null
Invoke-Adb shell chmod 755 /data/local/tmp/executor-cov | Out-Null

$envs = "TMPDIR=/data/local/tmp LLVM_PROFILE_FILE=/data/local/tmp/cov-%p.profraw"
if (-not $WithNetwork) { $envs += " DROIDNODE_SKIP_NETWORK_TESTS=1" }
Write-Host "== run instrumented tests =="
& $adb @devArgs shell "$envs /data/local/tmp/executor-cov --test-threads=1" 2>&1 |
    Select-String -Pattern "test result" | Select-Object -Last 1

$out = Join-Path (Resolve-Path "$PSScriptRoot\..") "build-cache\cov"
New-Item -ItemType Directory -Force -Path $out | Out-Null
Invoke-Adb shell "ls /data/local/tmp/*.profraw" | ForEach-Object {
    $name = Split-Path $_ -Leaf
    Invoke-Adb pull "/data/local/tmp/$name" (Join-Path $out $name) | Out-Null
}
$raw = Get-ChildItem $out -Filter "*.profraw"
if (-not $raw) { throw "no .profraw files produced" }

$prof = Join-Path $out "tests.profdata"
& $profdata merge -sparse @($raw.FullName) -o $prof 2>&1 | Select-Object -Last 2
if ($LASTEXITCODE -ne 0) { throw "llvm-profdata merge failed" }

Push-Location $agent
try {
    & $cov report $bin.FullName "-instr-profile=$prof" "--ignore-filename-regex=(registry|rustlib|toolchains|\.cargo)" 2>&1 | Select-Object -Last 12
} finally {
    Pop-Location
}
