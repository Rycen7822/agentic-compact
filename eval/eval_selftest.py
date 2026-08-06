"""Synthetic contract tests; these fixtures are never benchmark evidence."""

from __future__ import annotations

import hashlib
import json
import tempfile
from pathlib import Path
from typing import Any

from codex_context_telemetry import analyze_context_telemetry
from eval_contract import (
    EXPECTED_THRESHOLDS,
    EvaluationError,
    PROMPT_FILES,
    RESET2_CORPUS_PROVENANCE_SHA256,
    RESET2_SELECTION_RULE,
    RestrictedSchema,
    canonical_json,
    expected_result_keys,
    load_schemas,
    result_path,
    sha256_file,
    validate_annotation,
    validate_block_attempts,
    validate_manifest,
    validate_privacy,
    validate_result_tree,
    validate_run,
)
from eval_metrics import (
    TOKEN_FIELDS,
    _gate,
    bootstrap_mean,
    bootstrap_median,
    derive_summary,
    expected_double_review_ids,
    majority,
    mcnemar,
    nearest_rank,
    validate_summary,
)
from reference_transition_selftest import run_reference_transition_selftest


def _synthetic_manifest() -> dict[str, Any]:
    digest = "a" * 64
    tasks: list[dict[str, Any]] = []
    for index in range(20):
        selection_evidence = ["active-context-pressure", "semantic-bundle"]
        tasks.append(
            {
                "taskId": f"dev-{index:02d}",
                "benchmark": "swe-bench-verified",
                "role": "dev",
                "baseCommit": "b" * 40,
                "fallbackExposed": False,
                "semanticBundleEligible": True,
                "diagnosticSubset": False,
                "selectionEvidence": selection_evidence,
                "taskConfigSha256": digest,
            }
        )
    for index in range(50):
        selection_evidence = [
            "native-fallback" if index < 15 else "active-context-pressure"
        ]
        if index < 20:
            selection_evidence.append("semantic-bundle")
        tasks.append(
            {
                "taskId": f"held-{index:02d}",
                "benchmark": "swe-bench-verified",
                "role": "held-out",
                "baseCommit": "b" * 40,
                "fallbackExposed": index < 15,
                "semanticBundleEligible": index < 20,
                "diagnosticSubset": index < 20,
                "selectionEvidence": selection_evidence,
                "taskConfigSha256": digest,
            }
        )
    return {
        "schemaVersion": 1,
        "manifestId": "agentic-compact-v0.2",
        "selectionRule": RESET2_SELECTION_RULE,
        "corpusProvenanceSha256": RESET2_CORPUS_PROVENANCE_SHA256,
        "seeds": {"armOrderSeed": 11, "bootstrapSeed": 13, "judgeSamplingSeed": 17},
        "runtime": {
            "codexVersion": "0.146.0",
            "codexBinarySha256": digest,
            "model": "gpt-5.6-luna",
            "reasoningEffort": "high",
            "serviceTier": "priority",
            "sandboxMode": "workspace-write",
            "approvalPolicy": "never",
            "runtimeConfigSha256": digest,
            "environmentSha256": digest,
            "dependencyCacheSha256": digest,
            "targetContextWindow": 258_400,
            "fixedFallbackLimit": 49_152,
            "noForcedLimit": 232_560,
        },
        "v01Baseline": {
            "releaseTag": "v0.1.0",
            "sourceCommit": "898061f019ddca8599debd7b15a204040bd6b349",
            "binarySha256": digest,
            "pluginSha256": digest,
            "surfaceSha256": digest,
            "capabilityRecordSha256": digest,
        },
        "judge": {
            "backendId": "synthetic",
            "modelRevision": "synthetic-v1",
            "settingsSha256": digest,
            "judgeConfigSha256": digest,
            "calibrationExamplesSha256": digest,
            "promptSha256": {stream: digest for stream in PROMPT_FILES},
            "doubleReviewRate": 0.10,
            "minimumAgreement": 0.90,
        },
        "thresholds": dict(EXPECTED_THRESHOLDS),
        "tasks": tasks,
    }


def _expect_invalid(action: Any) -> None:
    try:
        action()
    except EvaluationError:
        return
    raise AssertionError("self-test expected a validation failure")


