"""Synthetic regression for privacy-safe reference transition attribution."""

from __future__ import annotations

import json
import tempfile
from pathlib import Path
from typing import Any

from eval_contract import ArtifactValidationError
from reference_transition_attribution import attribute_reference_transition, sha256


def _checkpoint(thread: str, source: str, compact: str, receipt: str) -> dict[str, Any]:
    unsigned = {
        "version": 1,
        "checkpointId": "checkpoint",
        "receiptId": receipt,
        "sourceThreadId": thread,
        "sourceTurnId": source,
        "compactTurnId": compact,
        "createdAtMs": 1,
        "trigger": "model_semantic_boundary",
        "model": {"preserve": ["verified state"], "nextAction": "continue"},
        "evidence": {},
    }
    return {
        **unsigned,
        "sha256": sha256(
            json.dumps(unsigned, ensure_ascii=False, separators=(",", ":")).encode()
        ),
    }


def run_reference_transition_selftest() -> None:
    thread = "thread"
    source = "source"
    compact = "compact"
    continuation = "continuation"
    receipt = "rcpt_00000000000000000000000000000000"
    checkpoint = _checkpoint(thread, source, compact, receipt)
    checkpoint["model"] = {
        "nextAction": checkpoint["model"]["nextAction"],
        "preserve": checkpoint["model"]["preserve"],
    }
    checkpoint = dict(sorted(checkpoint.items()))
    journal = {
        "schemaVersion": 1,
        "threadId": thread,
        "sourceTurnId": source,
        "receiptId": receipt,
        "checkpointId": "checkpoint",
        "intent": {"preserve": ["verified state"], "nextAction": "continue"},
        "state": "COOLDOWN",
        "compactTurnId": compact,
        "checkpoint": checkpoint,
        "checkpointSha256": checkpoint["sha256"],
        "continuationTurnId": continuation,
        "reasonCode": None,
        "lastDetail": "same-thread empty continuation started",
        "createdAtMs": 1,
        "updatedAtMs": 2,
    }
    snapshot = {
        "thread": {
            "id": thread,
            "turns": [
                {
                    "id": source,
                    "status": "completed",
                    "items": [
                        {
                            "id": "source-item",
                            "type": "mcpToolCall",
                            "server": "agentic-compact",
                            "tool": "request_compaction",
                            "result": {
                                "structuredContent": {
                                    "status": "scheduled_after_turn",
                                    "receiptId": receipt,
                                    "checkpointId": "checkpoint",
                                },
                            },
                        }
                    ],
                },
                {
                    "id": compact,
                    "status": "completed",
                    "items": [
                        {"id": "snapshot-compact-item", "type": "contextCompaction"}
                    ],
                },
                {
                    "id": continuation,
                    "status": "completed",
                    "items": [{"id": "continuation-item", "type": "agentMessage"}],
                },
            ],
        }
    }
    telemetry = [
        {
            "method": "thread/tokenUsage/updated",
            "monotonicMs": 1,
            "threadHash": sha256(thread),
            "turnHash": sha256(source),
            "tokenUsage": {
                "modelContextWindow": 258_400,
                "last": {"inputTokens": 100},
                "total": {"inputTokens": 100},
            },
        },
        {
            "method": "item/started",
            "monotonicMs": 2,
            "threadHash": sha256(thread),
            "turnHash": sha256(compact),
            "itemHash": sha256("compact-item"),
            "itemType": "contextCompaction",
        },
        {
            "method": "item/completed",
            "monotonicMs": 3,
            "threadHash": sha256(thread),
            "turnHash": sha256(compact),
            "itemHash": sha256("compact-item"),
            "itemType": "contextCompaction",
        },
        {
            "method": "thread/tokenUsage/updated",
            "monotonicMs": 4,
            "threadHash": sha256(thread),
            "turnHash": sha256(continuation),
            "tokenUsage": {
                "modelContextWindow": 258_400,
                "last": {"inputTokens": 40},
                "total": {"inputTokens": 140},
            },
        },
    ]
    summary = {
        "arm": "v0.1",
        "agenticRequests": 1,
        "scheduledAgenticRequests": 1,
        "terminalJournalCopies": 1,
        "contextCompactions": 1,
        "serverSurvived": True,
        "mcpProcessStable": True,
        "mcpProcessCount": 1,
        "mcpProcessIdentityHash": "1" * 64,
    }
    with tempfile.TemporaryDirectory(prefix="ac-reference-") as directory:
        journal_dir = Path(directory)
        (journal_dir / "journal.json").write_text(json.dumps(journal))
        projected = attribute_reference_transition(
            snapshot, journal_dir, telemetry, summary
        )
        assert projected["agenticTransitions"] == 1
        assert projected["activeInputTokensRemoved"] == 60
        assert projected["retainedRatio"] == 0.4
        journal["continuationTurnId"] = "different"
        (journal_dir / "journal.json").write_text(json.dumps(journal))
        try:
            attribute_reference_transition(snapshot, journal_dir, telemetry, summary)
        except ArtifactValidationError:
            pass
        else:
            raise AssertionError("cross-sequence journal identity was accepted")
