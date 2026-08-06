"""Account for context usage reported directly by Codex app-server telemetry."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any

from eval_contract import ArtifactValidationError, canonical_json


def _event_hash(event: dict[str, Any]) -> str:
    return hashlib.sha256(canonical_json(event).encode("ascii")).hexdigest()


def analyze_context_telemetry(events: list[dict[str, Any]]) -> dict[str, Any]:
    if not events:
        raise ArtifactValidationError("Codex context telemetry is empty")
    encoded_lines: list[bytes] = []
    usages: list[dict[str, Any]] = []
    compaction_bounds: dict[str, dict[str, Any]] = {}
    thread_hashes: set[str] = set()
    context_windows: set[int] = set()
    previous_monotonic = -1
    previous_total_input = 0

    for index, event in enumerate(events):
        if not isinstance(event, dict):
            raise ArtifactValidationError("Codex telemetry contains a non-object event")
        encoded_lines.append(canonical_json(event).encode("ascii") + b"\n")
        monotonic = event.get("monotonicMs")
        if type(monotonic) is not int or monotonic < previous_monotonic:
            raise ArtifactValidationError("Codex telemetry order is invalid")
        previous_monotonic = monotonic
        thread_hash = event.get("threadHash")
        if isinstance(thread_hash, str):
            thread_hashes.add(thread_hash)

        usage = event.get("tokenUsage")
        if isinstance(usage, dict):
            if event.get("method") != "thread/tokenUsage/updated":
                raise ArtifactValidationError(
                    "token usage was not reported by Codex token telemetry"
                )
            last = usage.get("last")
            total = usage.get("total")
            window = usage.get("modelContextWindow")
            if not isinstance(last, dict) or not isinstance(total, dict):
                raise ArtifactValidationError("token-usage event is incomplete")
            input_tokens = last.get("inputTokens")
            total_input = total.get("inputTokens")
            if (
                type(input_tokens) is not int
                or input_tokens < 0
                or type(total_input) is not int
                or total_input < previous_total_input
                or type(window) is not int
                or window <= 0
                or not isinstance(event.get("turnHash"), str)
            ):
                raise ArtifactValidationError("token-usage counters are invalid")
            context_windows.add(window)
            if total_input > previous_total_input:
                if total_input - previous_total_input != input_tokens:
                    raise ArtifactValidationError(
                        "Codex telemetry skipped a cumulative input advance"
                    )
                previous_total_input = total_input
                usages.append(
                    {
                        "index": index,
                        "turnHash": event.get("turnHash"),
                        "usageEventHash": _event_hash(event),
                        "inputTokens": input_tokens,
                        "totalInputTokens": total_input,
                    }
                )

        if event.get("itemType") != "contextCompaction":
            continue
        item_hash = event.get("itemHash")
        turn_hash = event.get("turnHash")
        if not isinstance(item_hash, str) or not isinstance(turn_hash, str):
            raise ArtifactValidationError("compaction event lacks hashed identity")
        bounds = compaction_bounds.setdefault(
            item_hash, {"turnHash": turn_hash, "start": None, "completed": None}
        )
        if bounds["turnHash"] != turn_hash:
            raise ArtifactValidationError("compaction item changed turns")
        method = event.get("method")
        field = "start" if method == "item/started" else "completed"
        if method not in {"item/started", "item/completed"} or bounds[field] is not None:
            raise ArtifactValidationError("compaction lifecycle is invalid")
        bounds[field] = index

    if len(thread_hashes) != 1 or len(context_windows) != 1 or not usages:
        raise ArtifactValidationError("Codex telemetry is not single-thread complete")
    if sum(item["inputTokens"] for item in usages) != usages[-1]["totalInputTokens"]:
        raise ArtifactValidationError("Codex telemetry does not reconcile with final input")

    if any(
        bounds["start"] is None
        or bounds["completed"] is None
        or bounds["start"] >= bounds["completed"]
        for bounds in compaction_bounds.values()
    ):
        raise ArtifactValidationError("compaction lifecycle is incomplete")
    ordered_bounds = sorted(compaction_bounds.items(), key=lambda item: item[1]["start"])
    observations: list[dict[str, Any]] = []
    for ordinal, (item_hash, bounds) in enumerate(ordered_bounds, start=1):
        start = bounds["start"]
        completed = bounds["completed"]
        pre = next(
            (usage for usage in reversed(usages) if usage["index"] < start), None
        )
        post = next(
            (
                usage
                for usage in usages
                if usage["index"] > completed and usage["inputTokens"] > 0
            ),
            None,
        )
        if pre is None or pre["inputTokens"] <= 0 or post is None:
            raise ArtifactValidationError("compaction lacks adjacent token-usage evidence")
        next_start = next(
            (
                later["start"]
                for _, later in ordered_bounds
                if later["start"] > start
            ),
            None,
        )
        if next_start is not None and post["index"] > next_start:
            raise ArtifactValidationError("compaction observations overlap")
        observations.append(
            {
                "ordinal": ordinal,
                "compactionItemHash": item_hash,
                "compactionTurnHash": bounds["turnHash"],
                "preCompactionTurnHash": pre["turnHash"],
                "preCompactionUsageEventHash": pre["usageEventHash"],
                "preCompactionInputTokens": pre["inputTokens"],
                "postCompactionTurnHash": post["turnHash"],
                "postCompactionUsageEventHash": post["usageEventHash"],
                "postCompactionInputTokens": post["inputTokens"],
            }
        )

    positive_usages = [usage for usage in usages if usage["inputTokens"] > 0]
    if not positive_usages:
        raise ArtifactValidationError("Codex telemetry contains no token-usage sample")
    return {
        "contextTelemetrySha256": hashlib.sha256(b"".join(encoded_lines)).hexdigest(),
        "tokenUsageSamples": len(positive_usages),
        "finalReportedInputTokens": usages[-1]["totalInputTokens"],
        "peakActiveInputTokens": max(
            usage["inputTokens"] for usage in positive_usages
        ),
        "modelContextWindow": context_windows.pop(),
        "compactions": observations,
    }


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Summarize context-usage telemetry reported by Codex."
    )
    parser.add_argument("telemetry", type=Path)
    args = parser.parse_args()
    events = [
        json.loads(line)
        for line in args.telemetry.read_text().splitlines()
        if line
    ]
    print(canonical_json(analyze_context_telemetry(events)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