def _synthetic_run(manifest: dict[str, Any]) -> dict[str, Any]:
    digest = "c" * 64
    return {
        "schemaVersion": 1,
        "runId": digest,
        "taskId": "held-00",
        "benchmark": "swe-bench-verified",
        "arm": "stock",
        "repetition": 1,
        "attempt": 1,
        "priorInvalidBlocks": [],
        "candidateCommit": "d" * 40,
        "configHashes": {
            "binarySha256": None,
            "pluginSha256": None,
            "surfaceSha256": None,
            "capabilityRecordSha256": None,
            "configSha256": manifest["runtime"]["runtimeConfigSha256"],
            "taskManifestSha256": digest,
            "codexBinarySha256": manifest["runtime"]["codexBinarySha256"],
        },
        "codexVersion": "0.146.0",
        "model": "gpt-5.6-luna",
        "reasoningEffort": "high",
        "serviceTier": "priority",
        "sandboxMode": "workspace-write",
        "approvalPolicy": "never",
        "contextRegime": "fixed-fallback",
        "resolvedContextLimit": manifest["runtime"]["fixedFallbackLimit"],
        "benchmarkScore": 1.0,
        "solved": True,
        "outcomeClass": "passed",
        "failureClass": None,
        "candidateCaused": False,
        "failureReviewed": False,
        "tokens": {
            "inputTokens": 100,
            "cachedInputTokens": 20,
            "cacheWriteTokens": 0,
            "outputTokens": 10,
            "reasoningOutputTokens": 5,
            "nonCachedInputTokens": 80,
            "blendedTokens": 90,
        },
        "wallTimeMs": 1,
        "threadHash": digest,
        "observer": {
            "finalTokenUsagePresent": True,
            "finalOutcomePresent": True,
            "counterMonotonic": True,
            "threadConsistent": True,
            "targetThreadCount": 1,
            "tokenUsageSamples": 1,
            "peakActiveInputTokens": 100,
            "modelContextWindow": 258_400,
            "contextTelemetrySha256": digest,
        },
        "counters": {
            "turnsStarted": 1,
            "itemsCompleted": 1,
            "toolItemsCompleted": 0,
            "contextCompactions": 0,
            "unattributedCompactions": 0,
            "agenticRequests": 0,
            "scheduledAgenticRequests": 0,
            "agenticTransitions": 0,
        },
        "rejections": [],
        "transitions": [],
        "safety": {
            field: 0
            for field in (
                "duplicateCompactions",
                "crossThreadActions",
                "syntheticUserMessages",
                "lostCheckpoints",
                "duplicateCheckpoints",
                "unsafeActiveWorkTransitions",
                "blindMutationRetries",
                "tuiThreadPageSwitches",
                "resourceLeaks",
                "userWinsFailures",
                "processSurvivalFailures",
                "atMostOnceFailures",
            )
        },
        "semanticAvailability": {
            "trigger": False,
            "checkpoint": False,
            "continuation": False,
        },
        "traceRef": digest,
    }


def _synthetic_annotation(
    manifest: dict[str, Any], run: dict[str, Any]
) -> dict[str, Any]:
    digest = "e" * 64
    return {
        "schemaVersion": 1,
        "annotationId": digest,
        "runId": run["runId"],
        "taskId": run["taskId"],
        "candidateCommit": run["candidateCommit"],
        "contextRegime": run["contextRegime"],
        "arm": run["arm"],
        "stream": "trigger",
        "repetition": run["repetition"],
        "ordinal": 1,
        "sourceAnchor": {
            "kind": "actual-request",
            "turnHash": digest,
            "itemHash": digest,
            "position": 1,
        },
        "preferredAnchor": None,
        "primary": "KEEP",
        "secondary": None,
        "adjudicated": None,
        "final": "KEEP",
        "doubleReviewed": False,
        "promptSha256": manifest["judge"]["promptSha256"]["trigger"],
        "judgeConfigSha256": manifest["judge"]["judgeConfigSha256"],
        "attributes": {
            "phaseBefore": "exploration",
            "phaseAfter": "implementation",
            "substantialWorkRemaining": True,
            "stateStable": True,
            "activeWork": False,
            "recentCompaction": False,
            "criticalContradiction": None,
            "criticalOmission": None,
            "nextActionActionable": None,
            "hostEvidenceMatch": None,
            "repeatedSettledPhase": None,
        },
        "taxonomy": None,
        "rationale": "Synthetic schema fixture.",
        "rawBundleAvailable": True,
    }


