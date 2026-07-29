#!/usr/bin/env python3
"""Compare generated app-server schemas while ignoring JSON object key order."""

from __future__ import annotations

import argparse
import json
from pathlib import Path


def json_files(root: Path, *, exclude_baseline: bool) -> set[Path]:
    return {
        path.relative_to(root)
        for path in root.rglob("*.json")
        if not exclude_baseline or path.name != "baseline.json"
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("expected", type=Path)
    parser.add_argument("actual", type=Path)
    args = parser.parse_args()

    expected = json_files(args.expected, exclude_baseline=True)
    actual = json_files(args.actual, exclude_baseline=False)
    missing = sorted(expected - actual)
    extra = sorted(actual - expected)
    mismatched = [
        path
        for path in sorted(expected & actual)
        if json.loads((args.expected / path).read_text())
        != json.loads((args.actual / path).read_text())
    ]
    if missing or extra or mismatched:
        for label, paths in [
            ("missing", missing),
            ("extra", extra),
            ("semantic mismatch", mismatched),
        ]:
            for path in paths[:20]:
                print(f"{label}: {path}")
        raise SystemExit(1)
    print(f"verified {len(expected)} schema files")


if __name__ == "__main__":
    main()
