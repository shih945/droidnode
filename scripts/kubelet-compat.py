#!/usr/bin/env python3
"""Compare the DroidNode agent against the kubelet v1.29 HTTP contract.

    python scripts/kubelet-compat.py capture-adb --serial <adb-serial> --out build-cache/kubelet-compat/target
    python scripts/kubelet-compat.py capture-kubectl --kubeconfig K --node k3s-main --out REF
    python scripts/kubelet-compat.py check --target build-cache/kubelet-compat/target
    python scripts/kubelet-compat.py compare --reference REF --target TARGET

`check` reports, per endpoint:
  * missing required fields / type mismatches  (kubelet incompatibility)
  * fields and endpoints that exist only for the APK UI (whitelisted in the
    contract's `extensions` map, listed separately)
  * unexpected extra leaf fields

Exit code is 0 for an audit run; pass --fail-on-missing to make missing
required fields fail (useful once the gaps are fixed).
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import socket
import ssl
import subprocess
import sys
import urllib.request
from pathlib import Path
from typing import Any, Iterable

ROOT = Path(__file__).resolve().parent.parent
CONTRACT_PATH = ROOT / "docs" / "api" / "kubelet-contract-v1.29.json"
CONTRACT_DIR = ROOT / "docs" / "api" / "contracts"

# endpoint -> capture file name
FILES = {
    "/healthz": "healthz.txt",
    "/pods": "pods.json",
    "/images": "images.json",
    "/stats/summary": "stats_summary.json",
    "/metrics": "metrics.txt",
    "/metrics/resource": "metrics_resource.txt",
}
JSON_ENDPOINTS = ["/pods", "/images", "/stats/summary"]


def contract_path(version: str) -> Path:
    """Resolve the contract for a kubelet minor, falling back to the legacy
    top-level file for 1.29."""
    candidates = [
        CONTRACT_DIR / f"kubelet-v{version}.json",
        ROOT / "docs" / "api" / f"kubelet-contract-v{version}.json",
    ]
    for candidate in candidates:
        if candidate.exists():
            return candidate
    raise SystemExit(
        f"no contract for kubelet {version}; expected one of: "
        + ", ".join(str(c.relative_to(ROOT)) for c in candidates)
    )


def load_contract(version: str = "1.29") -> dict:
    return json.loads(contract_path(version).read_text(encoding="utf-8"))


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def adb(serial: str, *args: str) -> subprocess.CompletedProcess:
    exe = os.environ.get("ADB", "adb")
    return subprocess.run([exe, "-s", serial, *args], capture_output=True, text=True)


def capture_adb(serial: str, out: Path) -> None:
    out.mkdir(parents=True, exist_ok=True)
    port = free_port()
    forwarded = False
    try:
        result = adb(serial, "forward", f"tcp:{port}", "tcp:10255")
        if result.returncode != 0:
            raise SystemExit(f"adb forward failed: {result.stderr.strip()}")
        forwarded = True
        ctx = ssl._create_unverified_context()
        meta = {}
        for endpoint, name in FILES.items():
            url = f"https://127.0.0.1:{port}{endpoint}"
            try:
                with urllib.request.urlopen(url, context=ctx, timeout=10) as response:
                    body = response.read()
                    meta[endpoint] = {
                        "status": response.status,
                        "content_type": response.headers.get("Content-Type", ""),
                    }
            except urllib.error.HTTPError as exc:
                body = exc.read()
                meta[endpoint] = {
                    "status": exc.code,
                    "content_type": exc.headers.get("Content-Type", "") if exc.headers else "",
                }
            (out / name).write_bytes(body)
            print(f"{endpoint} -> {name} ({len(body)} bytes)")
        (out / "meta.json").write_text(json.dumps(meta, indent=2, ensure_ascii=False), encoding="utf-8")
    finally:
        if forwarded:
            adb(serial, "forward", "--remove", f"tcp:{port}")


def capture_kubectl(kubeconfig: str, node: str, out: Path) -> None:
    out.mkdir(parents=True, exist_ok=True)
    exe = os.environ.get("KUBECTL") or shutil.which("kubectl") or "kubectl"
    for endpoint, name in FILES.items():
        url = f"/api/v1/nodes/{node}/proxy{endpoint}"
        result = subprocess.run(
            [exe, "--kubeconfig", kubeconfig, "get", "--raw", url],
            capture_output=True,
        )
        if result.returncode != 0:
            print(f"{endpoint}: FAILED ({result.stderr.decode('utf-8', 'replace').strip()})")
            continue
        (out / name).write_bytes(result.stdout)
        print(f"{endpoint} -> {name} ({len(result.stdout)} bytes)")


def split_path(path: str) -> list[str]:
    return path.split(".")


def values_at(data: Any, path: str) -> list[Any]:
    """All values matching a contract path such as items[].status.phase."""
    current = [data]
    for raw in split_path(path):
        is_list = raw.endswith("[]")
        key = raw[:-2] if is_list else raw
        nxt: list[Any] = []
        for item in current:
            if not isinstance(item, dict) or key not in item:
                continue
            value = item[key]
            if is_list:
                if isinstance(value, list):
                    nxt.extend(value)
            else:
                nxt.append(value)
        current = nxt
        if not current:
            return []
    return current


def json_type(value: Any) -> str:
    if isinstance(value, bool):
        return "boolean"
    if isinstance(value, int):
        return "integer"
    if isinstance(value, float):
        return "number"
    if isinstance(value, str):
        return "string"
    if isinstance(value, list):
        return "array"
    if isinstance(value, dict):
        return "object"
    return "null"


def leaf_paths(value: Any, prefix: str = "") -> Iterable[str]:
    if isinstance(value, dict):
        for key, child in value.items():
            child_prefix = f"{prefix}.{key}" if prefix else key
            yield from leaf_paths(child, child_prefix)
    elif isinstance(value, list):
        list_prefix = f"{prefix}[]"
        if not value:
            yield list_prefix
        for item in value:
            yield from leaf_paths(item, list_prefix)
    else:
        yield prefix


def allowed(path: str, allowed_paths: set[str]) -> bool:
    if path in allowed_paths:
        return True
    return any(path.startswith(candidate + ".") for candidate in allowed_paths)


def check_endpoint(endpoint: str, data: Any, contract: dict) -> dict:
    required = contract["required"].get(endpoint, {})
    allowed_paths = set(required) | set(contract["allowed_extra"].get(endpoint, []))
    allowed_paths |= set(contract["extensions"].get(endpoint, {}))

    missing: list[tuple[str, str, int, int]] = []
    type_mismatch: list[tuple[str, str, str]] = []
    for path, expected_type in required.items():
        found = values_at(data, path)
        if not found:
            missing.append((path, expected_type, 0, 0))
            continue
        bad = [json_type(value) for value in found if json_type(value) != expected_type]
        if bad:
            type_mismatch.append((path, expected_type, ", ".join(sorted(set(bad)))))

    extras = []
    if endpoint in contract.get("strict_extra", []):
        for path in sorted(set(leaf_paths(data))):
            if not allowed(path, allowed_paths):
                extras.append(path)

    extensions = []
    for path in sorted(set(contract["extensions"].get(endpoint, {}))):
        if values_at(data, path):
            extensions.append((path, contract["extensions"][endpoint][path]))

    return {
        "missing": missing,
        "type_mismatch": type_mismatch,
        "extras": extras,
        "extensions": extensions,
    }


def render_report(
    contract: dict, target: Path, contract_file: Path | None = None
) -> str:
    version = contract.get("kubelet_version", "v1.29")
    lines = [
        f"# Kubelet {version} API compatibility report",
        "",
        f"- target: `{target}`",
        f"- contract: `{(contract_file or CONTRACT_PATH).relative_to(ROOT)}`",
        "",
        "字段缺失/类型不符 = 与 kubelet 不兼容；`extensions` 是 APK 需要、kubelet 没有的字段/端点，单独列出，不算不兼容。",
        "",
    ]

    endpoint_rows = []
    for path, info in contract["endpoints"].items():
        implemented = info.get("implemented", True)
        custom = "extension" in info
        file = FILES.get(path)
        captured = file is not None and (target / file).exists()
        if custom:
            state = "APK 扩展"
        elif not implemented:
            state = "未实现（kubelet 有）"
        elif captured:
            state = "已捕获"
        elif file:
            state = "未捕获"
        else:
            state = "仅协议（kubectl 验证）"
        endpoint_rows.append((path, ", ".join(m.upper() for m in info["methods"]), state))
    lines += ["## 端点覆盖", "", "| 端点 | 方法 | 状态 |", "|---|---|---|"]
    lines += [f"| `{p}` | {m} | {s} |" for p, m, s in endpoint_rows]

    missing_total = 0
    mismatch_total = 0
    extension_total = 0
    extra_total = 0
    sections: list[str] = []

    for endpoint in JSON_ENDPOINTS:
        file = target / FILES[endpoint]
        if not file.exists():
            sections.append(f"## `{endpoint}`\n\n未捕获（缺 `{file.name}`）。\n")
            continue
        try:
            data = json.loads(file.read_text(encoding="utf-8"))
        except json.JSONDecodeError as exc:
            sections.append(f"## `{endpoint}`\n\nJSON 解析失败：{exc}\n")
            continue
        result = check_endpoint(endpoint, data, contract)
        missing_total += len(result["missing"])
        mismatch_total += len(result["type_mismatch"])
        extension_total += len(result["extensions"])
        extra_total += len(result["extras"])

        block = [f"## `{endpoint}`", ""]
        if result["missing"]:
            block += ["### 缺少的 kubelet 字段（不兼容）", "", "| 字段 | 期望类型 | 覆盖 |", "|---|---|---|"]
            for path, expected_type, got, want in result["missing"]:
                cover = f"{got}/{want}" if want else "0"
                block.append(f"| `{path}` | {expected_type} | {cover} |")
            block.append("")
        if result["type_mismatch"]:
            block += ["### 类型不符（不兼容）", "", "| 字段 | 期望 | 实际 |", "|---|---|---|"]
            for path, expected_type, actual in result["type_mismatch"]:
                block.append(f"| `{path}` | {expected_type} | {actual} |")
            block.append("")
        if result["extensions"]:
            block += ["### APK 扩展字段（已标注，不算不兼容）", "", "| 字段 | 用途 |", "|---|---|"]
            for path, why in result["extensions"]:
                block.append(f"| `{path}` | {why} |")
            block.append("")
        if result["extras"]:
            block += ["### 未识别的额外字段（需要确认）", ""]
            block += [f"- `{path}`" for path in result["extras"]]
            block.append("")
        if len(block) == 2:
            block.append("必填字段齐全。")
            block.append("")
        sections.append("\n".join(block))

    metrics = target / FILES["/metrics"]
    if metrics.exists():
        names = sorted(
            {
                line.split("{")[0].split(" ")[0]
                for line in metrics.read_text(encoding="utf-8", errors="replace").splitlines()
                if line and not line.startswith("#")
            }
        )
        custom = [name for name in names if not name.startswith(("kubelet_", "container_", "process_"))]
        sections.append(
            "## `/metrics`\n\n"
            f"共 {len(names)} 个指标名，其中 {len(custom)} 个不是 kubelet 指标（按 APK 扩展处理）：\n\n"
            + ("\n".join(f"- `{name}`" for name in custom) if custom else "- 全部是 kubelet 风格指标")
            + "\n"
        )

    lines += [
        "## 汇总",
        "",
        f"- 缺少的 kubelet 必填字段：**{missing_total}**",
        f"- 类型不符：**{mismatch_total}**",
        f"- APK 扩展字段：**{extension_total}**",
        f"- 未识别额外字段：**{extra_total}**",
        "",
        "## 明细",
        "",
    ]
    return "\n".join(lines + sections)


def cmd_check(args: argparse.Namespace) -> int:
    version = getattr(args, "k8s_version", "1.29")
    contract_file = contract_path(version)
    contract = load_contract(version)
    target = Path(args.target)
    if not target.exists():
        raise SystemExit(f"target dir not found: {target}")
    report = render_report(contract, target, contract_file)
    if args.report:
        Path(args.report).write_text(report, encoding="utf-8")
        print(f"wrote {args.report}")
    else:
        print(report)
    if args.fail_on_missing:
        missing = sum(
            len(check_endpoint(endpoint, json.loads((target / FILES[endpoint]).read_text(encoding="utf-8")), contract)["missing"])
            for endpoint in JSON_ENDPOINTS
            if (target / FILES[endpoint]).exists()
        )
        if missing:
            print(f"{missing} missing required fields", file=sys.stderr)
            return 1
    return 0


def cmd_compare(args: argparse.Namespace) -> int:
    reference = Path(args.reference)
    target = Path(args.target)
    ok = True
    for endpoint in JSON_ENDPOINTS:
        name = FILES[endpoint]
        ref_file, tgt_file = reference / name, target / name
        if not ref_file.exists() or not tgt_file.exists():
            print(f"{endpoint}: skipped (missing file)")
            continue
        ref = json.loads(ref_file.read_text(encoding="utf-8"))
        tgt = json.loads(tgt_file.read_text(encoding="utf-8"))
        ref_paths = set(leaf_paths(ref))
        tgt_paths = set(leaf_paths(tgt))
        missing = sorted(ref_paths - tgt_paths)
        extra = sorted(tgt_paths - ref_paths)
        print(f"\n{endpoint}: reference-only={len(missing)} target-only={len(extra)}")
        for path in missing[: args.limit]:
            print(f"  - {path}")
        if len(missing) > args.limit:
            print(f"  ... {len(missing) - args.limit} more")
        for path in extra[: args.limit]:
            print(f"  + {path}")
        if len(extra) > args.limit:
            print(f"  ... {len(extra) - args.limit} more")
        if missing:
            ok = False
    return 0 if ok else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    adb_parser = sub.add_parser("capture-adb", help="capture via adb forward + HTTPS")
    adb_parser.add_argument("--serial", required=True)
    adb_parser.add_argument("--out", required=True)

    kubectl_parser = sub.add_parser("capture-kubectl", help="capture via apiserver proxy")
    kubectl_parser.add_argument("--kubeconfig", required=True)
    kubectl_parser.add_argument("--node", required=True)
    kubectl_parser.add_argument("--out", required=True)

    check_parser = sub.add_parser("check", help="check a captured target against the contract")
    check_parser.add_argument("--target", required=True)
    check_parser.add_argument("--report")
    check_parser.add_argument("--fail-on-missing", action="store_true")
    check_parser.add_argument(
        "--k8s-version",
        default="1.29",
        help="kubelet minor version whose contract to use (default 1.29)",
    )

    compare_parser = sub.add_parser("compare", help="structural diff between reference and target captures")
    compare_parser.add_argument("--reference", required=True)
    compare_parser.add_argument("--target", required=True)
    compare_parser.add_argument("--limit", type=int, default=40)

    args = parser.parse_args()
    if args.command == "capture-adb":
        capture_adb(args.serial, Path(args.out))
        return 0
    if args.command == "capture-kubectl":
        capture_kubectl(args.kubeconfig, args.node, Path(args.out))
        return 0
    if args.command == "check":
        return cmd_check(args)
    return cmd_compare(args)


if __name__ == "__main__":
    raise SystemExit(main())