def _synthetic_summary(manifest: dict[str, Any]) -> dict[str, Any]:
    digest = "f" * 64
    return {
        "schemaVersion": 1,
        "stage": "stage-b",
        "candidateCommit": "d" * 40,
        "provenance": {
            field: digest
            for field in (
                "binarySha256",
                "pluginSha256",
                "surfaceSha256",
                "capabilityRecordSha256",
                "configSha256",
                "taskManifestSha256",
                "judgeConfigSha256",
                "codexBinarySha256",
                "resultSetSha256",
            )
        },
        "runtime": {
            field: manifest["runtime"][field]
            for field in (
                "codexVersion",
                "model",
                "reasoningEffort",
                "serviceTier",
                "sandboxMode",
                "approvalPolicy",
            )
        },
        "counts": {
            field: 0
            for field in (
                "heldOutTasks",
                "validRuns",
                "invalidAttempts",
                "invalidBlocks",
                "validPairedBlocks",
                "annotations",
                "doubleReviewedAnnotations",
                "judgeDisagreements",
                "systematicNewFailureClasses",
            )
        },
        "invalidReasons": [],
        "gates": [_gate("absolute-safety", "pass", "zero_violations")],
        "diagnostics": {
            **{
                field: (
                    0
                    if field in {"mcnemarB", "mcnemarC", "compactSubsetTaskCount"}
                    else None
                )
                for field in (
                    "qualityDifference",
                    "qualityLowerBound",
                    "qualityUpperBound",
                    "mcnemarB",
                    "mcnemarC",
                    "mcnemarPValue",
                    "compactSubsetTaskCount",
                    "compactSubsetMedian",
                    "compactSubsetUpperBound",
                    "blendedTokenRatio",
                    "medianWallRatio",
                    "harmfulTriggerRate",
                    "repeatedSettledPhaseRate",
                    "criticalOmissionRate",
                    "actionableNextActionRate",
                    "hostEvidenceRecall",
                    "judgeAgreement",
                )
            },
            "armComparison": {
                "runCount": 0,
                "stock": {
                    "tokens": {field: 0 for field in TOKEN_FIELDS},
                    "benchmarkScoreMean": 0.0,
                    "solvedRuns": 0,
                    "wallTimeMs": 0,
                    "contextUsage": {
                        "tokenUsageSamples": 0,
                        "modelContextWindow": 0,
                        "meanInputTokensPerSample": 0.0,
                        "peakInputTokens": 0,
                        "peakWindowUtilization": 0.0,
                    },
                },
                "agenticCompact": {
                    "tokens": {field: 0 for field in TOKEN_FIELDS},
                    "benchmarkScoreMean": 0.0,
                    "solvedRuns": 0,
                    "wallTimeMs": 0,
                    "contextUsage": {
                        "tokenUsageSamples": 0,
                        "modelContextWindow": 0,
                        "meanInputTokensPerSample": 0.0,
                        "peakInputTokens": 0,
                        "peakWindowUtilization": 0.0,
                    },
                },
                "delta": {
                    "tokens": {field: 0 for field in TOKEN_FIELDS},
                    "benchmarkScoreMean": 0.0,
                    "solvedRuns": 0,
                    "wallTimeMs": 0,
                    "contextUsage": {
                        "tokenUsageSamples": 0,
                        "meanInputTokensPerSample": 0.0,
                        "peakInputTokens": 0,
                        "peakWindowUtilization": 0.0,
                    },
                },
                "ratio": {
                    "tokens": {field: None for field in TOKEN_FIELDS},
                    "benchmarkScoreMean": None,
                    "wallTimeMs": 0.0,
                    "contextUsage": {
                        "tokenUsageSamples": 0.0,
                        "meanInputTokensPerSample": 0.0,
                        "peakInputTokens": 0.0,
                        "peakWindowUtilization": 0.0,
                    },
                },
            },
            "compactionAccounting": {
                "stockNative": {"compactions": 0, "tasks": 0},
                "candidateProactive": {
                    "requests": 0,
                    "requestTasks": 0,
                    "scheduledRequests": 0,
                    "transitions": 0,
                    "transitionTasks": 0,
                    "measuredTransitions": 0,
                    "preCompactionInputTokens": 0,
                    "postCompactionInputTokens": 0,
                    "activeInputTokensRemoved": 0,
                    "postToPreInputRatio": None,
                },
                "candidateResidualNative": {"compactions": 0, "tasks": 0},
            },
        },
        "unavailableMetrics": [],
        "overallPass": True,
    }


