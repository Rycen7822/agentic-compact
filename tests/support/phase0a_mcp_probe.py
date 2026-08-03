#!/usr/bin/env python3
"""Disposable stdio MCP server for frozen-host characterization only."""

import json
import os
import sys


OUTPUT_SCHEMA = {
    "oneOf": [
        {
            "type": "object",
            "properties": {"status": {"const": "scheduled_after_turn"}},
            "required": ["status"],
            "additionalProperties": False,
        },
        {
            "type": "object",
            "properties": {
                "status": {"const": "rejected"},
                "reasonCode": {"type": "string"},
                "message": {"type": "string"},
                "retryable": {"const": False},
            },
            "required": ["status", "reasonCode", "message", "retryable"],
            "additionalProperties": False,
        },
    ]
}

TOOL = {
    "name": "request_compaction",
    "title": "Request Agentic Context Compaction",
    "description": "Phase 0A host characterization probe.",
    "inputSchema": {
        "type": "object",
        "properties": {
            "preserve": {
                "type": "array",
                "items": {"type": "string"},
                "maxItems": 4,
            },
            "next_action": {"type": "string", "minLength": 1},
        },
        "required": ["next_action"],
        "additionalProperties": False,
    },
    "outputSchema": OUTPUT_SCHEMA,
}


def record(event):
    path = os.environ["PHASE0A_PROBE_LOG"]
    with open(path, "a", encoding="utf-8") as stream:
        stream.write(json.dumps({"pid": os.getpid(), "event": event}) + "\n")


def result(arguments):
    preserve = arguments.get("preserve", [])
    if "force rejection" in preserve:
        rejected = {
            "status": "rejected",
            "reasonCode": "shared_app_server_unavailable",
            "message": "The shared Codex app-server is unavailable; continue without compaction.",
            "retryable": False,
        }
        return {
            "content": [{"type": "text", "text": json.dumps(rejected, separators=(",", ":"))}],
            "structuredContent": rejected,
            "isError": False,
        }
    return {
        "content": [],
        "structuredContent": {"status": "scheduled_after_turn"},
        "_meta": {
            "agenticCompact": {"receiptId": os.environ["PHASE0A_RECEIPT"]}
        },
        "isError": False,
    }


def respond(request):
    method = request.get("method")
    if method == "initialize":
        record("initialize")
        return {
            "protocolVersion": request.get("params", {}).get(
                "protocolVersion", "2025-06-18"
            ),
            "capabilities": {"tools": {"listChanged": False}},
            "serverInfo": {"name": "phase0a-probe", "version": "1"},
        }
    if method == "ping":
        return {}
    if method == "tools/list":
        return {"tools": [TOOL]}
    if method == "tools/call":
        record("call")
        return result(request.get("params", {}).get("arguments", {}))
    raise ValueError("unsupported method")


for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    try:
        response = {"jsonrpc": "2.0", "id": request["id"], "result": respond(request)}
    except (KeyError, TypeError, ValueError):
        response = {
            "jsonrpc": "2.0",
            "id": request["id"],
            "error": {"code": -32602, "message": "invalid probe request"},
        }
    print(json.dumps(response, separators=(",", ":")), flush=True)
