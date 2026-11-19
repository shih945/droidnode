# 清理「节点已失联」节点上卡在 Terminating 的 Pod。
#
# 只删同时满足三个条件的对象：
#   1. 已在删除（有 deletionTimestamp）
#   2. 删除开始超过 N 分钟（默认 10）
#   3. 所属 Node 不存在，或该 Node 的 Ready 心跳也超过 N 分钟
#
# 默认干跑（只打印计划），确认后加 -Apply 才真正删除。
#
# 用法：
#   pwsh -File scripts/reap-stale-pods.ps1                 # 干跑
#   pwsh -File scripts/reap-stale-pods.ps1 -Apply          # 执行
#   pwsh -File scripts/reap-stale-pods.ps1 -Apply -Minutes 30
#
# 需要能 list nodes / delete pods 的 kubeconfig（控制面 admin）。
# 节点 SA（droidnode-agent）没有 nodes list，会用不了——脚本会直接报错退出。

param(
    [switch]$Apply,
    [int]$Minutes = 10
)

$ErrorActionPreference = 'Stop'

# ── 1. kubectl 能用吗？拿不到数据就退出，避免管道空转后静默什么都不做 ──────────
$ctx = kubectl config current-context
if ($LASTEXITCODE -ne 0 -or -not $ctx) {
    throw 'kubectl 不可用或没有当前 context'
}

$podsJson = kubectl get pods -A -o json
if ($LASTEXITCODE -ne 0 -or -not $podsJson) {
    throw "kubectl get pods 失败（context=$ctx）"
}

# 节点列表拿不到同样退出：否则 $nodes 为空，所有 Pod 都会被当成"节点不存在"
$nodesJson = kubectl get nodes -o json
if ($LASTEXITCODE -ne 0 -or -not $nodesJson) {
    throw 'kubectl get nodes 失败：无法判断节点是否失联，拒绝执行（需要能 list nodes 的 kubeconfig）'
}

# ── 2. 时间比较 ────────────────────────────────────────────────────────────────
$now = (Get-Date).ToUniversalTime()
$cutoff = $now.AddMinutes(-$Minutes)

# Kubernetes 的 RFC3339 时间带 Z、可能有小数秒；显式用 InvariantCulture 解析，
# 避免受系统区域设置影响。
function Convert-ToUtc([string]$s) {
    [datetime]::Parse(
        ($s -replace '\.\d+', ''),
        [Globalization.CultureInfo]::InvariantCulture,
        [Globalization.DateTimeStyles]::AdjustToUniversal -bor [Globalization.DateTimeStyles]::AssumeUniversal)
}

$nodes = @{}
foreach ($n in ($nodesJson | ConvertFrom-Json).items) {
    $ready = $n.status.conditions | Where-Object { $_.type -eq 'Ready' } | Select-Object -First 1
    $nodes[$n.metadata.name] = if ($ready) { $ready.lastHeartbeatTime } else { $null }
}

# ── 3. 逐条判断 ────────────────────────────────────────────────────────────────
$found = 0
foreach ($p in ($podsJson | ConvertFrom-Json).items) {
    $ts = $p.metadata.deletionTimestamp
    if (-not $ts) { continue }
    if ((Convert-ToUtc $ts) -gt $cutoff) { continue }

    $nodeName = $p.spec.nodeName
    $hb = $nodes[$nodeName]
    if ($hb -and (Convert-ToUtc $hb) -gt $cutoff) { continue }

    $found++
    $age = ($now - (Convert-ToUtc $ts)).TotalMinutes
    $desc = '{0}/{1}  node={2}  删除已 {3:N0} 分钟  node心跳={4}' -f `
        $p.metadata.namespace, $p.metadata.name, $nodeName, $age,
        $(if ($hb) { $hb } else { '节点不存在' })

    if ($Apply) {
        Write-Host ('delete       ' + $desc)
        kubectl delete pod -n $p.metadata.namespace $p.metadata.name --force --grace-period=0
    } else {
        Write-Host ('would delete ' + $desc)
    }
}

if ($found -eq 0) {
    Write-Host ('没有符合条件的残留 Pod（context={0}）' -f $ctx)
} elseif (-not $Apply) {
    Write-Host ''
    Write-Host ('共 {0} 个待清理对象；确认无误后加 -Apply 执行。' -f $found)
}