def _passing_matrix(
    manifest: dict[str, Any], schemas: dict[str, RestrictedSchema]
) -> tuple[dict[tuple[str, str, str, int], dict[str, Any]], list[dict[str, Any]]]:
    candidate = "d" * 40
    manifest_sha256 = "c" * 64
    tasks = {task["taskId"]: task for task in manifest["tasks"]}
    runs: dict[tuple[str, str, str, int], dict[str, Any]] = {}
    annotations: list[dict[str, Any]] = []
    for key in expected_result_keys(manifest):
        regime, arm, task_id, repetition = key
        run = _synthetic_run(manifest)
        run["runId"] = hashlib.sha256(canonical_json(key).encode("ascii")).hexdigest()
        run["taskId"] = task_id
        run["benchmark"] = tasks[task_id]["benchmark"]
        run["arm"] = arm
        run["repetition"] = repetition
        run["contextRegime"] = regime
        run["resolvedContextLimit"] = (
            manifest["runtime"]["fixedFallbackLimit"]
            if regime == "fixed-fallback"
            else manifest["runtime"]["noForcedLimit"]
        )
        run["configHashes"]["taskManifestSha256"] = manifest_sha256
        run["wallTimeMs"] = 100
        run["tokens"].update(
            {
                "inputTokens": 100,
                "cachedInputTokens": 0,
                "outputTokens": 10,
                "reasoningOutputTokens": 5,
                "nonCachedInputTokens": 100,
                "blendedTokens": 110,
            }
        )
        if arm != "stock":
            run["configHashes"].update(
                {
                    "binarySha256": hashlib.sha256(
                        f"{arm}:binary".encode()
                    ).hexdigest(),
                    "pluginSha256": hashlib.sha256(
                        f"{arm}:plugin".encode()
                    ).hexdigest(),
                    "surfaceSha256": hashlib.sha256(
                        f"{arm}:surface".encode()
                    ).hexdigest(),
                }
            )
        if arm == "v0.1":
            run["configHashes"].update(
                {
                    field: manifest["v01Baseline"][field]
                    for field in (
                        "binarySha256",
                        "pluginSha256",
                        "surfaceSha256",
                        "capabilityRecordSha256",
                    )
                }
            )
        if arm == "surface-sham":
            run["configHashes"]["surfaceSha256"] = "3" * 64
        if arm == "v0.2":
            run["configHashes"].update(
                {
                    "binarySha256": "1" * 64,
                    "pluginSha256": "2" * 64,
                    "surfaceSha256": "3" * 64,
                    "capabilityRecordSha256": "4" * 64,
                }
            )
            run["wallTimeMs"] = 80
            run["tokens"].update(
                {"inputTokens": 80, "nonCachedInputTokens": 80, "blendedTokens": 90}
            )
            run["observer"].update(
                {
                    "tokenUsageSamples": 2,
                    "peakActiveInputTokens": 50,
                }
            )
            run["counters"].update(
                {
                    "turnsStarted": 3,
                    "itemsCompleted": 3,
                    "toolItemsCompleted": 1,
                    "contextCompactions": 1,
                    "agenticRequests": 1,
                    "scheduledAgenticRequests": 1,
                    "agenticTransitions": 1,
                }
            )
            hashes = [
                hashlib.sha256(f"{run['runId']}:{index}".encode()).hexdigest()
                for index in range(10)
            ]
            run["transitions"] = [
                {
                    "ordinal": 1,
                    "sourceTurnHash": hashes[0],
                    "sourceItemHash": hashes[1],
                    "sourcePosition": 0,
                    "preCompactionInputTokens": 50,
                    "preCompactionUsageEventHash": hashes[8],
                    "compactionTurnHash": hashes[2],
                    "compactionItemHash": hashes[3],
                    "compactionPosition": 1,
                    "continuationTurnHash": hashes[4],
                    "continuationPosition": 2,
                    "postCompactionInputTokens": 30,
                    "postCompactionUsageEventHash": hashes[9],
                    "continuationKind": "auto",
                    "terminalState": "cooldown",
                    "journalSha256": hashes[5],
                    "checkpointSha256": hashes[6],
                    "continuityViewSha256": hashes[7],
                    "hostEvidenceMatch": True,
                    "processSurvived": True,
                    "sameThread": True,
                    "injectionCount": 1,
                    "continuationCount": 1,
                }
            ]
            if regime == "fixed-fallback":
                run["semanticAvailability"] = {
                    "trigger": True,
                    "checkpoint": True,
                    "continuation": True,
                }
        elif arm == "surface-sham":
            run["counters"]["agenticRequests"] = 1
            run["rejections"] = [
                {"reasonCode": "shared_app_server_unavailable", "count": 1}
            ]
        validate_run(run, schemas["run"], manifest, tasks, manifest_sha256, candidate)
        runs[key] = run

        if arm != "v0.2" or regime != "fixed-fallback":
            continue
        for stream, kind, position in (
            ("trigger", "actual-request", 0),
            ("checkpoint", "checkpoint", 1),
            ("continuation", "continuation", 2),
        ):
            annotation = _synthetic_annotation(manifest, run)
            annotation["annotationId"] = hashlib.sha256(
                f"{run['runId']}:{stream}".encode()
            ).hexdigest()
            annotation["stream"] = stream
            annotation["sourceAnchor"] = {
                "kind": kind,
                "turnHash": run["transitions"][0]["sourceTurnHash"],
                "itemHash": run["transitions"][0]["sourceItemHash"],
                "position": position,
            }
            annotation["promptSha256"] = manifest["judge"]["promptSha256"][stream]
            annotation["attributes"].update(
                {
                    "criticalContradiction": False,
                    "criticalOmission": False,
                    "nextActionActionable": True,
                    "hostEvidenceMatch": True,
                    "repeatedSettledPhase": False,
                }
            )
            validate_annotation(
                annotation, schemas["annotation"], manifest, run, candidate
            )
            annotations.append(annotation)
    selected = expected_double_review_ids(
        annotations, manifest["seeds"]["judgeSamplingSeed"]
    )
    for annotation in annotations:
        if annotation["annotationId"] in selected:
            annotation.update(
                {
                    "doubleReviewed": True,
                    "secondary": "KEEP",
                    "adjudicated": "KEEP",
                }
            )
    runs_by_id = {run["runId"]: run for run in runs.values()}
    for annotation in annotations:
        validate_annotation(
            annotation,
            schemas["annotation"],
            manifest,
            runs_by_id[annotation["runId"]],
            candidate,
        )
    return runs, annotations


def _replace(value: Any, path: tuple[Any, ...], replacement: Any) -> Any:
    changed = json.loads(canonical_json(value))
    target = changed
    for component in path[:-1]:
        target = target[component]
    target[path[-1]] = replacement
    return changed


