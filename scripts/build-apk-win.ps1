param(
    [switch]$CheckToolchain,
    [switch]$NoVersionBump,
    [switch]$SkipProot
)

# Build a debug APK for DroidNode on this Windows machine.
#
# Run it from a normal PowerShell / cmd window (NOT elevated):
#     cd <repo>
#     powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-apk-win.ps1
#
# Toolchain locations are NEVER hardcoded in this file. They are resolved in
# this order:
#     1. toolchain.env.local   (gitignored; copy toolchain.env.example first)
#     2. variables already set in the environment (ANDROID_HOME, JAVA_HOME, ...)
#     3. auto-detection        (PATH, %LOCALAPPDATA%\Android\Sdk, rustup defaults)
# Run with -CheckToolchain to print the resolved paths and exit without building.
#
# NOTE: this file is intentionally pure ASCII. Windows PowerShell 5.1 reads
# .ps1 files as ANSI (GBK on this machine); non-ASCII characters corrupt the
# parser. Keep it ASCII-only when editing.
#
# Why it builds in place:
#   Everything (cargo target, gradle build, intermediate files) stays inside the
#   repository. Do NOT add staging copies outside the project.
#
# Pipeline (equivalent to scripts/build-android.sh + ./gradlew assembleDebug):
#   1. resolve toolchains
#   2. rustup target add aarch64-linux-android      (network)
#   3. cargo build --release -p node-agent          -> libnode_agent.so
#   4. scripts/build-proot-win.py                   -> libproot.so / libtalloc2x.so / libproot_loader.so
#   5. bump android/app/build.gradle versionName (0.1.0 -> 0.1.1, ...) and versionCode
#   6. gradle assembleDebug (wrapper, or $env:GRADLE_HOME when set)
#   7. copy the APK to <repo>\dist\
#
# Pass -NoVersionBump to rebuild the same version (e.g. while iterating on a fix).
# A failed gradle run rolls the version bump back.

$ErrorActionPreference = 'Continue'   # native tools write progress to stderr

$Root  = Split-Path -Parent $PSScriptRoot
$Repo  = $Root
$Jni   = Join-Path $Repo 'android\app\src\main\jniLibs\arm64-v8a'
$Dist  = Join-Path $Root 'dist'

$script:StepNo = 0
function Say($msg) { Write-Host $msg }
function Step($msg) {
    $script:StepNo++
    Write-Host ''
    Write-Host ('==== [{0}] {1}   {2:HH:mm:ss} ====' -f $script:StepNo, $msg, (Get-Date))
}
function Fail($msg) {
    Write-Host ''
    Write-Host ('BUILD FAILED: ' + $msg) -ForegroundColor Red
    Write-Host 'Set the missing path in toolchain.env.local (see toolchain.env.example).' -ForegroundColor Yellow
    Write-Host 'Paste the last 30 lines above back to the assistant.' -ForegroundColor Yellow
    exit 1
}

Say ('repo  : ' + $Root)

# -- 0. local toolchain env file ---------------------------------------------
# KEY=VALUE lines, `#` comments. Values here override the real environment.
function Read-LocalEnv([string]$Path) {
    if (-not (Test-Path $Path)) { return 0 }
    $n = 0
    foreach ($raw in (Get-Content -Path $Path)) {
        $line = $raw.Trim()
        if (-not $line) { continue }
        if ($line.StartsWith('#')) { continue }
        $eq = $line.IndexOf('=')
        if ($eq -lt 1) { continue }
        $key = $line.Substring(0, $eq).Trim()
        $val = $line.Substring($eq + 1).Trim()
        if ($val.Length -ge 2 -and $val.StartsWith('"') -and $val.EndsWith('"')) {
            $val = $val.Substring(1, $val.Length - 2)
        } elseif ($val.Length -ge 2 -and $val.StartsWith("'") -and $val.EndsWith("'")) {
            $val = $val.Substring(1, $val.Length - 2)
        }
        if ($key -notmatch '^[A-Za-z_][A-Za-z0-9_]*$') { continue }
        if (-not $val) { continue }
        Set-Item -Path ('env:' + $key) -Value $val
        $n++
    }
    return $n
}

