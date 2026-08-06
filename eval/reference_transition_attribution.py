"""Project one real v0.1 reference transition into privacy-safe attribution evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any

from codex_context_telemetry import analyze_context_telemetry
from eval_contract import ArtifactValidationError, canonical_json


AGENTIC_SERVERS = {"agentic-compact", "agentic_compact"}
AGENTIC_TOOLS = {"request_compaction", "agentic_compact.request_compaction"}
CHECKPOINT_KEYS = {
    "version",
    "checkpointId",
    "receiptId",
    "sourceThreadId",
    "sourceTurnId",
    "compactTurnId",
    "createdAtMs",
    "trigger",
    "model",
    "evidence",
    "sha256",
}
JOURNAL_KEYS = {
    "schemaVersion",
    "threadId",
    "sourceTurnId",
    "receiptId",
    "checkpointId",
    "intent",
    "state",
    "compactTurnId",
    "checkpoint",
    "checkpointSha256",
    "continuationTurnId",
    "reasonCode",
    "lastDetail",
    "createdAtMs",
    "updatedAtMs",
}


def sha256(value: str | bytes) -> str:
    data = value.encode() if isinstance(value, str) else value
    return hashlib.sha256(data).hexdigest()


def _object(value: Any, message: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ArtifactValidationError(message)
    return value


def _turns(snapshot: Any) -> tuple[str, list[dict[str, Any]]]:
    thread = _object(_object(snapshot, "snapshot is not an object").get("thread"), "snapshot lacks a thread")
    thread_id = thread.get("id")
    turns = thread.get("turns")
    if not isinstance(thread_id, str) or not isinstance(turns, list):
        raise ArtifactValidationError("snapshot thread identity or turns are invalid")
    normalized: list[dict[str, Any]] = []
    turn_ids: set[str] = set()
    item_ids: set[str] = set()
    for turn in turns:
        turn = _object(turn, "snapshot contains a non-object turn")
        turn_id = turn.get("id")
        items = turn.get("items")
        if (
            not isinstance(turn_id, str)
            or turn_id in turn_ids
            or turn.get("status") != "completed"
            or not isinstance(items, list)
        ):
            raise ArtifactValidationError("snapshot turn identity or terminal state is invalid")
        turn_ids.add(turn_id)
        for item in items:
            item = _object(item, "snapshot contains a non-object item")
            item_id = item.get("id")
            if not isinstance(item_id, str) or item_id in item_ids:
                raise ArtifactValidationError("snapshot item identity is missing or duplicated")
            item_ids.add(item_id)
        normalized.append(turn)
    return thread_id, normalized


def _scheduled_source(
    turns: list[dict[str, Any]],
) -> tuple[int, dict[str, Any], str, str]:
    scheduled: list[tuple[int, dict[str, Any], str, str]] = []
    for position, turn in enumerate(turns):
        for item in turn["items"]:
            if item.get("server") not in AGENTIC_SERVERS or item.get("tool") not in AGENTIC_TOOLS:
                continue
            result = item.get("result")
            structured = result.get("structuredContent") if isinstance(result, dict) else None
            if not isinstance(structured, dict) or structured.get("status") != "scheduled_after_turn":
                continue
            if set(structured) != {"status", "receiptId", "checkpointId"}:
                raise ArtifactValidationError("v0.1 scheduled result shape is invalid")
            receipt = structured.get("receiptId")
            checkpoint = structured.get("checkpointId")
            if not isinstance(receipt, str) or not isinstance(checkpoint, str):
                raise ArtifactValidationError("scheduled request lacks correlation identities")
            scheduled.append((position, item, receipt, checkpoint))
    if len(scheduled) != 1:
        raise ArtifactValidationError("reference smoke must contain exactly one scheduled request")
    return scheduled[0]


def _canonical_model(value: Any) -> dict[str, Any]:
    model = _object(value, "checkpoint model is invalid")
    preserve = model.get("preserve")
    next_action = model.get("nextAction")
    if (
        set(model) != {"preserve", "nextAction"}
        or not isinstance(preserve, list)
        or any(not isinstance(item, str) for item in preserve)
        or not isinstance(next_action, str)
    ):
        raise ArtifactValidationError("checkpoint model shape is invalid")
    return {"preserve": preserve, "nextAction": next_action}


def _canonical_evidence(value: Any) -> dict[str, Any]:
    evidence = _object(value, "checkpoint evidence is invalid")
    allowed = {"lastUserObjective", "changedFiles", "verification"}
    if not set(evidence) <= allowed:
        raise ArtifactValidationError("checkpoint evidence has unknown fields")
    canonical: dict[str, Any] = {}
    objective = evidence.get("lastUserObjective")
    if "lastUserObjective" in evidence:
        if not isinstance(objective, str):
            raise ArtifactValidationError("checkpoint objective is invalid")
        canonical["lastUserObjective"] = objective
    changed = evidence.get("changedFiles")
    if "changedFiles" in evidence:
        if not isinstance(changed, list) or any(
            not isinstance(item, str) for item in changed
        ):
            raise ArtifactValidationError("checkpoint changed files are invalid")
        canonical["changedFiles"] = changed
    verification = evidence.get("verification")
    if "verification" in evidence:
        if not isinstance(verification, list):
            raise ArtifactValidationError("checkpoint verification is invalid")
        ordered: list[dict[str, Any]] = []
        for raw_item in verification:
            item = _object(raw_item, "checkpoint verification item is invalid")
            required = {"itemId", "kind", "label", "status"}
            if not required <= set(item) <= required | {"exitCode"} or any(
                not isinstance(item[key], str) for key in required
            ):
                raise ArtifactValidationError("checkpoint verification shape is invalid")
            normalized = {key: item[key] for key in ("itemId", "kind", "label", "status")}
            if "exitCode" in item:
                if type(item["exitCode"]) is not int:
                    raise ArtifactValidationError("checkpoint exit code is invalid")
                normalized["exitCode"] = item["exitCode"]
            ordered.append(normalized)
        canonical["verification"] = ordered
    return canonical


def _checkpoint_sha256(checkpoint: Any) -> str:
    checkpoint = _object(checkpoint, "terminal journal lacks a checkpoint")
    if set(checkpoint) != CHECKPOINT_KEYS:
        raise ArtifactValidationError("checkpoint has unknown or missing fields")
    unsigned = {
        "version": checkpoint["version"],
        "checkpointId": checkpoint["checkpointId"],
        "receiptId": checkpoint["receiptId"],
        "sourceThreadId": checkpoint["sourceThreadId"],
        "sourceTurnId": checkpoint["sourceTurnId"],
        "compactTurnId": checkpoint["compactTurnId"],
        "createdAtMs": checkpoint["createdAtMs"],
        "trigger": checkpoint["trigger"],
        "model": _canonical_model(checkpoint["model"]),
        "evidence": _canonical_evidence(checkpoint["evidence"]),
    }
    actual = sha256(
        json.dumps(unsigned, ensure_ascii=False, separators=(",", ":")).encode()
    )
    if checkpoint["sha256"] != actual:
        raise ArtifactValidationError("checkpoint SHA-256 does not match its payload")
    return actual


def _journal(journal_dir: Path) -> tuple[dict[str, Any], str]:
    files = sorted(path for path in journal_dir.iterdir() if path.is_file())
    if len(files) != 1:
        raise ArtifactValidationError("reference smoke must contain one terminal journal copy")
    raw = files[0].read_bytes()
    journal = _object(json.loads(raw), "terminal journal is not an object")
    if set(journal) != JOURNAL_KEYS or journal.get("schemaVersion") != 1:
        raise ArtifactValidationError("terminal journal has unknown or missing fields")
    if journal.get("state") != "COOLDOWN" or journal.get("reasonCode") is not None:
        raise ArtifactValidationError("reference transition did not reach cooldown")
    checkpoint_sha = _checkpoint_sha256(journal.get("checkpoint"))
    if journal.get("checkpointSha256") != checkpoint_sha:
        raise ArtifactValidationError("journal checkpoint hash does not reconcile")
    return journal, sha256(raw)


def attribute_reference_transition(
    snapshot: Any,
    journal_dir: Path,
    telemetry_events: list[dict[str, Any]],
    summary: Any,
) -> dict[str, Any]:
    thread_id, turns = _turns(snapshot)
    source_position, source_item, receipt_id, checkpoint_id = _scheduled_source(turns)
    journal, journal_sha = _journal(journal_dir)
    compact_position = source_position + 1
    continuation_position = compact_position + 1
    if continuation_position >= len(turns):
        raise ArtifactValidationError("reference transition is not chronologically complete")
    source_turn = turns[source_position]
    compact_turn = turns[compact_position]
    continuation_turn = turns[continuation_position]
    compact_items = compact_turn["items"]
    if len(compact_items) != 1 or compact_items[0].get("type") != "contextCompaction":
        raise ArtifactValidationError("reference compaction turn is not unique and pure")
    continuation_items = continuation_turn["items"]
    if any(item.get("type") == "contextCompaction" for item in continuation_items):
        raise ArtifactValidationError("reference continuation is another compaction turn")
    user_messages = sum(item.get("type") == "userMessage" for item in continuation_items)
    if user_messages > 1:
        raise ArtifactValidationError("reference continuation contains ambiguous user input")
    identities = [
        journal.get("threadId") == thread_id,
        journal.get("sourceTurnId") == source_turn.get("id"),
        journal.get("receiptId") == receipt_id,
        journal.get("checkpointId") == checkpoint_id,
        journal.get("compactTurnId") == compact_turn.get("id"),
        journal.get("continuationTurnId") == continuation_turn.get("id"),
    ]
    checkpoint = _object(journal.get("checkpoint"), "terminal journal lacks a checkpoint")
    identities.extend(
        [
            checkpoint.get("sourceThreadId") == thread_id,
            checkpoint.get("sourceTurnId") == source_turn.get("id"),
            checkpoint.get("receiptId") == receipt_id,
            checkpoint.get("checkpointId") == checkpoint_id,
            checkpoint.get("compactTurnId") == compact_turn.get("id"),
        ]
    )
    if not all(identities):
        raise ArtifactValidationError("reference transition identities do not reconcile")
    telemetry = analyze_context_telemetry(telemetry_events)
    compact_turn_hash = sha256(compact_turn["id"])
    observations = [
        item
        for item in telemetry["compactions"]
        if item["compactionTurnHash"] == compact_turn_hash
    ]
    if len(observations) != 1:
        raise ArtifactValidationError("direct telemetry does not uniquely bind the compaction")
    observation = observations[0]
    if (
        observation["preCompactionTurnHash"] != sha256(source_turn["id"])
        or observation["postCompactionTurnHash"] != sha256(continuation_turn["id"])
    ):
        raise ArtifactValidationError("direct occupancy evidence binds different turns")
    summary = _object(summary, "run summary is not an object")
    if (
        summary.get("arm") != "v0.1"
        or summary.get("agenticRequests") != 1
        or summary.get("scheduledAgenticRequests") != 1
        or summary.get("terminalJournalCopies") != 1
        or summary.get("contextCompactions") != len(telemetry["compactions"])
        or summary.get("serverSurvived") is not True
        or summary.get("mcpProcessStable") is not True
        or summary.get("mcpProcessCount") != 1
        or not isinstance(summary.get("mcpProcessIdentityHash"), str)
    ):
        raise ArtifactValidationError("reference run counters or process survival do not reconcile")
    pre = observation["preCompactionInputTokens"]
    post = observation["postCompactionInputTokens"]
    return {
        "schemaVersion": 1,
        "arm": "v0.1",
        "threadHash": sha256(thread_id),
        "sourceTurnHash": sha256(source_turn["id"]),
        "sourceItemHash": sha256(source_item["id"]),
        "sourcePosition": source_position,
        "compactionTurnHash": compact_turn_hash,
        "compactionItemHash": observation["compactionItemHash"],
        "compactionPosition": compact_position,
        "continuationTurnHash": sha256(continuation_turn["id"]),
        "continuationPosition": continuation_position,
        "continuationKind": "user-wins" if user_messages else "auto",
        "terminalState": "cooldown",
        "journalSha256": journal_sha,
        "checkpointSha256": journal["checkpointSha256"],
        "preCompactionInputTokens": pre,
        "preCompactionUsageEventHash": observation["preCompactionUsageEventHash"],
        "postCompactionInputTokens": post,
        "postCompactionUsageEventHash": observation["postCompactionUsageEventHash"],
        "activeInputTokensRemoved": pre - post,
        "retainedRatio": post / pre,
        "contextCompactions": len(telemetry["compactions"]),
        "agenticTransitions": 1,
        "unattributedCompactions": len(telemetry["compactions"]) - 1,
        "mcpProcessIdentityHash": summary["mcpProcessIdentityHash"],
        "sameThread": True,
        "processSurvived": True,
        "injectionCount": 1,
        "continuationCount": 1,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--snapshot", type=Path, required=True)
    parser.add_argument("--journal-dir", type=Path, required=True)
    parser.add_argument("--telemetry", type=Path, required=True)
    parser.add_argument("--summary", type=Path, required=True)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    telemetry_events = [
        json.loads(line) for line in args.telemetry.read_text().splitlines() if line
    ]
    projected = attribute_reference_transition(
        json.loads(args.snapshot.read_text()),
        args.journal_dir,
        telemetry_events,
        json.loads(args.summary.read_text()),
    )
    encoded = canonical_json(projected) + "\n"
    if args.output is None:
        print(encoded, end="")
    else:
        args.output.write_text(encoded, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
