#!/usr/bin/env python3
"""Deterministic no-op MCP used to isolate the candidate model surface."""

from __future__ import annotations

import argparse
import json
import sys
import tomllib
from pathlib import Path
from typing import Any, cast

ROOT = Path(__file__).resolve().parents[1]
FIXTURE_PATH = ROOT / "tests/fixtures/model-surface/v0.2.json"
CARGO_TOML = ROOT / "Cargo.toml"
DEFAULT_PROTOCOL_VERSION = "2025-06-18"


def load_fixture() -> dict[str, Any]:
    return cast(dict[str, Any], json.loads(FIXTURE_PATH.read_text(encoding="utf-8")))


def package_version() -> str:
    with CARGO_TOML.open("rb") as handle:
        return str(tomllib.load(handle)["package"]["version"])


def initialize_result(protocol_version: str) -> dict[str, Any]:
    surface = load_fixture()["initialize"]
    return {
        "protocolVersion": protocol_version,
        "capabilities": surface["capabilities"],
        "serverInfo": {
            **surface["serverInfo"],
            "version": package_version(),
        },
        "instructions": surface["instructions"],
    }


def tools_result() -> dict[str, Any]:
    return {"tools": load_fixture()["tools"]}


def rejected_result(message: str = "The shared Codex app-server is unavailable; continue without compaction.") -> dict[str, Any]:
    structured = {
        "status": "rejected",
        "reasonCode": "shared_app_server_unavailable",
        "message": message,
        "retryable": False,
    }
    return {
        "content": [
            {
                "type": "text",
                "text": json.dumps(structured, separators=(",", ":"), ensure_ascii=False),
            }
        ],
        "structuredContent": structured,
        "isError": False,
    }


def hard_rejection(message: str) -> dict[str, Any]:
    result = rejected_result(message)
    result["structuredContent"]["reasonCode"] = "invalid_request"
    result["content"][0]["text"] = json.dumps(
        result["structuredContent"], separators=(",", ":"), ensure_ascii=False
    )
    result["isError"] = True
    return result


def handle_request(request: dict[str, Any]) -> dict[str, Any] | None:
    if "id" not in request:
        return None

    request_id = request["id"]
    method = request.get("method")
    params = request.get("params") or {}
    if method == "initialize":
        result = initialize_result(
            str(params.get("protocolVersion", DEFAULT_PROTOCOL_VERSION))
        )
    elif method == "ping":
        result = {}
    elif method == "tools/list":
        result = tools_result()
    elif method == "tools/call":
        if params.get("name") == "request_compaction":
            result = rejected_result()
        else:
            result = hard_rejection("Unknown MCP tool.")
    else:
        return {
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {
                "code": -32602,
                "message": "Unsupported MCP method.",
                "data": {"reasonCode": "invalid_request", "retryable": False},
            },
        }
    return {"jsonrpc": "2.0", "id": request_id, "result": result}


def serve() -> int:
    for raw_line in sys.stdin.buffer:
        response: dict[str, Any] | None
        try:
            request = json.loads(raw_line)
        except (TypeError, ValueError):
            response = {
                "jsonrpc": "2.0",
                "id": None,
                "error": {
                    "code": -32700,
                    "message": "Invalid JSON-RPC request.",
                    "data": {"reasonCode": "invalid_request", "retryable": False},
                },
            }
        else:
            response = handle_request(request)
        if response is not None:
            print(json.dumps(response, separators=(",", ":"), ensure_ascii=False), flush=True)
    return 0


def self_test() -> int:
    fixture = load_fixture()
    assert tools_result() == {"tools": fixture["tools"]}
    assert len(fixture["tools"]) == 1
    assert fixture["tools"][0]["name"] == "request_compaction"
    initialized = initialize_result(DEFAULT_PROTOCOL_VERSION)
    assert initialized["instructions"] == fixture["initialize"]["instructions"]
    rejected = rejected_result()
    assert rejected["isError"] is False
    assert rejected["structuredContent"]["reasonCode"] == "shared_app_server_unavailable"
    assert rejected["structuredContent"]["retryable"] is False
    assert "_meta" not in rejected
    assert (
        json.loads(rejected["content"][0]["text"])
        == rejected["structuredContent"]
    )
    print("sham MCP self-test passed")
    return 0


def main(arguments: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", nargs="?", choices=("mcp",))
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(arguments)
    if args.self_test:
        if args.command is not None:
            parser.error("--self-test cannot be combined with a command")
        return self_test()
    if args.command != "mcp":
        parser.error("the mcp command is required")
    return serve()


if __name__ == "__main__":
    raise SystemExit(main())