$envFile = Join-Path $Root 'toolchain.env.local'
$envCount = Read-LocalEnv $envFile
if (Test-Path $envFile) {
    Say ('env   : toolchain.env.local (' + $envCount + ' entries)')
} else {
    Say 'env   : no toolchain.env.local (copy toolchain.env.example to set paths)'
}

$TcRoot = $env:DROIDNODE_TOOLCHAIN_ROOT
function TcPath([string]$rel) {
    if ($TcRoot) { return (Join-Path $TcRoot $rel) }
    return $null
}

# -- 1. sanity check (in-place build) ---------------------------------------
Step 'check the working tree'
if (-not (Test-Path (Join-Path $Repo 'agent\Cargo.toml'))) { Fail 'not a DroidNode checkout: no agent\Cargo.toml' }

# -- 2. resolve Android SDK --------------------------------------------------
Step 'resolve Android SDK'
$sdkCandidates = @($env:ANDROID_HOME, $env:ANDROID_SDK_ROOT, (TcPath 'android-sdk'))
if ($env:LOCALAPPDATA) { $sdkCandidates += (Join-Path $env:LOCALAPPDATA 'Android\Sdk') }
$Sdk = $null
foreach ($cand in $sdkCandidates) {
    if (-not $cand) { continue }
    if (-not (Test-Path (Join-Path $cand 'build-tools'))) { continue }
    $platforms = Join-Path $cand 'platforms'
    if (-not (Test-Path $platforms)) { continue }
    $anyPlatform = Get-ChildItem $platforms -Directory -ErrorAction SilentlyContinue |
                   Where-Object { $_.Name -like 'android-*' } | Select-Object -First 1
    if ($anyPlatform) { $Sdk = $cand; break }
}
if (-not $Sdk) {
    Fail 'no Android SDK found. Set ANDROID_HOME in toolchain.env.local (needs platforms\android-* and build-tools\).'
}
Say ('  SDK         = ' + $Sdk)
Say ('  platforms   = ' + ((Get-ChildItem (Join-Path $Sdk 'platforms') -Directory | ForEach-Object { $_.Name }) -join ', '))
Say ('  build-tools = ' + ((Get-ChildItem (Join-Path $Sdk 'build-tools') -Directory | ForEach-Object { $_.Name }) -join ', '))

# -- 3. resolve NDK clang ----------------------------------------------------
Step 'resolve NDK clang for aarch64-linux-android'
$ndkCandidates = @($env:ANDROID_NDK_HOME, $env:ANDROID_NDK)
$ndkRoot = Join-Path $Sdk 'ndk'
if (Test-Path $ndkRoot) {
    Get-ChildItem $ndkRoot -Directory | Sort-Object Name -Descending |
        ForEach-Object { $ndkCandidates += $_.FullName }
}
$ClangCmd = $null
foreach ($ndk in $ndkCandidates) {
    if (-not $ndk) { continue }
    $bin = Join-Path $ndk 'toolchains\llvm\prebuilt\windows-x86_64\bin'
    if (-not (Test-Path $bin)) { continue }
    $found = Get-ChildItem $bin -Filter 'aarch64-linux-android*-clang.cmd' -ErrorAction SilentlyContinue |
             Sort-Object Name -Descending | Select-Object -First 1
    if ($found) { $ClangCmd = $found.FullName; break }
}
if (-not $ClangCmd) { Fail ('no aarch64-linux-android*-clang.cmd under ' + $ndkRoot + ' (set ANDROID_NDK_HOME)') }
$BinDir = Split-Path -Parent $ClangCmd
$CxxCmd = Join-Path $BinDir ((Split-Path -Leaf $ClangCmd) -replace '-clang\.cmd$', '-clang++.cmd')
Say ('  clang       = ' + $ClangCmd)
Say ('  clang++     = ' + (Test-Path $CxxCmd))

