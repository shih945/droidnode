param(
    [string]$Serial = "",
    [string]$NodeName = "dn-k10",
    [int]$Port = 10255,
    [string]$Language = "zh",
    [string]$Appearance = "light",
    [switch]$Force
)

# Restore the app after instrumentation tests / a wipe.
#
# connectedDebugAndroidTest uninstalls the app, which deletes files/node/config/kubeconfig.enc
# and shared_prefs. This script recreates both from the kubeconfig kept at
# /sdcard/Download/droidnode-kubeconfig and the parameters below.
#
#   scripts/device-restore.ps1 -Serial <serial> [-Force]
$ErrorActionPreference = "Stop"
$adb = if ($env:ADB) { $env:ADB } else { "adb" }
$devArgs = @()
if ($Serial) { $devArgs = @("-s", $Serial) }
function Invoke-Adb { & $adb @devArgs @args }

Invoke-Adb get-state | Out-Null

$hasEnc = (Invoke-Adb shell "run-as com.droidnode test -f files/node/config/kubeconfig.enc && echo yes") -join " "
if ($LASTEXITCODE -eq 0 -and -not $Force -and $hasEnc -match "yes") {
    Write-Host "app already configured; use -Force to rewrite"
    exit 0
}

Write-Host "== stop app =="
Invoke-Adb shell am force-stop com.droidnode | Out-Null

$tmpKube = "/data/local/tmp/droidnode-restore-kubeconfig"
$kubeOnDevice = (Invoke-Adb shell "test -f /sdcard/Download/droidnode-kubeconfig && echo yes") -join " "
if ($kubeOnDevice -match "yes") {
    Invoke-Adb shell cp /sdcard/Download/droidnode-kubeconfig $tmpKube | Out-Null
    Invoke-Adb shell chmod 644 $tmpKube | Out-Null
    Invoke-Adb shell run-as com.droidnode mkdir -p files/node/config | Out-Null
    Invoke-Adb shell run-as com.droidnode cp $tmpKube files/node/config/kubeconfig | Out-Null
    Invoke-Adb shell rm -f $tmpKube | Out-Null
    Write-Host "kubeconfig imported (will be encrypted on next app start)"
} elseif ($hasEnc -match "yes") {
    Write-Host "keeping the existing encrypted kubeconfig"
} else {
    Write-Error "no kubeconfig on the device: push it to /sdcard/Download/droidnode-kubeconfig"
}

$prefs = @"
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <string name="appearance">$Appearance</string>
    <string name="node_name">$NodeName</string>
    <string name="log_level">info</string>
    <long name="drained_requested_at" value="0" />
    <int name="kubelet_port" value="$Port" />
    <string name="language">$Language</string>
    <boolean name="autostart" value="true" />
</map>
"@
$local = Join-Path $env:TEMP "droidnode-console-prefs.xml"
[System.IO.File]::WriteAllText($local, $prefs, (New-Object System.Text.UTF8Encoding($false)))
$tmpPrefs = "/data/local/tmp/droidnode-restore-prefs.xml"
Invoke-Adb push $local $tmpPrefs | Out-Null
Invoke-Adb shell chmod 644 $tmpPrefs | Out-Null
Invoke-Adb shell run-as com.droidnode cp $tmpPrefs shared_prefs/droidnode_console.xml | Out-Null
Invoke-Adb shell rm -f $tmpPrefs | Out-Null

Write-Host "== start app =="
Invoke-Adb shell am start -W -n com.droidnode/.ui.v2.NodeActivity | Out-Null
Start-Sleep -Seconds 12
$pidText = (Invoke-Adb shell pidof com.droidnode) -join " "
if (-not $pidText.Trim()) { Write-Error "app did not start" }
$enc = (Invoke-Adb shell "run-as com.droidnode test -f files/node/config/kubeconfig.enc && echo yes") -join " "
Write-Host "restored: node=$NodeName port=$Port kubeconfig.enc=$($enc -match 'yes')"
