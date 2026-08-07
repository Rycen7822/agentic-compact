#!/usr/bin/env python3
"""Verify the frozen v0.2 model surface against production, sham, and plugin files."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
FIXTURE_PATH = ROOT / "tests/fixtures/model-surface/v0.2.json"
PLUGIN_PATH = ROOT / "plugins/agentic-compact/.codex-plugin/plugin.json"
SHAM_PATH = ROOT / "eval/sham_mcp.py"
PROTOCOL_VERSION = "2025-06-18"


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(message)


def canonical_bytes(value: Any) -> bytes:
    return json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    ).encode("utf-8")


def default_binary() -> Path:
    target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    if not target.is_absolute():
        target = ROOT / target
    return target / "debug/agentic-compact"


def run_mcp(command: list[str], isolated: bool) -> list[dict[str, Any]]:
    requests = [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": PROTOCOL_VERSION},
        },
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
    ]
    wire = b"".join(canonical_bytes(request) + b"\n" for request in requests)
    env = os.environ.copy()
    with tempfile.TemporaryDirectory(prefix="agentic-compact-surface-") as temp:
        if isolated:
            temp_root = Path(temp)
            codex_home = temp_root / "codex"
            codex_home.mkdir()
            env["CODEX_HOME"] = str(codex_home)
            env["AGENTIC_COMPACT_STATE_DIR"] = str(temp_root / "state")
        completed = subprocess.run(
            command,
            input=wire,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=ROOT,
            env=env,
            check=False,
        )
    require(completed.returncode == 0, "MCP subprocess failed")
    try:
        responses = [json.loads(line) for line in completed.stdout.splitlines()]
    except ValueError as error:
        raise SystemExit("MCP stdout was not JSON-RPC") from error
    require(len(responses) == 2, "MCP returned an unexpected response count")
    return responses


def initialize_surface(result: dict[str, Any]) -> dict[str, Any]:
    server_info = result["serverInfo"]
    return {
        "capabilities": result["capabilities"],
        "serverInfo": {
            "name": server_info["name"],
            "title": server_info["title"],
        },
        "instructions": result["instructions"],
    }


def skill_body_words(text: str) -> int:
    lines = text.splitlines()
    require(bool(lines) and lines[0] == "---", "skill frontmatter is missing")
    try:
        closing = lines.index("---", 1)
    except ValueError as error:
        raise SystemExit("skill frontmatter is not closed") from error
    return len(" ".join(lines[closing + 1 :]).split())


def verify(binary: Path) -> str:
    require(binary.is_file(), "production binary is missing; build it before verification")
    fixture = json.loads(FIXTURE_PATH.read_text(encoding="utf-8"))
    plugin = json.loads(PLUGIN_PATH.read_text(encoding="utf-8"))

    production = run_mcp([str(binary), "mcp"], isolated=True)
    sham = run_mcp([sys.executable, str(SHAM_PATH), "mcp"], isolated=False)
    require(production == sham, "production and sham MCP surfaces differ")

    initialize = production[0]["result"]
    tools = production[1]["result"]["tools"]
    require(initialize["protocolVersion"] == PROTOCOL_VERSION, "protocol version changed")
    require(initialize_surface(initialize) == fixture["initialize"], "initialize surface changed")
    require(tools == fixture["tools"], "tool surface changed")

    default_prompt = plugin["interface"]["defaultPrompt"]
    require(default_prompt == fixture["plugin"]["defaultPrompt"], "defaultPrompt changed")
    skill_path = ROOT / fixture["plugin"]["skillPath"]
    skill_bytes = skill_path.read_bytes()
    skill_text = skill_bytes.decode("utf-8")
    require(
        hashlib.sha256(skill_bytes).hexdigest() == fixture["plugin"]["skillSha256"],
        "skill text changed",
    )

    budgets = fixture["budgets"]
    require(len(tools) == budgets["toolCount"], "tool count budget failed")
    require(
        len(canonical_bytes(tools)) <= budgets["normalizedToolsBytesMax"],
        "normalized tool bytes budget failed",
    )
    require(len(skill_bytes) <= budgets["skillBytesMax"], "skill bytes budget failed")
    require(
        skill_body_words(skill_text) <= budgets["skillBodyWordsMax"],
        "skill body word budget failed",
    )
    require(
        len(initialize["instructions"].split()) <= budgets["initializeWordsMax"],
        "initialize word budget failed",
    )
    require(
        len(default_prompt) == budgets["defaultPromptCount"],
        "defaultPrompt count budget failed",
    )
    require(
        len(default_prompt[0].encode("utf-8")) <= budgets["defaultPromptBytesMax"],
        "defaultPrompt bytes budget failed",
    )

    frozen = {
        "initialize": fixture["initialize"],
        "tools": tools,
        "defaultPrompt": default_prompt,
        "skillSha256": fixture["plugin"]["skillSha256"],
    }
    return hashlib.sha256(canonical_bytes(frozen)).hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=default_binary())
    args = parser.parse_args()
    surface_hash = verify(args.binary.resolve())
    print(f"model surface verified: {surface_hash}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
