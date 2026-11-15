param(
    [string]$Serial = "",
    [string]$Apk = "",
    [string]$OutDir = "build-cache\smoke"
)

# Windows equivalent of scripts/smoke-device.sh. Installs the APK, starts the
# console, checks the process/resumed activity/crash buffer, then saves a
# screenshot. Exits non-zero on the first failure.
$ErrorActionPreference = "Stop"
$adb = if ($env:ADB) { $env:ADB } else { "adb" }

if (-not $Apk) {
    $Apk = Get-ChildItem dist\droidnode-debug-*.apk |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1 -ExpandProperty FullName
}
if (-not (Test-Path $Apk)) {
    Write-Error "no APK found; build one first or pass -Apk"
}

$devArgs = @()
if ($Serial) { $devArgs = @("-s", $Serial) }
function Invoke-Adb { & $adb @devArgs @args }

Write-Host "== device =="
Invoke-Adb get-state
Invoke-Adb shell getprop ro.product.model

Write-Host "== install $Apk =="
$install = Invoke-Adb install -r -d $Apk
if (($install -join "`n") -notmatch "Success") {
    Write-Error "install failed: $install"
}

Write-Host "== start console =="
Invoke-Adb shell am force-stop com.droidnode
Invoke-Adb shell am start -W -n com.droidnode/.ui.v2.NodeActivity | Out-Null
Start-Sleep -Seconds 8

Write-Host "== process =="
$pidText = Invoke-Adb shell pidof com.droidnode
if (-not $pidText.Trim()) {
    Write-Error "com.droidnode is not running"
}

$resumed = Invoke-Adb shell dumpsys activity activities | Select-String "com.droidnode/.ui.v2.NodeActivity"
if (-not $resumed) {
    Write-Error "NodeActivity is not the resumed activity"
}

Write-Host "== crash buffer =="
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$crash = Join-Path $OutDir "crash.txt"
Invoke-Adb logcat -d -b crash | Set-Content -Path $crash -Encoding utf8
if (Select-String -Path $crash -Pattern "Process: com.droidnode" -Quiet) {
    Write-Error "crash found in $crash"
}

Write-Host "== screenshot =="
$png = Join-Path $OutDir "node.png"
Invoke-Adb shell screencap -p /sdcard/droidnode-smoke.png | Out-Null
Invoke-Adb pull /sdcard/droidnode-smoke.png $png | Out-Null
Write-Host "saved $png"

Write-Host "OK: com.droidnode is up on $(if ($Serial) { $Serial } else { 'the default device' })"
