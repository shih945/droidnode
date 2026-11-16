#!/usr/bin/env python3
"""Export the agent HTTP API docs from docs/api/routes.json.

    python scripts/export-api-docs.py            # write agent-http.md + agent-openapi.json
    python scripts/export-api-docs.py --check    # verify router coverage + docs freshness

`routes.json` is the single source of truth. The script also parses
`virtual_kubelet_exposer.rs` and fails when a route exists in code but not in the
metadata (or the generated files are stale), so the API reference cannot silently
drift from the router.
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SPEC = ROOT / "docs" / "api" / "routes.json"
MD = ROOT / "docs" / "api" / "agent-http.md"
OPENAPI = ROOT / "docs" / "api" / "agent-openapi.json"
ROUTER = ROOT / "agent" / "executor" / "src" / "exposers" / "virtual_kubelet_exposer.rs"

METHODS = ("get", "post", "put", "delete", "patch")
TYPE_MAP = {"boolean": "boolean", "integer": "integer", "number": "number", "string": "string"}


def load() -> dict:
    return json.loads(SPEC.read_text(encoding="utf-8"))


def router_routes() -> set[tuple[str, str]]:
    text = ROUTER.read_text(encoding="utf-8")
    start = text.index("fn build_router")
    block = text[start:]
    found: set[tuple[str, str]] = set()
    matches = list(re.finditer(r'\.route\(\s*"([^"]+)"', block))
    for i, match in enumerate(matches):
        path = re.sub(r":([A-Za-z_][A-Za-z0-9_]*)", r"{\1}", match.group(1))
        end = matches[i + 1].start() if i + 1 < len(matches) else len(block)
        tail = block[match.end():end]
        methods = {m for m in METHODS if re.search(rf"\b{m}\s*\(", tail)}
        if not methods:
            methods = {"get"}
        for method in methods:
            found.add((method, path))
    return found


def openapi_parameters(parameters: list[dict]) -> list[dict]:
    out = []
    for p in parameters:
        item = {
            "name": p["name"],
            "in": p["in"],
            "required": bool(p.get("required", p["in"] == "path")),
            "schema": {"type": TYPE_MAP.get(p.get("type", "string"), "string")},
        }
        if "description" in p:
            item["description"] = p["description"]
        if "example" in p:
            item["example"] = p["example"]
        out.append(item)
    return out


def openapi_content(media_type: str, example) -> dict:
    if media_type == "application/json":
        schema = {"type": "array"} if isinstance(example, list) else {"type": "object"}
    else:
        schema = {"type": "string"}
    return {media_type: {"schema": schema, "example": example}}


def build_openapi(spec: dict) -> dict:
    paths = {}
    for path, methods in spec["paths"].items():
        item = {}
        for method, op in methods.items():
            operation = {
                "summary": op["summary"],
                "description": op.get("description", ""),
                "tags": [op.get("transport", "http")],
                "security": [] if op.get("auth") == "none" else [{"ClientCertificate": []}],
                "x-droidnode-auth": op.get("auth", "none"),
                "x-droidnode-transport": op.get("transport", "http"),
                "responses": {},
            }
            if op.get("parameters"):
                operation["parameters"] = openapi_parameters(op["parameters"])
            for code, response in op["responses"].items():
                entry = {"description": response.get("description", "")}
                media_type = response.get("mediaType")
                if media_type and code != "101":
                    entry["content"] = openapi_content(media_type, response.get("example", ""))
                operation["responses"][code] = entry
            item[method] = operation
        paths[path] = item

    return {
        "openapi": "3.1.0",
        "info": {
            "title": spec["info"]["title"],
            "version": spec["info"]["version"],
            "description": spec["info"]["description"],
        },
        "servers": [{"url": spec["info"]["base"]}],
        "components": {
            "securitySchemes": {
                "ClientCertificate": {
                    "type": "mutualTLS",
                    "description": spec["security"]["description"],
                }
            }
        },
        "paths": paths,
    }


def curl_example(path: str, method: str, op: dict, base: str) -> str:
    example_path = path
    for p in op.get("parameters", []):
        if p["in"] == "path":
            example_path = example_path.replace("{" + p["name"] + "}", f"<{p['name']}>")
    url = base.rstrip("/") + example_path
    if op.get("transport") == "spdy":
        return (
            f"# {method.upper()} {path}（SPDY/3.1，kubectl 内部调用）\n"
            f"# 需要客户端证书，Upgrade: SPDY/3.1"
        )
    query = [p for p in op.get("parameters", []) if p["in"] == "query"]
    if query:
        pairs = "&".join(f"{p['name']}=<{p['name']}>" for p in query)
        url += "?" + pairs
    flag = "" if method == "get" else f"-X {method.upper()} "
    insecure = "-k " if url.startswith("https://") else ""
    return f"curl {insecure}{flag}\"{url}\""


def build_markdown(spec: dict) -> str:
    info = spec["info"]
    lines = [
        f"# {info['title']}",
        "",
        f"> 版本：`{info['version']}`　基础地址：`{info['base']}`",
        "",
        info["description"],
        "",
        "## 认证",
        "",
        spec["security"]["description"],
        "",
        "## 端点一览",
        "",
        "| 方法 | 路径 | 认证 | 传输 | 说明 |",
        "|---|---|---|---|---|",
    ]
    for path, methods in spec["paths"].items():
        for method, op in methods.items():
            lines.append(
                f"| `{method.upper()}` | `{path}` | {op.get('auth', 'none')} | "
                f"{op.get('transport', 'http')} | {op['summary']} |"
            )

    lines += ["", "## 端点详情", ""]
    for path, methods in spec["paths"].items():
        for method, op in methods.items():
            lines += [
                f"### `{method.upper()} {path}`",
                "",
                op["summary"],
                "",
                op.get("description", ""),
                "",
                f"- 认证：`{op.get('auth', 'none')}`",
                f"- 传输：`{op.get('transport', 'http')}`",
                "",
            ]
            params = op.get("parameters", [])
            if params:
                lines += ["| 参数 | 位置 | 必填 | 类型 | 说明 |", "|---|---|---|---|---|"]
                for p in params:
                    desc = p.get("description", "")
                    if "example" in p:
                        desc += f"（示例：`{p['example']}`）"
                    lines.append(
                        f"| `{p['name']}` | {p['in']} | {'是' if p.get('required') else '否'} | "
                        f"{p.get('type', 'string')} | {desc} |"
                    )
                lines.append("")
            lines += ["| 状态码 | 说明 | 响应 |", "|---|---|---|"]
            for code, response in op["responses"].items():
                media = response.get("mediaType", "—")
                example = response.get("example")
                if isinstance(example, (dict, list)):
                    example = json.dumps(example, ensure_ascii=False)
                sample = f"`{example}`" if example is not None else "—"
                lines.append(f"| `{code}` | {response['description']} | {media} {sample} |")
            lines += ["", "```bash", curl_example(path, method, op, info["base"]), "```", ""]

    ipc = spec.get("ipc")
    if ipc:
        lines += [
            f"## {ipc['title']}",
            "",
            ipc["description"],
            "",
            f"- 路径：`{ipc['path']}`",
            "",
            "### 请求（app → agent，一行 JSON）",
            "",
            "| 字段 | 类型 | 说明 |",
            "|---|---|---|",
        ]
        for field in ipc["request_fields"]:
            lines.append(f"| `{field['name']}` | {field['type']} | {field.get('description', '')} |")
        lines += ["", "### 响应（agent → app，一行 JSON）", "", "| 字段 | 类型 | 说明 |", "|---|---|---|"]
        for field in ipc["response_fields"]:
            lines.append(f"| `{field['name']}` | {field['type']} | {field.get('description', '')} |")
        lines.append("")

    lines += [
        "---",
        "",
        "本文档由 `scripts/export-api-docs.py` 从 `docs/api/routes.json` 生成；",
        "改接口时先改 `routes.json`，再运行脚本。`--check` 会比对路由与代码。",
        "",
    ]
    return "\n".join(lines)


def main() -> int:
    spec = load()
    check = "--check" in sys.argv

    expected_routes = {
        (method, path) for path, methods in spec["paths"].items() for method in methods
    }
    actual_routes = router_routes()
    missing = sorted(actual_routes - expected_routes)
    extra = sorted(expected_routes - actual_routes)
    ok = True
    if missing:
        ok = False
        print("路由存在但 routes.json 没有：", file=sys.stderr)
        for method, path in missing:
            print(f"  {method.upper()} {path}", file=sys.stderr)
    if extra:
        ok = False
        print("routes.json 有但代码里没有：", file=sys.stderr)
        for method, path in extra:
            print(f"  {method.upper()} {path}", file=sys.stderr)

    markdown = build_markdown(spec)
    openapi = json.dumps(build_openapi(spec), ensure_ascii=False, indent=2) + "\n"

    if check:
        if MD.exists() and MD.read_text(encoding="utf-8") != markdown:
            ok = False
            print(f"{MD.relative_to(ROOT)} 已过期，请重新生成", file=sys.stderr)
        if OPENAPI.exists() and OPENAPI.read_text(encoding="utf-8") != openapi:
            ok = False
            print(f"{OPENAPI.relative_to(ROOT)} 已过期，请重新生成", file=sys.stderr)
        if ok:
            print("API docs are in sync")
        return 0 if ok else 1

    MD.write_text(markdown, encoding="utf-8")
    OPENAPI.write_text(openapi, encoding="utf-8")
    print(f"wrote {MD.relative_to(ROOT)} and {OPENAPI.relative_to(ROOT)}")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