# -- 4. resolve JDK ----------------------------------------------------------
Step 'resolve JDK 17'
function Get-JdkVersion([string]$jdkDir) {
    $release = Join-Path $jdkDir 'release'
    if (-not (Test-Path $release)) { return $null }
    foreach ($line in (Get-Content $release)) {
        if ($line -match 'JAVA_VERSION="([^"]+)"') { return $matches[1] }
    }
    return $null
}
$jdkCandidates = @($env:JAVA_HOME, (TcPath 'jdk'))
foreach ($glob in @(
    'C:\Program Files\Java\jdk-17*',
    'C:\Program Files\Java\jdk-21*',
    'C:\Program Files\Eclipse Adoptium\jdk-17*',
    'C:\Program Files\Amazon Corretto\jdk17*',
    'C:\Program Files\Microsoft\jdk-17*'
)) {
    Get-Item -Path $glob -ErrorAction SilentlyContinue | ForEach-Object { $jdkCandidates += $_.FullName }
}
$Jdk = $null
$JdkVer = $null
$fallbackJdk = $null
$fallbackVer = $null
foreach ($cand in $jdkCandidates) {
    if (-not $cand) { continue }
    if (-not (Test-Path (Join-Path $cand 'bin\java.exe'))) { continue }
    $v = Get-JdkVersion $cand
    if ($v -and $v.StartsWith('17')) { $Jdk = $cand; $JdkVer = $v; break }
    if (-not $fallbackJdk) { $fallbackJdk = $cand; $fallbackVer = $v }
}
if (-not $Jdk) { $Jdk = $fallbackJdk; $JdkVer = $fallbackVer }
if (-not $Jdk) { Fail 'no JDK found. Set JAVA_HOME in toolchain.env.local (JDK 17 recommended).' }
Say ('  JAVA_HOME   = ' + $Jdk)
Say ('  version     = ' + $JdkVer)
& (Join-Path $Jdk 'bin\java.exe') -version 2>&1 | ForEach-Object { Say ('  ' + $_) }
if ($LASTEXITCODE -ne 0) { Fail ('java -version failed for ' + $Jdk) }

# -- 5. resolve Rust ---------------------------------------------------------
Step 'resolve Rust (rustup + cargo)'
$cargoHome = $env:CARGO_HOME
$cargoExe = $null
if ($cargoHome) {
    $candidate = Join-Path $cargoHome 'bin\cargo.exe'
    if (Test-Path $candidate) { $cargoExe = $candidate }
}
if (-not $cargoExe) {
    $cmd = Get-Command cargo -ErrorAction SilentlyContinue
    if ($cmd) {
        $cargoExe = $cmd.Source
        $cargoHome = Split-Path -Parent (Split-Path -Parent $cmd.Source)
    }
}
if (-not $cargoExe) {
    $guesses = @()
    $tcGuess = TcPath 'rust\cargo-home'
    if ($tcGuess) { $guesses += $tcGuess }
    if ($env:USERPROFILE) { $guesses += (Join-Path $env:USERPROFILE '.cargo') }
    foreach ($g in $guesses) {
        $candidate = Join-Path $g 'bin\cargo.exe'
        if (Test-Path $candidate) { $cargoHome = $g; $cargoExe = $candidate; break }
    }
}
if (-not $cargoExe) { Fail 'cargo not found. Set CARGO_HOME in toolchain.env.local.' }

$rustupHome = $env:RUSTUP_HOME
if (-not $rustupHome) {
    $sibling = Join-Path (Split-Path -Parent $cargoHome) 'rustup-home'
    if (Test-Path $sibling) { $rustupHome = $sibling }
}
if (-not $rustupHome -and $env:USERPROFILE) {
    $default = Join-Path $env:USERPROFILE '.rustup'
    if (Test-Path $default) { $rustupHome = $default }
}
$rustup = Join-Path $cargoHome 'bin\rustup.exe'
if (-not (Test-Path $rustup)) { Fail ('rustup not found at ' + $rustup + ' (set RUSTUP_HOME/CARGO_HOME)') }

