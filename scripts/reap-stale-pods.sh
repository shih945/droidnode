#!/usr/bin/env bash
# 清理「节点已失联」节点上卡在 Terminating 的 Pod。
#
# 只删同时满足三个条件的对象：
#   1. 已在删除（有 deletionTimestamp）
#   2. 删除开始超过 N 分钟（默认 10）
#   3. 所属 Node 不存在，或该 Node 的 Ready 心跳也超过 N 分钟
#
# 默认干跑，确认后加 --apply 才真正删除。
#
# 用法：
#   scripts/reap-stale-pods.sh                 # 干跑
#   scripts/reap-stale-pods.sh --apply         # 执行
#   scripts/reap-stale-pods.sh --apply --minutes 30
#
# 需要能 list nodes / delete pods 的 kubeconfig（控制面 admin）和 python3。
# 节点 SA（droidnode-agent）没有 nodes list，会用不了——脚本会直接报错退出。

set -eu

APPLY=0
MINUTES=10
while [ $# -gt 0 ]; do
  case "$1" in
    --apply) APPLY=1 ;;
    --minutes)
      shift
      MINUTES="${1:?--minutes 需要一个数字}"
      ;;
    -h|--help)
      sed -n '2,18p' "$0"
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
  shift
done

exec python3 - "$APPLY" "$MINUTES" <<'PY'
import datetime
import json
import subprocess
import sys

apply_ = sys.argv[1] == "1"
minutes = int(sys.argv[2])


def run(*args):
    return subprocess.run(["kubectl", *args], capture_output=True, text=True)


def kjson(*args):
    r = run(*args, "-o", "json")
    if r.returncode != 0:
        sys.exit("kubectl " + " ".join(args) + " 失败：" + r.stderr.strip())
    return json.loads(r.stdout)


ctx = run("config", "current-context")
if ctx.returncode != 0 or not ctx.stdout.strip():
    sys.exit("kubectl 不可用或没有当前 context")
ctx = ctx.stdout.strip()

pods = kjson("get", "pods", "-A")
# 节点列表拿不到就退出：否则所有 Pod 都会被当成"节点不存在"
nodes = kjson("get", "nodes")


def ts(text):
    return datetime.datetime.fromisoformat(text.replace("Z", "+00:00"))


now = datetime.datetime.now(datetime.timezone.utc)
cutoff = now - datetime.timedelta(minutes=minutes)

heartbeat = {}
for node in nodes.get("items", []):
    for cond in (node.get("status") or {}).get("conditions") or []:
        if cond.get("type") == "Ready":
            heartbeat[node["metadata"]["name"]] = cond.get("lastHeartbeatTime")
            break

found = 0
for pod in pods.get("items", []):
    deleted = (pod.get("metadata") or {}).get("deletionTimestamp")
    if not deleted:
        continue
    if ts(deleted) > cutoff:
        continue

    node = (pod.get("spec") or {}).get("nodeName") or ""
    beat = heartbeat.get(node)
    if beat and ts(beat) > cutoff:
        continue

    found += 1
    ns = pod["metadata"]["namespace"]
    name = pod["metadata"]["name"]
    age = int((now - ts(deleted)).total_seconds() // 60)
    prefix = "delete      " if apply_ else "would delete"
    print(f"{prefix} {ns}/{name}  node={node or '?'}  删除已 {age} 分钟")

    if apply_:
        r = run("delete", "pod", "-n", ns, name, "--force", "--grace-period=0")
        if r.returncode != 0:
            print("  删除失败：" + r.stderr.strip(), file=sys.stderr)

if found == 0:
    print(f"没有符合条件的残留 Pod（context={ctx}）")
elif not apply_:
    print(f"\n共 {found} 个待清理对象；确认无误后加 --apply 执行。")
PY
