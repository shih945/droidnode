param(
    [string]$Apk = "",
    [switch]$DryRun
)

# List every adb device and install one APK on all of them.
#
#   scripts\adb-install-all.ps1                          # latest dist APK
#   scripts\adb-install-all.ps1 -Apk .\dist\droidnode-debug-xxx.apk
#   scripts\adb-install-all.ps1 -DryRun                  # only print commands
#
# `-r` keeps app data, `-d` allows a lower version code. Devices that are
# offline/unauthorized are reported and skipped; any real install failure makes
# the script exit 1.
$ErrorActionPreference = "Continue"
$adb = if ($env:ADB) { $env:ADB } else { "adb" }

if (-not $Apk) {
    $dist = Join-Path $PSScriptRoot "..\dist"
    $Apk = Get-ChildItem $dist -Filter "droidnode-debug-*.apk" -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1 -ExpandProperty FullName
}
if (-not $Apk -or -not (Test-Path $Apk)) {
    Write-Error "APK not found; pass -Apk <path> or build one into dist\"
    exit 2
}
$Apk = (Resolve-Path $Apk).Path
$apkSize = [math]::Round((Get-Item $Apk).Length / 1MB, 2)

Write-Host "== adb devices =="
& $adb devices -l

$rows = & $adb devices | Select-Object -Skip 1 | Where-Object { $_.Trim() }
$devices = @()
foreach ($row in $rows) {
    $parts = $row.Trim() -split "\s+"
    if ($parts.Count -ge 2) {
        $devices += [pscustomobject]@{ Serial = $parts[0]; State = $parts[1] }
    }
}
if ($devices.Count -eq 0) {
    Write-Error "no adb devices connected"
    exit 1
}

Write-Host ("== install {0} ({1} MB) on {2} device(s) ==" -f $Apk, $apkSize, $devices.Count)

$installed = 0
$failed = @()
foreach ($d in $devices) {
    if ($d.State -ne "device") {
        Write-Warning ("skip {0}: {1}" -f $d.Serial, $d.State)
        continue
    }
    if ($DryRun) {
        Write-Host ("dry-run: adb -s {0} install -r -d `"{1}`"" -f $d.Serial, $Apk)
        continue
    }

    Write-Host ("== {0} ==" -f $d.Serial)
    $output = & $adb -s $d.Serial install -r -d $Apk 2>&1
    $text = $output -join "`n"
    Write-Host $text.Trim()

    if ($text -match "Success") {
        $installed++
    } else {
        $failed += $d.Serial
    }
}

if ($DryRun) {
    Write-Host "dry-run complete"
    exit 0
}
if ($failed.Count -gt 0) {
    Write-Error ("install failed on: {0}" -f ($failed -join ", "))
    exit 1
}
Write-Host ("OK: installed on {0} device(s)" -f $installed)
exit 0