def _exercise_result_tree(
    manifest: dict[str, Any],
    runs: dict[tuple[str, str, str, int], dict[str, Any]],
    annotations: list[dict[str, Any]],
    schemas: dict[str, RestrictedSchema],
) -> None:
    candidate = "d" * 40
    with tempfile.TemporaryDirectory(prefix="agentic-compact-eval-") as directory:
        temporary = Path(directory)
        manifest_path = temporary / "v0.2.json"
        manifest_path.write_text(canonical_json(manifest) + "\n", encoding="utf-8")
        manifest_sha256 = sha256_file(manifest_path)
        root = temporary / "results"
        for key, run in runs.items():
            run["configHashes"]["taskManifestSha256"] = manifest_sha256
            path = result_path(root, candidate, key)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(canonical_json(run) + "\n", encoding="utf-8")
        for annotation in annotations:
            path = (
                root
                / candidate
                / "annotations"
                / annotation["contextRegime"]
                / annotation["arm"]
                / annotation["stream"]
                / annotation["taskId"]
                / f"rep-{annotation['repetition']}-{annotation['ordinal']}.json"
            )
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(canonical_json(annotation) + "\n", encoding="utf-8")
        validated_runs, validated_annotations = validate_result_tree(
            root, candidate, manifest_path, schemas
        )
        assert len(validated_runs) == 380 and len(validated_annotations) == 450
        unexpected = root / candidate / "unexpected.json"
        unexpected.write_text("{}\n", encoding="utf-8")
        _expect_invalid(
            lambda: validate_result_tree(root, candidate, manifest_path, schemas)
        )
        unexpected.unlink()

        rehearsal_keys = [
            key
            for key in runs
            if key[0] == "fixed-fallback" and key[2:] == ("held-00", 1)
        ]
        assert len(rehearsal_keys) == 4
        for key in rehearsal_keys:
            rerun = json.loads(canonical_json(runs[key]))
            rerun["attempt"] = 2
            rerun["priorInvalidBlocks"] = [
                {"reasonCode": "observer_disconnect", "count": 1}
            ]
            result_path(root, candidate, key).write_text(
                canonical_json(rerun) + "\n", encoding="utf-8"
            )
        rerun_runs, rerun_annotations = validate_result_tree(
            root, candidate, manifest_path, schemas
        )
        assert validate_block_attempts(rerun_runs) == (4, 1)
        rerun_summary = derive_summary(
            rerun_runs,
            rerun_annotations,
            manifest,
            manifest_sha256,
            candidate,
        )
        assert rerun_summary["counts"]["invalidAttempts"] == 4
        assert rerun_summary["counts"]["invalidBlocks"] == 1
        assert rerun_summary["invalidReasons"] == [
            {"reason": "observer_disconnect", "count": 1}
        ]
        validate_summary(rerun_summary, schemas["summary"])

        partial = json.loads(canonical_json(rerun_runs[rehearsal_keys[0]]))
        partial["attempt"] = 1
        partial["priorInvalidBlocks"] = []
        result_path(root, candidate, rehearsal_keys[0]).write_text(
            canonical_json(partial) + "\n", encoding="utf-8"
        )
        _expect_invalid(
            lambda: validate_result_tree(root, candidate, manifest_path, schemas)
        )


def _exercise_schema(
    schema: RestrictedSchema,
    valid: dict[str, Any],
    *,
    enum_path: tuple[Any, ...],
    pattern_path: tuple[Any, ...],
    range_path: tuple[Any, ...],
) -> None:
    schema.validate(valid)
    missing = json.loads(canonical_json(valid))
    del missing[next(iter(valid))]
    _expect_invalid(lambda: schema.validate(missing))
    _expect_invalid(
        lambda: schema.validate(_replace(valid, ("schemaVersion",), "wrong-type"))
    )
    unknown = json.loads(canonical_json(valid))
    unknown["unknownField"] = True
    _expect_invalid(lambda: schema.validate(unknown))
    _expect_invalid(lambda: schema.validate(_replace(valid, enum_path, "outside-enum")))
    _expect_invalid(
        lambda: schema.validate(_replace(valid, pattern_path, "invalid value"))
    )
    _expect_invalid(lambda: schema.validate(_replace(valid, range_path, -1)))


def _synthetic_context_telemetry() -> list[dict[str, Any]]:
    thread_hash = "1" * 64
    turn_hash = "2" * 64
    item_hash = "3" * 64

    def usage(monotonic_ms: int, input_tokens: int, total_input: int) -> dict[str, Any]:
        return {
            "method": "thread/tokenUsage/updated",
            "monotonicMs": monotonic_ms,
            "threadHash": thread_hash,
            "turnHash": turn_hash,
            "tokenUsage": {
                "modelContextWindow": 258_400,
                "last": {"inputTokens": input_tokens},
                "total": {"inputTokens": total_input},
            },
        }

    return [
        usage(1, 100, 100),
        usage(2, 100, 100),
        {
            "method": "item/started",
            "monotonicMs": 3,
            "threadHash": thread_hash,
            "turnHash": turn_hash,
            "itemHash": item_hash,
            "itemType": "contextCompaction",
        },
        usage(4, 0, 100),
        {
            "method": "item/completed",
            "monotonicMs": 5,
            "threadHash": thread_hash,
            "turnHash": turn_hash,
            "itemHash": item_hash,
            "itemType": "contextCompaction",
        },
        usage(6, 40, 140),
    ]


