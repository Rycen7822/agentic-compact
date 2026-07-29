#!/usr/bin/env python3
"""Create deterministic provenance for already-built release artifacts."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SUPPORTED_VERSIONS = ("0.146.0", "0.145.0")
DEFAULT_BASELINES = [
    ROOT / f"tests/fixtures/app-server/codex-cli-{version}/baseline.json"
    for version in SUPPORTED_VERSIONS
]
DEFAULT_LAUNCHERS = [
    ROOT / f"tests/fixtures/launcher/codex-cli-{version}/launcher-args-v1.json"
    for version in SUPPORTED_VERSIONS
]
DEFAULT_PLUGIN_CLIS = [
    ROOT / f"tests/fixtures/codex-cli/codex-cli-{version}/plugin-cli.json"
    for version in SUPPORTED_VERSIONS
]


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_json(path: Path) -> dict:
    value = json.loads(path.read_text())
    if not isinstance(value, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return value


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("artifacts", nargs="+", type=Path)
    parser.add_argument("--output", type=Path, default=Path("release-metadata.json"))
    parser.add_argument("--project-source-commit", required=True)
    parser.add_argument("--rustc-version", required=True)
    parser.add_argument("--test-summary", required=True)
    parser.add_argument("--baseline", action="append", type=Path)
    parser.add_argument("--launcher-policy", action="append", type=Path)
    parser.add_argument("--plugin-cli-policy", action="append", type=Path)
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9a-f]{40}", args.project_source_commit):
        parser.error("--project-source-commit must be a 40-character lowercase Git SHA")
    baselines = args.baseline or DEFAULT_BASELINES
    launchers = args.launcher_policy or DEFAULT_LAUNCHERS
    plugin_clis = args.plugin_cli_policy or DEFAULT_PLUGIN_CLIS
    if len({len(baselines), len(launchers), len(plugin_clis)}) != 1:
        parser.error("baseline, launcher-policy and plugin-cli-policy counts must match")

    cargo = tomllib.loads((ROOT / "Cargo.toml").read_text())
    entries = []
    for path in sorted(args.artifacts, key=lambda item: item.name):
        entries.append({"file": path.name, "bytes": path.stat().st_size, "sha256": sha256(path)})
    contracts = []
    for baseline_path, launcher_path, plugin_cli_path in zip(
        baselines, launchers, plugin_clis, strict=True
    ):
        baseline = load_json(baseline_path)
        contracts.append(
            {
                "version": baseline["codexVersion"],
                "userAgent": baseline["userAgent"],
                "nativeBinarySha256": baseline["nativeBinarySha256"],
                "generatedSchemaSha256": baseline["generatedSchemaBundleSha256"],
                "generatedSchemaFileCount": baseline["generatedSchemaFileCount"],
                "sourceCommit": baseline["sourceCommit"],
                "sourceTag": baseline["sourceTag"],
                "launcherPolicySha256": sha256(launcher_path),
                "pluginCliPolicySha256": sha256(plugin_cli_path),
            }
        )
    provenance = {
        "schemaVersion": 2,
        "project": {
            "name": cargo["package"]["name"],
            "version": cargo["package"]["version"],
            "sourceCommit": args.project_source_commit,
        },
        "build": {"rustcVersion": args.rustc_version},
        "codexContracts": contracts,
        "tests": {"summary": args.test_summary},
        "artifacts": entries,
    }
    args.output.write_text(json.dumps(provenance, indent=2) + "\n")


if __name__ == "__main__":
    main()