# Host toolchain: this repo builds with the GNU toolchain on Windows (the MSVC
# one needs link.exe from Visual Studio, which a plain MinGW setup does not
# have). Honour an explicit RUSTUP_TOOLCHAIN, otherwise prefer GNU when present.
$mingwHome = $env:MINGW_HOME
if (-not $mingwHome) { $mingwHome = TcPath 'mingw64' }
$gnuToolchain = 'stable-x86_64-pc-windows-gnu'
$gnuBin = Join-Path $rustupHome ('toolchains\' + $gnuToolchain + '\bin')
if (-not $env:RUSTUP_TOOLCHAIN -and (Test-Path (Join-Path $gnuBin 'cargo.exe'))) {
    $env:RUSTUP_TOOLCHAIN = $gnuToolchain
    $cargoExe = Join-Path $gnuBin 'cargo.exe'
    if ($mingwHome -and (Test-Path (Join-Path $mingwHome 'bin'))) {
        $env:PATH = (Join-Path $mingwHome 'bin') + ';' + $env:PATH
    }
}

Say ('  CARGO_HOME  = ' + $cargoHome)
Say ('  RUSTUP_HOME = ' + $rustupHome)
Say ('  cargo       = ' + $cargoExe)
Say ('  toolchain   = ' + $(if ($env:RUSTUP_TOOLCHAIN) { $env:RUSTUP_TOOLCHAIN } else { 'rustup default' }))
if ($env:RUSTUP_TOOLCHAIN -like '*-gnu') {
    Say ('  mingw       = ' + $(if ($mingwHome) { $mingwHome } else { 'MISSING (cargo will need a GNU linker on PATH)' }))
}

# -- 6. resolve Gradle (optional; wrapper is the default) --------------------
Step 'resolve Gradle (wrapper unless GRADLE_HOME is set)'
$GradleBat = $null
if ($env:GRADLE_HOME) {
    $candidate = Join-Path $env:GRADLE_HOME 'bin\gradle.bat'
    if (Test-Path $candidate) { $GradleBat = $candidate }
}
$tcGradle = TcPath 'gradle'
if (-not $GradleBat -and $tcGradle) {
    $candidate = Join-Path $tcGradle 'bin\gradle.bat'
    if (Test-Path $candidate) { $GradleBat = $candidate }
}
if ($GradleBat) { Say ('  gradle      = ' + $GradleBat) } else { Say '  gradle      = wrapper (android\gradle\wrapper)' }

# -- 7. resolve Python (proot staging) ---------------------------------------
$py = $env:PYTHON
if (-not $py) {
    $pyCmd = Get-Command python -ErrorAction SilentlyContinue
    if ($pyCmd) { $py = $pyCmd.Source }
}
if (-not $py) {
    $pyLauncher = Get-Command py -ErrorAction SilentlyContinue
    if ($pyLauncher) { $py = $pyLauncher.Source }
}

if ($CheckToolchain) {
    Say ''
    Say ('SDK    = ' + $Sdk)
    Say ('NDK    = ' + $ndkRoot)
    Say ('CLANG  = ' + $ClangCmd)
    Say ('JDK    = ' + $Jdk)
    Say ('CARGO  = ' + $cargoExe)
    Say ('RUSTUP = ' + $rustup)
    Say ('TOOLCHAIN = ' + $(if ($env:RUSTUP_TOOLCHAIN) { $env:RUSTUP_TOOLCHAIN } else { 'rustup default' }))
    Say ('GRADLE = ' + $(if ($GradleBat) { $GradleBat } else { 'wrapper' }))
    Say ('PYTHON = ' + $(if ($py) { $py } else { 'MISSING (needed for proot staging)' }))
    Say ''
    Say 'toolchain OK'
    exit 0
}

if (-not $py) { Fail 'python not found. Set PYTHON in toolchain.env.local or put python on PATH.' }
Say ('  python      = ' + $py)

Say ('  source files: ' + (Get-ChildItem $Repo -Recurse -File -Force |
        Where-Object { $_.FullName -notmatch '\\(target|build|\.gradle|\.git|dist)\\' } |
        Measure-Object).Count)
New-Item -ItemType Directory -Force -Path $Jni | Out-Null
New-Item -ItemType Directory -Force -Path $Dist | Out-Null

$env:RUSTUP_HOME = $rustupHome
$env:CARGO_HOME  = $cargoHome

# -- 8. Rust target ----------------------------------------------------------
Step 'rustup target add aarch64-linux-android'
& $rustup target add aarch64-linux-android
if ($LASTEXITCODE -ne 0) { Fail 'rustup target add failed' }
& $rustup target list --installed | ForEach-Object { Say ('  installed: ' + $_) }

# -- 9. cargo build ----------------------------------------------------------
Step 'cargo build --release -p node-agent (aarch64-linux-android)'
$env:CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = $ClangCmd
$env:CC_aarch64_linux_android     = $ClangCmd
$env:CXX_aarch64_linux_android    = $CxxCmd
$env:AR_aarch64_linux_android     = Join-Path $BinDir 'llvm-ar.exe'
$env:RANLIB_aarch64_linux_android = Join-Path $BinDir 'llvm-ranlib.exe'
Push-Location (Join-Path $Repo 'agent')
& $cargoExe build --release -p node-agent --target aarch64-linux-android
$cargoExit = $LASTEXITCODE
Pop-Location
if ($cargoExit -ne 0) { Fail ('cargo build failed, exit ' + $cargoExit) }

# Android targets produce an extension-less ELF (no .exe); accept both so the
# script also works if someone builds a Windows host binary by mistake.
$agentExe = Join-Path $Repo 'agent\target\aarch64-linux-android\release\node-agent.exe'
if (-not (Test-Path $agentExe)) {
    $agentExe = Join-Path $Repo 'agent\target\aarch64-linux-android\release\node-agent'
}
if (-not (Test-Path $agentExe)) { Fail ('expected binary not found: ' + $agentExe) }
Copy-Item $agentExe (Join-Path $Jni 'libnode_agent.so') -Force
$agentMb = [math]::Round((Get-Item $agentExe).Length / 1MB, 1)
Say ('  node-agent ' + $agentMb + ' MB -> libnode_agent.so')

# -- 10. proot / libtalloc / loader ------------------------------------------
if ($SkipProot) {
    Say '  skipped (-SkipProot): reusing the staged proot libraries'
} else {
    Step 'fetch proot + libtalloc + loader (python; no patchelf needed)'
    & $py (Join-Path $Repo 'scripts\build-proot-win.py')
    if ($LASTEXITCODE -ne 0) { Fail 'proot staging failed' }
}

# -- 11. bump the app version ------------------------------------------------
# versionName/versionCode live in android/app/build.gradle. Every packaged APK
# gets the next patch version (0.1.0 -> 0.1.1 -> 0.1.2 ...) so the Settings
# "About" row and `adb shell dumpsys package` always match the build.
$versionPath = Join-Path $Repo 'android\app\build.gradle'
# Windows PowerShell 5.1's `Set-Content -Encoding UTF8` prepends a BOM, and
# Gradle refuses to parse a build file that starts with U+FEFF. Write UTF-8
# without a BOM through .NET instead.
function Write-TextNoBom([string]$Path, [string]$Text) {
    [System.IO.File]::WriteAllText($Path, $Text, (New-Object System.Text.UTF8Encoding($false)))
}
$versionBefore = $null
$versionName = $null
$versionCode = $null
if (-not $NoVersionBump) {
    Step 'bump app version'
    $versionBefore = Get-Content $versionPath -Raw -Encoding UTF8
    $codeMatch = [regex]::Match($versionBefore, '(?m)^(\s*versionCode\s+)(\d+)\s*$')
    $nameMatch = [regex]::Match($versionBefore, '(?m)^(\s*versionName\s+")([^"]+)(")')
    if (-not $codeMatch.Success -or -not $nameMatch.Success) {
        Fail 'cannot parse versionCode/versionName in android/app/build.gradle'
    }
    $versionCode = [int]$codeMatch.Groups[2].Value + 1
    $parts = $nameMatch.Groups[2].Value.Split('.')
    if ($parts.Length -ne 3) { Fail ('unexpected versionName ' + $nameMatch.Groups[2].Value) }
    $versionName = '{0}.{1}.{2}' -f $parts[0], $parts[1], ([int]$parts[2] + 1)

    $updated = [regex]::Replace($versionBefore, '(?m)^(\s*versionCode\s+)\d+\s*$', ('${1}' + $versionCode))
    $updated = [regex]::Replace($updated, '(?m)^(\s*versionName\s+")[^"]+(")', ('${1}' + $versionName + '${2}'))
    Write-TextNoBom $versionPath $updated
    Say ('  versionName ' + $versionName + '   versionCode ' + $versionCode)
} else {
    Step 'version bump skipped (-NoVersionBump)'
    $nameMatch = [regex]::Match((Get-Content $versionPath -Raw -Encoding UTF8), '(?m)^\s*versionName\s+"([^"]+)"')
    if ($nameMatch.Success) { $versionName = $nameMatch.Groups[1].Value }
}

# -- 12. gradle assembleDebug ------------------------------------------------
Step 'gradle assembleDebug'
$sdkProp = $Sdk -replace '\\', '/'
$lp = @(
    '# generated by build-apk-win.ps1',
    ('sdk.dir=' + $sdkProp)
)
Set-Content -Path (Join-Path $Repo 'android\local.properties') -Value $lp -Encoding ASCII
$env:JAVA_HOME        = $Jdk
$env:ANDROID_HOME     = $Sdk
$env:ANDROID_SDK_ROOT = $Sdk
Push-Location (Join-Path $Repo 'android')
if ($GradleBat) {
    & $GradleBat 'assembleDebug' '--no-daemon'
} else {
    $wrapperProps = Join-Path $Repo 'android\gradle\wrapper\gradle-wrapper.properties'
    $jdkMajor = 17
    if ($JdkVer) { $jdkMajor = [int]($JdkVer.Split('.')[0]) }
    if ($jdkMajor -le 17) { $wantGradle = 'gradle-8.4-bin.zip' } else { $wantGradle = 'gradle-8.7-bin.zip' }
    $wrapperText = Get-Content $wrapperProps -Raw
    if ($wrapperText -notmatch ([regex]::Escape($wantGradle))) {
        $wrapperText = $wrapperText -replace 'gradle-[\d.]+-bin\.zip', $wantGradle
        Set-Content -Path $wrapperProps -Value $wrapperText -Encoding ASCII
        Say ('  distributionUrl -> ' + $wantGradle)
    } else {
        Say ('  distributionUrl already uses ' + $wantGradle)
    }
    $java       = Join-Path $Jdk 'bin\java.exe'
    $wrapperJar = Join-Path $Repo 'android\gradle\wrapper\gradle-wrapper.jar'
    & $java '-Dorg.gradle.appname=gradlew' '-classpath' $wrapperJar 'org.gradle.wrapper.GradleWrapperMain' 'assembleDebug' '--no-daemon' '--stacktrace'
}
$gradleExit = $LASTEXITCODE
Pop-Location
if ($gradleExit -ne 0) {
    if ($null -ne $versionBefore) {
        Write-TextNoBom $versionPath $versionBefore
        Say '  version bump rolled back (build failed)'
    }
    Fail ('gradle assembleDebug failed, exit ' + $gradleExit)
}

# -- 12. collect -------------------------------------------------------------
Step 'result'
$apk = Join-Path $Repo 'android\app\build\outputs\apk\debug\app-debug.apk'
if (-not (Test-Path $apk)) { Fail ('APK not produced at ' + $apk) }
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$out = Join-Path $Dist ('droidnode-debug-' + $stamp + '.apk')
$copied = $true
try {
    Copy-Item $apk $out -Force -ErrorAction Stop
} catch {
    $copied = $false
}

Say ''
if ($copied) {
    $mb = [math]::Round((Get-Item $out).Length / 1MB, 2)
    Say ('APK     : ' + $out)
    Say ('size    : ' + $mb + ' MB')
    Say ('version : ' + $versionName + ' (versionCode ' + $versionCode + ')')
    Say ('sha256  : ' + (Get-FileHash $out -Algorithm SHA256).Hash)
} else {
    Say ('APK     : ' + $apk + '   (copy to dist\ failed; use this path)')
}
Say 'native libs:'
Get-ChildItem $Jni -File | ForEach-Object { Say ('  ' + $_.Name.PadRight(22) + ' ' + $_.Length + ' bytes') }
Say ''
Say 'APK READY'
Write-Host ''
Write-Host ('install: adb install -r "' + $out + '"') -ForegroundColor Green