def run_self_test() -> None:
    run_reference_transition_selftest()
    schemas = load_schemas()
    telemetry = _synthetic_context_telemetry()
    telemetry_summary = analyze_context_telemetry(telemetry)
    assert telemetry_summary["tokenUsageSamples"] == 2
    assert telemetry_summary["finalReportedInputTokens"] == 140
    assert telemetry_summary["peakActiveInputTokens"] == 100
    assert telemetry_summary["compactions"][0]["preCompactionInputTokens"] == 100
    assert telemetry_summary["compactions"][0]["postCompactionInputTokens"] == 40
    broken_telemetry_total = json.loads(canonical_json(telemetry))
    broken_telemetry_total[-1]["tokenUsage"]["total"]["inputTokens"] = 139
    _expect_invalid(lambda: analyze_context_telemetry(broken_telemetry_total))
    broken_telemetry_method = json.loads(canonical_json(telemetry))
    broken_telemetry_method[0]["method"] = "turn/started"
    _expect_invalid(lambda: analyze_context_telemetry(broken_telemetry_method))
    manifest = _synthetic_manifest()
    tasks = validate_manifest(manifest, schemas["manifest"])
    assert len(tasks) == 70
    _expect_invalid(
        lambda: validate_manifest(
            _replace(manifest, ("selectionRule",), "stale-selection-rule"),
            schemas["manifest"],
        )
    )
    _expect_invalid(
        lambda: validate_manifest(
            _replace(manifest, ("corpusProvenanceSha256",), "f" * 64),
            schemas["manifest"],
        )
    )
    wrong_reset2_source = json.loads(canonical_json(manifest))
    wrong_reset2_source["tasks"][-1]["benchmark"] = "long-range-controlled"
    wrong_reset2_source["tasks"][-1]["selectionEvidence"].append(
        "public-long-horizon"
    )
    _expect_invalid(
        lambda: validate_manifest(wrong_reset2_source, schemas["manifest"])
    )
    run = _synthetic_run(manifest)
    annotation = _synthetic_annotation(manifest, run)
    summary = _synthetic_summary(manifest)
    _exercise_schema(
        schemas["manifest"],
        manifest,
        enum_path=("tasks", 0, "benchmark"),
        pattern_path=("tasks", 0, "taskId"),
        range_path=("seeds", "armOrderSeed"),
    )
    _exercise_schema(
        schemas["run"],
        run,
        enum_path=("arm",),
        pattern_path=("runId",),
        range_path=("repetition",),
    )
    _exercise_schema(
        schemas["annotation"],
        annotation,
        enum_path=("stream",),
        pattern_path=("annotationId",),
        range_path=("ordinal",),
    )
    _exercise_schema(
        schemas["summary"],
        summary,
        enum_path=("gates", 0, "name"),
        pattern_path=("candidateCommit",),
        range_path=("counts", "validRuns"),
    )
    validate_run(run, schemas["run"], manifest, tasks, "c" * 64, "d" * 40)
    broken_run = _replace(run, ("tokens", "nonCachedInputTokens"), 79)
    _expect_invalid(
        lambda: validate_run(
            broken_run, schemas["run"], manifest, tasks, "c" * 64, "d" * 40
        )
    )
    broken_score = _replace(run, ("benchmarkScore",), 0.5)
    _expect_invalid(
        lambda: validate_run(
            broken_score, schemas["run"], manifest, tasks, "c" * 64, "d" * 40
        )
    )
    broken_trace = _replace(run, ("observer", "tokenUsageSamples"), 0)
    _expect_invalid(
        lambda: validate_run(
            broken_trace, schemas["run"], manifest, tasks, "c" * 64, "d" * 40
        )
    )
    annotation_run = json.loads(canonical_json(run))
    annotation_run["semanticAvailability"]["trigger"] = True
    validate_annotation(
        annotation, schemas["annotation"], manifest, annotation_run, "d" * 40
    )
    broken_annotation = _replace(
        annotation, ("preferredAnchor",), annotation["sourceAnchor"]
    )
    _expect_invalid(
        lambda: validate_annotation(
            broken_annotation, schemas["annotation"], manifest, annotation_run, "d" * 40
        )
    )
    harmful_annotation = json.loads(canonical_json(annotation))
    harmful_annotation.update(
        {"primary": "DELETE", "final": "DELETE", "taxonomy": "harmful_trigger"}
    )
    validate_annotation(
        harmful_annotation, schemas["annotation"], manifest, annotation_run, "d" * 40
    )
    harmful_annotation["taxonomy"] = None
    _expect_invalid(
        lambda: validate_annotation(
            harmful_annotation,
            schemas["annotation"],
            manifest,
            annotation_run,
            "d" * 40,
        )
    )
    passing_runs, passing_annotations = _passing_matrix(manifest, schemas)
    derived = derive_summary(
        passing_runs, passing_annotations, manifest, "c" * 64, "d" * 40
    )
    validate_summary(derived, schemas["summary"])
    assert derived["overallPass"] and derived["counts"]["validRuns"] == 380
    comparison = derived["diagnostics"]["armComparison"]
    assert comparison["runCount"] == 150
    assert comparison["stock"]["tokens"]["blendedTokens"] == 16_500
    assert comparison["agenticCompact"]["tokens"]["blendedTokens"] == 13_500
    assert comparison["agenticCompact"]["benchmarkScoreMean"] == 1.0
    assert comparison["delta"]["wallTimeMs"] == -3_000
    assert comparison["ratio"]["tokens"]["blendedTokens"] == 13_500 / 16_500
    assert comparison["stock"]["contextUsage"]["tokenUsageSamples"] == 150
    assert comparison["agenticCompact"]["contextUsage"]["tokenUsageSamples"] == 300
    assert comparison["agenticCompact"]["contextUsage"][
        "meanInputTokensPerSample"
    ] == 40.0
    assert comparison["ratio"]["contextUsage"]["peakInputTokens"] == 0.5
    tampered_comparison = json.loads(canonical_json(derived))
    tampered_comparison["diagnostics"]["armComparison"]["delta"]["tokens"][
        "inputTokens"
    ] = 1
    _expect_invalid(
        lambda: validate_summary(tampered_comparison, schemas["summary"])
    )
    tampered_context = json.loads(canonical_json(derived))
    tampered_context["diagnostics"]["armComparison"]["stock"]["contextUsage"][
        "meanInputTokensPerSample"
    ] = 1.0
    _expect_invalid(lambda: validate_summary(tampered_context, schemas["summary"]))
    accounting = derived["diagnostics"]["compactionAccounting"]
    assert accounting == {
        "stockNative": {"compactions": 0, "tasks": 0},
        "candidateProactive": {
            "requests": 150,
            "requestTasks": 50,
            "scheduledRequests": 150,
            "transitions": 150,
            "transitionTasks": 50,
            "measuredTransitions": 150,
            "preCompactionInputTokens": 7_500,
            "postCompactionInputTokens": 4_500,
            "activeInputTokensRemoved": 3_000,
            "postToPreInputRatio": 0.6,
        },
        "candidateResidualNative": {"compactions": 0, "tasks": 0},
    }
    native_runs = dict(passing_runs)
    native_key = ("fixed-fallback", "stock", "held-00", 1)
    native_run = json.loads(canonical_json(native_runs[native_key]))
    native_run["counters"]["contextCompactions"] = 1
    native_run["counters"]["unattributedCompactions"] = 1
    validate_run(native_run, schemas["run"], manifest, tasks, "c" * 64, "d" * 40)
    native_runs[native_key] = native_run
    residual_key = ("fixed-fallback", "v0.2", "held-01", 1)
    residual_run = json.loads(canonical_json(native_runs[residual_key]))
    residual_run["counters"]["contextCompactions"] = 2
    residual_run["counters"]["unattributedCompactions"] = 1
    validate_run(residual_run, schemas["run"], manifest, tasks, "c" * 64, "d" * 40)
    native_runs[residual_key] = residual_run
    native_summary = derive_summary(
        native_runs, passing_annotations, manifest, "c" * 64, "d" * 40
    )
    assert native_summary["diagnostics"]["compactionAccounting"]["stockNative"] == {
        "compactions": 1,
        "tasks": 1,
    }
    assert native_summary["diagnostics"]["compactionAccounting"][
        "candidateResidualNative"
    ] == {"compactions": 1, "tasks": 1}
    tampered_accounting = json.loads(canonical_json(derived))
    tampered_accounting["diagnostics"]["compactionAccounting"]["candidateProactive"][
        "scheduledRequests"
    ] = 149
    _expect_invalid(lambda: validate_summary(tampered_accounting, schemas["summary"]))
    tampered_contraction = json.loads(canonical_json(derived))
    tampered_contraction["diagnostics"]["compactionAccounting"][
        "candidateProactive"
    ]["activeInputTokensRemoved"] = 1
    _expect_invalid(
        lambda: validate_summary(tampered_contraction, schemas["summary"])
    )
    broken_summary = json.loads(canonical_json(derived))
    broken_summary["overallPass"] = False
    _expect_invalid(lambda: validate_summary(broken_summary, schemas["summary"]))
    tampered_summary = json.loads(canonical_json(derived))
    token_gate = next(
        gate
        for gate in tampered_summary["gates"]
        if gate["name"] == "overall-blended-token-ratio"
    )
    token_gate["estimate"] = 0.5
    tampered_summary["diagnostics"]["blendedTokenRatio"] = 0.5
    _expect_invalid(lambda: validate_summary(tampered_summary, schemas["summary"]))
    tampered_mcnemar = json.loads(canonical_json(derived))
    tampered_mcnemar["diagnostics"]["mcnemarPValue"] = 0.5
    _expect_invalid(lambda: validate_summary(tampered_mcnemar, schemas["summary"]))
    observed_failure_runs = dict(passing_runs)
    observed_key = ("fixed-fallback", "v0.2", "held-00", 1)
    observed_failure = json.loads(canonical_json(observed_failure_runs[observed_key]))
    observed_failure["transitions"][0]["hostEvidenceMatch"] = False
    observed_failure["transitions"][0]["sameThread"] = False
    _expect_invalid(
        lambda: validate_run(
            observed_failure, schemas["run"], manifest, tasks, "c" * 64, "d" * 40
        )
    )
    observed_failure["safety"]["crossThreadActions"] = 1
    validate_run(observed_failure, schemas["run"], manifest, tasks, "c" * 64, "d" * 40)
    observed_failure_runs[observed_key] = observed_failure
    failure_summary = derive_summary(
        observed_failure_runs, passing_annotations, manifest, "c" * 64, "d" * 40
    )
    failure_gates = {gate["name"]: gate for gate in failure_summary["gates"]}
    assert failure_gates["absolute-safety"]["status"] == "fail"
    assert failure_gates["host-evidence-recall"]["status"] == "fail"
    _exercise_result_tree(manifest, passing_runs, passing_annotations, schemas)
    assert len(expected_result_keys(manifest)) == 380
    manifest_without_diagnostic = json.loads(canonical_json(manifest))
    manifest_without_diagnostic["runtime"]["noForcedLimit"] = None
    assert len(expected_result_keys(manifest_without_diagnostic)) == 340

    broken_manifest = json.loads(canonical_json(manifest))
    broken_manifest["runtime"]["model"] = "gpt-5.6-sol"
    _expect_invalid(lambda: validate_manifest(broken_manifest, schemas["manifest"]))
    unconfirmed_limit = json.loads(canonical_json(manifest))
    unconfirmed_limit["runtime"]["fixedFallbackLimit"] = 65_536
    _expect_invalid(lambda: validate_manifest(unconfirmed_limit, schemas["manifest"]))
    mismatched_evidence = json.loads(canonical_json(manifest))
    mismatched_evidence["tasks"][0]["selectionEvidence"] = ["public-long-horizon"]
    _expect_invalid(lambda: validate_manifest(mismatched_evidence, schemas["manifest"]))
    duplicate_context_evidence = json.loads(canonical_json(manifest))
    duplicate_context_evidence["tasks"][20]["selectionEvidence"].append(
        "active-context-pressure"
    )
    _expect_invalid(
        lambda: validate_manifest(duplicate_context_evidence, schemas["manifest"])
    )
    ineligible_diagnostic = json.loads(canonical_json(manifest))
    ineligible_diagnostic["tasks"][20]["semanticBundleEligible"] = False
    ineligible_diagnostic["tasks"][20]["selectionEvidence"].remove(
        "semantic-bundle"
    )
    _expect_invalid(
        lambda: validate_manifest(ineligible_diagnostic, schemas["manifest"])
    )
    _expect_invalid(
        lambda: RestrictedSchema({"type": "object", "format": "unsupported"})
    )
    _expect_invalid(
        lambda: RestrictedSchema(
            {
                "type": "object",
                "required": ["missing"],
                "properties": {},
            }
        )
    )
    _expect_invalid(lambda: validate_privacy({"content": "redacted"}))
    _expect_invalid(lambda: validate_privacy({"safe": "/private/location"}))
    _expect_invalid(lambda: validate_privacy({"safe": "embedded /private/location"}))
    _expect_invalid(lambda: validate_privacy({"safe": "~/private/location"}))
    _expect_invalid(lambda: validate_privacy({"safe": r"C:\\private\\location"}))
    _expect_invalid(lambda: validate_privacy({"safe": r"\\server\private\location"}))
    _expect_invalid(lambda: validate_privacy({"sourceTurnId": "unhashed"}))

    block_runs = {
        ("fixed-fallback", arm, "held-00", 1): {
            "attempt": 2,
            "priorInvalidBlocks": [{"reasonCode": "observer_disconnect", "count": 1}],
        }
        for arm in ("stock", "v0.2")
    }
    assert validate_block_attempts(block_runs) == (2, 1)
    block_runs[("fixed-fallback", "v0.2", "held-00", 1)]["attempt"] = 1
    _expect_invalid(lambda: validate_block_attempts(block_runs))

    assert majority([True, False, True])
    _expect_invalid(lambda: majority([True, False]))
    differences = [0.0] * 49 + [-1.0]
    first = bootstrap_mean(differences, "13", "quality")
    second = bootstrap_mean(differences, "13", "quality")
    assert first == second and len(first) == 10_000
    assert nearest_rank([1.0, 2.0, 3.0, 4.0], 0.05) == 1.0
    assert nearest_rank([1.0, 2.0, 3.0, 4.0], 0.95) == 4.0
    assert bootstrap_median([0.9] * 15, "13") == [0.9] * 10_000
    assert mcnemar([True, True, True], [False, False, False]) == (3, 0, 0.25)
