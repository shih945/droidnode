#!/usr/bin/env python3
"""Count `unwrap()` / `expect(` outside `#[cfg(test)]` modules.

    python scripts/count-unwraps.py            # per-file table + total
    python scripts/count-unwraps.py --max 40   # exit 1 when above the budget

The test-module stripper is brace-based, so it also removes nested modules
inside `#[cfg(test)]`. It is a budgeting tool, not a Rust parser.
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = [ROOT / "agent" / "executor" / "src", ROOT / "agent" / "node-agent" / "src"]
PATTERN = re.compile(r"\.unwrap\(\)|\.expect\(")


def strip_test_modules(text: str) -> str:
    out: list[str] = []
    i = 0
    while True:
        marker = text.find("#[cfg(test)]", i)
        if marker == -1:
            out.append(text[i:])
            break
        out.append(text[i:marker])
        # Find the opening brace of the following item and match it.
        brace = text.find("{", marker)
        if brace == -1:
            i = marker + len("#[cfg(test)]")
            continue
        depth = 0
        j = brace
        while j < len(text):
            ch = text[j]
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    j += 1
                    break
            j += 1
        i = j
    return "".join(out)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--max", type=int, default=None, help="fail above this total")
    args = parser.parse_args()

    total = 0
    rows: list[tuple[int, Path]] = []
    for root in SRC:
        for path in sorted(root.rglob("*.rs")):
            text = strip_test_modules(path.read_text(encoding="utf-8"))
            count = len(PATTERN.findall(text))
            if count:
                rows.append((count, path))
                total += count

    for count, path in sorted(rows, reverse=True):
        print(f"{count:4d}  {path.relative_to(ROOT)}")
    print(f"\ntotal (non-test): {total}")

    if args.max is not None and total > args.max:
        print(f"over budget: {total} > {args.max}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
