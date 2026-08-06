"""Deterministic statistics and summary validation for v0.2 evaluation."""

from __future__ import annotations

import hashlib
import math
import random
import statistics
from typing import Any

from eval_contract import (
    ArtifactValidationError,
    EXPECTED_THRESHOLDS,
    RestrictedSchema,
    canonical_json,
    sha256_json,
    validate_block_attempts,
    validate_privacy,
)

TOKEN_FIELDS = (
    "inputTokens",
    "cachedInputTokens",
    "cacheWriteTokens",
    "outputTokens",
    "reasoningOutputTokens",
    "nonCachedInputTokens",
    "blendedTokens",
)


def majority(values: list[bool]) -> bool:
    if len(values) != 3:
        raise ArtifactValidationError(
            "majority aggregation requires exactly three repetitions"
        )
    return sum(values) >= 2


def arm_comparison(
    stock_runs: list[dict[str, Any]], candidate_runs: list[dict[str, Any]]
) -> dict[str, Any]:
    if not stock_runs or len(stock_runs) != len(candidate_runs):
        raise ArtifactValidationError("arm comparison requires paired runs")

    def aggregate(runs: list[dict[str, Any]]) -> dict[str, Any]:
        usage_samples = sum(run["observer"]["tokenUsageSamples"] for run in runs)
        input_tokens = sum(run["tokens"]["inputTokens"] for run in runs)
        model_context_window = _unique(
            [run["observer"]["modelContextWindow"] for run in runs],
            "observer.modelContextWindow",
        )
        peak_input_tokens = max(
            run["observer"]["peakActiveInputTokens"] for run in runs
        )
        return {
            "tokens": {
                field: sum(run["tokens"][field] for run in runs)
                for field in TOKEN_FIELDS
            },
            "benchmarkScoreMean": statistics.mean(
                run["benchmarkScore"] for run in runs
            ),
            "solvedRuns": sum(run["solved"] for run in runs),
            "wallTimeMs": sum(run["wallTimeMs"] for run in runs),
            "contextUsage": {
                "tokenUsageSamples": usage_samples,
                "modelContextWindow": model_context_window,
                "meanInputTokensPerSample": input_tokens / usage_samples,
                "peakInputTokens": peak_input_tokens,
                "peakWindowUtilization": peak_input_tokens / model_context_window,
            },
        }

    stock = aggregate(stock_runs)
    candidate = aggregate(candidate_runs)
    delta = {
        "tokens": {
            field: candidate["tokens"][field] - stock["tokens"][field]
            for field in TOKEN_FIELDS
        },
        "benchmarkScoreMean": (
            candidate["benchmarkScoreMean"] - stock["benchmarkScoreMean"]
        ),
        "solvedRuns": candidate["solvedRuns"] - stock["solvedRuns"],
        "wallTimeMs": candidate["wallTimeMs"] - stock["wallTimeMs"],
        "contextUsage": {
            field: candidate["contextUsage"][field] - stock["contextUsage"][field]
            for field in (
                "tokenUsageSamples",
                "meanInputTokensPerSample",
                "peakInputTokens",
                "peakWindowUtilization",
            )
        },
    }
    ratio = {
        "tokens": {
            field: (
                candidate["tokens"][field] / stock["tokens"][field]
                if stock["tokens"][field] > 0
                else None
            )
            for field in TOKEN_FIELDS
        },
        "benchmarkScoreMean": (
            candidate["benchmarkScoreMean"] / stock["benchmarkScoreMean"]
            if stock["benchmarkScoreMean"] > 0
            else None
        ),
        "wallTimeMs": candidate["wallTimeMs"] / stock["wallTimeMs"],
        "contextUsage": {
            field: candidate["contextUsage"][field] / stock["contextUsage"][field]
            for field in (
                "tokenUsageSamples",
                "meanInputTokensPerSample",
                "peakInputTokens",
                "peakWindowUtilization",
            )
        },
    }
    return {
        "runCount": len(stock_runs),
        "stock": stock,
        "agenticCompact": candidate,
        "delta": delta,
        "ratio": ratio,
    }


def nearest_rank(values: list[float], probability: float) -> float:
    if not values or not 0 < probability <= 1:
        raise ArtifactValidationError("nearest-rank input is invalid")
    ordered = sorted(values)
    return ordered[math.ceil(probability * len(ordered)) - 1]


def bootstrap_mean(
    differences: list[float], seed: str, stream: str, samples: int = 10_000
) -> list[float]:
    if not differences or samples < 1:
        raise ArtifactValidationError("bootstrap input is empty")
    generator = random.Random(f"{seed}:{stream}")
    size = len(differences)
    return [
        sum(differences[generator.randrange(size)] for _ in range(size)) / size
        for _ in range(samples)
    ]


def bootstrap_median(
    ratios: list[float], seed: str, samples: int = 10_000
) -> list[float]:
    if not ratios or samples < 1:
        raise ArtifactValidationError("bootstrap input is empty")
    generator = random.Random(f"{seed}:compact-subset")
    size = len(ratios)
    return [
        statistics.median(ratios[generator.randrange(size)] for _ in range(size))
        for _ in range(samples)
    ]


def mcnemar(stock: list[bool], candidate: list[bool]) -> tuple[int, int, float]:
    if len(stock) != len(candidate):
        raise ArtifactValidationError("McNemar inputs are not paired")
    b = sum(left and not right for left, right in zip(stock, candidate, strict=True))
    c = sum(not left and right for left, right in zip(stock, candidate, strict=True))
    discordant = b + c
    if discordant == 0:
        return b, c, 1.0
    tail = sum(math.comb(discordant, k) for k in range(min(b, c) + 1)) / (2**discordant)
    return b, c, min(1.0, 2 * tail)


def _gate(
    name: str,
    status: str,
    reason: str,
    *,
    numerator: int | None = None,
    denominator: int | None = None,
    estimate: float | None = None,
    lower: float | None = None,
    upper: float | None = None,
) -> dict[str, Any]:
    return {
        "name": name,
        "numerator": numerator,
        "denominator": denominator,
        "estimate": estimate,
        "lowerBound": lower,
        "upperBound": upper,
        "status": status,
        "reason": reason,
    }


def _threshold_gate(
    name: str,
    estimate: float,
    threshold: float,
    *,
    at_most: bool,
    numerator: int,
    denominator: int,
) -> dict[str, Any]:
    passed = estimate <= threshold if at_most else estimate >= threshold
    return _gate(
        name,
        "pass" if passed else "fail",
        "threshold_met" if passed else "threshold_exceeded",
        numerator=numerator,
        denominator=denominator,
        estimate=estimate,
    )


def _unique(values: list[Any], field: str) -> Any:
    canonical = {canonical_json(value) for value in values}
    if len(canonical) != 1:
        raise ArtifactValidationError(f"normalized results disagree on {field}")
    return values[0]


def _counter_support(runs: list[dict[str, Any]], counter: str) -> tuple[int, int]:
    return (
        sum(run["counters"][counter] for run in runs),
        len({run["taskId"] for run in runs if run["counters"][counter] > 0}),
    )


def _compaction_accounting(
    stock_runs: list[dict[str, Any]], candidate_runs: list[dict[str, Any]]
) -> dict[str, Any]:
    stock_compactions, stock_tasks = _counter_support(
        stock_runs, "unattributedCompactions"
    )
    requests, request_tasks = _counter_support(candidate_runs, "agenticRequests")
    scheduled, _ = _counter_support(candidate_runs, "scheduledAgenticRequests")
    transitions, transition_tasks = _counter_support(
        candidate_runs, "agenticTransitions"
    )
    successful_transitions = [
        transition
        for run in candidate_runs
        for transition in run["transitions"]
        if transition["terminalState"] == "cooldown"
    ]
    pre_compaction_tokens = sum(
        transition["preCompactionInputTokens"]
        for transition in successful_transitions
    )
    post_compaction_tokens = sum(
        transition["postCompactionInputTokens"]
        for transition in successful_transitions
    )
    residual_compactions, residual_tasks = _counter_support(
        candidate_runs, "unattributedCompactions"
    )
    return {
        "stockNative": {"compactions": stock_compactions, "tasks": stock_tasks},
        "candidateProactive": {
            "requests": requests,
            "requestTasks": request_tasks,
            "scheduledRequests": scheduled,
            "transitions": transitions,
            "transitionTasks": transition_tasks,
            "measuredTransitions": len(successful_transitions),
            "preCompactionInputTokens": pre_compaction_tokens,
            "postCompactionInputTokens": post_compaction_tokens,
            "activeInputTokensRemoved": (
                pre_compaction_tokens - post_compaction_tokens
            ),
            "postToPreInputRatio": (
                post_compaction_tokens / pre_compaction_tokens
                if pre_compaction_tokens > 0
                else None
            ),
        },
        "candidateResidualNative": {
            "compactions": residual_compactions,
            "tasks": residual_tasks,
        },
    }


def expected_double_review_ids(
    annotations: list[dict[str, Any]], seed: int
) -> set[str]:
    selected: set[str] = set()
    for stream in ("trigger", "checkpoint", "continuation"):
        ordered = sorted(
            (item for item in annotations if item["stream"] == stream),
            key=lambda item: (
                item["taskId"],
                item["repetition"],
                item["ordinal"],
                item["annotationId"],
            ),
        )
        if not ordered:
            continue
        count = max(1, math.ceil(0.10 * len(ordered)))
        generator = random.Random(f"{seed}:{stream}:double-review")
        selected.update(
            ordered[index]["annotationId"]
            for index in generator.sample(range(len(ordered)), count)
        )
    return selected


def derive_summary(
    runs: dict[tuple[str, str, str, int], dict[str, Any]],
    annotations: list[dict[str, Any]],
    manifest: dict[str, Any],
    manifest_sha256: str,
    candidate: str,
) -> dict[str, Any]:
    held_out = [task for task in manifest["tasks"] if task["role"] == "held-out"]
    task_ids = [task["taskId"] for task in held_out]
    stock_majority: list[bool] = []
    candidate_majority: list[bool] = []
    compact_task_ratios: list[float] = []
    wall_task_ratios: list[float] = []
    stock_blended = 0
    candidate_blended = 0
    main_candidate_runs: list[dict[str, Any]] = []
    main_stock_runs: list[dict[str, Any]] = []

    for task_id in task_ids:
        stock_runs = [
            runs[("fixed-fallback", "stock", task_id, repetition)]
            for repetition in range(1, 4)
        ]
        candidate_runs = [
            runs[("fixed-fallback", "v0.2", task_id, repetition)]
            for repetition in range(1, 4)
        ]
        stock_majority.append(majority([run["solved"] for run in stock_runs]))
        candidate_majority.append(majority([run["solved"] for run in candidate_runs]))
        main_stock_runs.extend(stock_runs)
        main_candidate_runs.extend(candidate_runs)
        ratios: list[float] = []
        wall_ratios: list[float] = []
        for stock_run, candidate_run in zip(stock_runs, candidate_runs, strict=True):
            stock_tokens = stock_run["tokens"]
            candidate_tokens = candidate_run["tokens"]
            stock_blended += stock_tokens["blendedTokens"]
            candidate_blended += candidate_tokens["blendedTokens"]
            if stock_run["wallTimeMs"] <= 0:
                raise ArtifactValidationError("stock wall time is not positive")
            wall_ratios.append(candidate_run["wallTimeMs"] / stock_run["wallTimeMs"])
            if (
                stock_run["solved"]
                and candidate_run["solved"]
                and stock_tokens["nonCachedInputTokens"] > 0
                and candidate_run["counters"]["agenticTransitions"] > 0
            ):
                ratios.append(
                    candidate_tokens["nonCachedInputTokens"]
                    / stock_tokens["nonCachedInputTokens"]
                )
        wall_task_ratios.append(statistics.median(wall_ratios))
        if ratios:
            compact_task_ratios.append(statistics.median(ratios))

    differences = [
        float(right) - float(left)
        for left, right in zip(stock_majority, candidate_majority, strict=True)
    ]
    quality_difference = statistics.mean(differences)
    bootstrap_seed = manifest["seeds"]["bootstrapSeed"]
    samples = manifest["thresholds"]["bootstrapSamples"]
    quality_bootstrap = bootstrap_mean(differences, bootstrap_seed, "quality", samples)
    quality_lower = nearest_rank(quality_bootstrap, 0.05)
    quality_interval_lower = nearest_rank(quality_bootstrap, 0.025)
    quality_upper = nearest_rank(quality_bootstrap, 0.975)
    mcnemar_b, mcnemar_c, mcnemar_p = mcnemar(stock_majority, candidate_majority)
    compact_median = (
        statistics.median(compact_task_ratios) if compact_task_ratios else None
    )
    compact_upper = None
    if compact_task_ratios:
        compact_upper = nearest_rank(
            bootstrap_median(compact_task_ratios, bootstrap_seed, samples), 0.95
        )
    if stock_blended <= 0:
        raise ArtifactValidationError("stock blended-token denominator is not positive")
    blended_ratio = candidate_blended / stock_blended
    wall_ratio = statistics.median(wall_task_ratios)
    comparison = arm_comparison(main_stock_runs, main_candidate_runs)

    safety_violations = sum(sum(run["safety"].values()) for run in main_candidate_runs)
    stock_failure_classes: dict[str, set[str]] = {}
    for key, run in runs.items():
        if (
            key[0] == "fixed-fallback"
            and key[1] == "stock"
            and run["failureClass"] is not None
        ):
            stock_failure_classes.setdefault(run["taskId"], set()).add(
                run["failureClass"]
            )
    candidate_failure_tasks: dict[str, set[str]] = {}
    for run in main_candidate_runs:
        failure_class = run["failureClass"]
        if (
            run["candidateCaused"]
            and failure_class is not None
            and failure_class not in stock_failure_classes.get(run["taskId"], set())
        ):
            candidate_failure_tasks.setdefault(failure_class, set()).add(run["taskId"])
    systematic_failure_classes = sum(
        len(tasks) >= 2 for tasks in candidate_failure_tasks.values()
    )

    main_run_ids = {run["runId"] for run in main_candidate_runs}
    main_annotations = [
        annotation for annotation in annotations if annotation["runId"] in main_run_ids
    ]

    triggers = [
        item
        for item in main_annotations
        if item["stream"] == "trigger" and item["final"] != "INSERT"
    ]
    harmful = sum(item["final"] == "DELETE" for item in triggers)
    trigger_tasks = {item["taskId"] for item in triggers}
    continuations = [
        item for item in main_annotations if item["stream"] == "continuation"
    ]
    repeated = sum(
        item["attributes"]["repeatedSettledPhase"] is True for item in continuations
    )
    continuation_tasks = {item["taskId"] for item in continuations}
    checkpoints = [item for item in main_annotations if item["stream"] == "checkpoint"]
    checkpoint_tasks = {item["taskId"] for item in checkpoints}
    contradictions = sum(
        item["attributes"]["criticalContradiction"] is True for item in checkpoints
    )
    omissions = sum(
        item["attributes"]["criticalOmission"] is True for item in checkpoints
    )
    actionable = sum(
        item["attributes"]["nextActionActionable"] is True for item in checkpoints
    )
    compaction_accounting = _compaction_accounting(main_stock_runs, main_candidate_runs)
    host_total = compaction_accounting["candidateProactive"]["transitions"]
    host_matches = sum(
        transition["hostEvidenceMatch"] is True
        for run in main_candidate_runs
        for transition in run["transitions"]
        if transition["terminalState"] == "cooldown"
    )
    double_reviewed = [item for item in main_annotations if item["doubleReviewed"]]
    agreements = sum(item["primary"] == item["secondary"] for item in double_reviewed)
    expected_reviews = expected_double_review_ids(
        main_annotations, manifest["seeds"]["judgeSamplingSeed"]
    )
    actual_reviews = {item["annotationId"] for item in double_reviewed}
    review_coverage = actual_reviews == expected_reviews and all(
        any(item["stream"] == stream for item in main_annotations)
        for stream in ("trigger", "checkpoint", "continuation")
    )
    stream_agreements = []
    for stream in ("trigger", "checkpoint", "continuation"):
        reviewed = [item for item in double_reviewed if item["stream"] == stream]
        if reviewed:
            stream_agreements.append(
                sum(item["primary"] == item["secondary"] for item in reviewed)
                / len(reviewed)
            )
    judge_agreement = min(stream_agreements) if len(stream_agreements) == 3 else None

    thresholds = manifest["thresholds"]
    gates: list[dict[str, Any]] = []
    gates.append(
        _gate(
            "absolute-safety",
            "pass" if safety_violations == 0 else "fail",
            "zero_violations" if safety_violations == 0 else "violations_present",
            numerator=safety_violations,
            denominator=len(main_candidate_runs),
            estimate=float(safety_violations),
        )
    )
    solved_deficit = sum(stock_majority) - sum(candidate_majority)
    stock_solved = sum(stock_majority)
    candidate_solved = sum(candidate_majority)
    gates.append(
        _threshold_gate(
            "quality-solved-deficit",
            float(solved_deficit),
            float(thresholds["maximumSolvedTaskDeficit"]),
            at_most=True,
            numerator=candidate_solved,
            denominator=stock_solved,
        )
    )
    gates.append(
        _threshold_gate(
            "quality-observed-difference",
            quality_difference,
            thresholds["minimumObservedQualityDifference"],
            at_most=False,
            numerator=candidate_solved,
            denominator=len(task_ids),
        )
    )
    quality_gate = _threshold_gate(
        "quality-bootstrap-lower-bound",
        quality_lower,
        thresholds["minimumQualityLowerBound"],
        at_most=False,
        numerator=candidate_solved,
        denominator=len(task_ids),
    )
    quality_gate["lowerBound"] = quality_interval_lower
    quality_gate["upperBound"] = quality_upper
    gates.append(quality_gate)
    gates.append(
        _gate(
            "systematic-new-failure",
            "pass" if systematic_failure_classes == 0 else "fail",
            "zero_violations"
            if systematic_failure_classes == 0
            else "violations_present",
            numerator=systematic_failure_classes,
            denominator=len(task_ids),
            estimate=float(systematic_failure_classes),
        )
    )

    compact_support = len(compact_task_ratios)
    compact_supported = compact_support >= thresholds["minimumCompactSubsetTasks"]
    gates.append(
        _gate(
            "compact-subset-support",
            "pass" if compact_supported else "inconclusive",
            "threshold_met" if compact_supported else "insufficient_support",
            numerator=compact_support,
            denominator=len(task_ids),
            estimate=float(compact_support),
        )
    )
    for name, value, threshold in (
        (
            "compact-subset-point",
            compact_median,
            thresholds["maximumCompactSubsetMedian"],
        ),
        (
            "compact-subset-upper-bound",
            compact_upper,
            thresholds["maximumCompactSubsetUpperBound"],
        ),
    ):
        if not compact_supported or value is None:
            gates.append(
                _gate(
                    name,
                    "inconclusive",
                    "insufficient_support",
                    numerator=compact_support,
                    denominator=len(task_ids),
                )
            )
        else:
            gates.append(
                _threshold_gate(
                    name,
                    value,
                    threshold,
                    at_most=True,
                    numerator=compact_support,
                    denominator=len(task_ids),
                )
            )
    gates.append(
        _threshold_gate(
            "overall-blended-token-ratio",
            blended_ratio,
            thresholds["maximumBlendedTokenRatio"],
            at_most=True,
            numerator=candidate_blended,
            denominator=stock_blended,
        )
    )
    gates.append(
        _threshold_gate(
            "overall-wall-time-ratio",
            wall_ratio,
            thresholds["maximumMedianWallRatio"],
            at_most=True,
            numerator=len(wall_task_ratios),
            denominator=len(task_ids),
        )
    )

    harmful_supported = (
        len(triggers) >= thresholds["minimumPolicyRequests"]
        and len(trigger_tasks) >= thresholds["minimumPolicyTasks"]
    )
    gates.append(
        _gate(
            "harmful-trigger-support",
            "pass" if harmful_supported else "inconclusive",
            "threshold_met" if harmful_supported else "insufficient_support",
            numerator=len(trigger_tasks),
            denominator=len(triggers),
        )
    )
    harmful_rate = harmful / len(triggers) if triggers else None
    gates.append(
        _threshold_gate(
            "harmful-trigger-rate",
            harmful_rate,
            thresholds["maximumHarmfulTriggerRate"],
            at_most=True,
            numerator=harmful,
            denominator=len(triggers),
        )
        if harmful_supported and harmful_rate is not None
        else _gate(
            "harmful-trigger-rate",
            "inconclusive",
            "insufficient_support",
            numerator=harmful,
            denominator=len(triggers),
        )
    )

    repeated_supported = len(continuation_tasks) >= thresholds[
        "minimumPolicyTasks"
    ] and bool(continuations)
    gates.append(
        _gate(
            "repeated-settled-phase-support",
            "pass" if repeated_supported else "inconclusive",
            "threshold_met" if repeated_supported else "insufficient_support",
            numerator=len(continuation_tasks),
            denominator=len(continuations),
        )
    )
    repeated_rate = repeated / len(continuations) if continuations else None
    gates.append(
        _threshold_gate(
            "repeated-settled-phase-rate",
            repeated_rate,
            thresholds["maximumRepeatedSettledPhaseRate"],
            at_most=True,
            numerator=repeated,
            denominator=len(continuations),
        )
        if repeated_supported and repeated_rate is not None
        else _gate(
            "repeated-settled-phase-rate",
            "inconclusive",
            "insufficient_support",
            numerator=repeated,
            denominator=len(continuations),
        )
    )

    checkpoint_supported = len(checkpoint_tasks) >= thresholds[
        "minimumPolicyTasks"
    ] and bool(checkpoints)
    gates.append(
        _gate(
            "critical-checkpoint-contradiction",
            "pass"
            if checkpoint_supported and contradictions == 0
            else ("fail" if contradictions else "inconclusive"),
            "zero_violations"
            if checkpoint_supported and contradictions == 0
            else ("violations_present" if contradictions else "insufficient_support"),
            numerator=contradictions,
            denominator=len(checkpoints),
        )
    )
    gates.append(
        _gate(
            "critical-omission-support",
            "pass" if checkpoint_supported else "inconclusive",
            "threshold_met" if checkpoint_supported else "insufficient_support",
            numerator=len(checkpoint_tasks),
            denominator=len(checkpoints),
        )
    )
    omission_rate = omissions / len(checkpoints) if checkpoints else None
    gates.append(
        _threshold_gate(
            "critical-omission-rate",
            omission_rate,
            thresholds["maximumCriticalOmissionRate"],
            at_most=True,
            numerator=omissions,
            denominator=len(checkpoints),
        )
        if checkpoint_supported and omission_rate is not None
        else _gate(
            "critical-omission-rate",
            "inconclusive",
            "insufficient_support",
            numerator=omissions,
            denominator=len(checkpoints),
        )
    )
    gates.append(
        _gate(
            "next-action-support",
            "pass" if checkpoint_supported else "inconclusive",
            "threshold_met" if checkpoint_supported else "insufficient_support",
            numerator=len(checkpoint_tasks),
            denominator=len(checkpoints),
        )
    )
    actionable_rate = actionable / len(checkpoints) if checkpoints else None
    gates.append(
        _threshold_gate(
            "next-action-actionable",
            actionable_rate,
            thresholds["minimumActionableNextActionRate"],
            at_most=False,
            numerator=actionable,
            denominator=len(checkpoints),
        )
        if checkpoint_supported and actionable_rate is not None
        else _gate(
            "next-action-actionable",
            "inconclusive",
            "insufficient_support",
            numerator=actionable,
            denominator=len(checkpoints),
        )
    )
    host_recall = host_matches / host_total if host_total else None
    gates.append(
        _threshold_gate(
            "host-evidence-recall",
            host_recall,
            thresholds["minimumHostEvidenceRecall"],
            at_most=False,
            numerator=host_matches,
            denominator=host_total,
        )
        if host_recall is not None
        else _gate(
            "host-evidence-recall",
            "inconclusive",
            "insufficient_support",
            numerator=host_matches,
            denominator=host_total,
        )
    )
    gates.append(
        _threshold_gate(
            "judge-agreement",
            judge_agreement,
            manifest["judge"]["minimumAgreement"],
            at_most=False,
            numerator=agreements,
            denominator=len(double_reviewed),
        )
        if review_coverage and judge_agreement is not None
        else _gate(
            "judge-agreement",
            "inconclusive",
            "insufficient_support",
            numerator=agreements,
            denominator=len(double_reviewed),
        )
    )

    invalid_attempts, invalid_blocks = validate_block_attempts(runs)
    invalid_reasons: dict[str, int] = {}
    seen_blocks: set[tuple[str, str, int]] = set()
    for (regime, _arm, task_id, repetition), run in runs.items():
        block = (regime, task_id, repetition)
        if block in seen_blocks:
            continue
        seen_blocks.add(block)
        for item in run["priorInvalidBlocks"]:
            invalid_reasons[item["reasonCode"]] = (
                invalid_reasons.get(item["reasonCode"], 0) + item["count"]
            )
    if sum(invalid_reasons.values()) != invalid_blocks:
        raise ArtifactValidationError(
            "invalid reason counts differ from paired-block attrition"
        )

    candidate_hashes = [run["configHashes"] for run in main_candidate_runs]
    binary_sha256 = _unique(
        [item["binarySha256"] for item in candidate_hashes], "candidate binary hash"
    )
    plugin_sha256 = _unique(
        [item["pluginSha256"] for item in candidate_hashes], "candidate plugin hash"
    )
    surface_sha256 = _unique(
        [item["surfaceSha256"] for item in candidate_hashes], "candidate surface hash"
    )
    capability_record_sha256 = _unique(
        [item["capabilityRecordSha256"] for item in candidate_hashes],
        "candidate capability record hash",
    )
    config_sha256 = _unique(
        [item["configSha256"] for item in candidate_hashes], "candidate config hash"
    )
    if None in (
        binary_sha256,
        plugin_sha256,
        surface_sha256,
        capability_record_sha256,
    ):
        raise ArtifactValidationError("v0.2 results lack candidate artifact hashes")
    artifact_hashes = [sha256_json(run) for _, run in sorted(runs.items())]
    artifact_hashes.extend(
        sha256_json(annotation)
        for annotation in sorted(annotations, key=lambda item: item["annotationId"])
    )
    result_set_sha256 = hashlib.sha256(
        "".join(artifact_hashes).encode("ascii")
    ).hexdigest()
    unavailable = [
        gate["name"]
        for gate in gates
        if gate["status"] in {"unavailable", "inconclusive"}
    ]
    overall_pass = all(gate["status"] == "pass" for gate in gates)
    summary = {
        "schemaVersion": 1,
        "stage": "stage-b",
        "candidateCommit": candidate,
        "provenance": {
            "binarySha256": binary_sha256,
            "pluginSha256": plugin_sha256,
            "surfaceSha256": surface_sha256,
            "capabilityRecordSha256": capability_record_sha256,
            "configSha256": config_sha256,
            "taskManifestSha256": manifest_sha256,
            "judgeConfigSha256": manifest["judge"]["judgeConfigSha256"],
            "codexBinarySha256": manifest["runtime"]["codexBinarySha256"],
            "resultSetSha256": result_set_sha256,
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
            "heldOutTasks": len(task_ids),
            "validRuns": len(runs),
            "invalidAttempts": invalid_attempts,
            "invalidBlocks": invalid_blocks,
            "validPairedBlocks": len(seen_blocks),
            "annotations": len(main_annotations),
            "doubleReviewedAnnotations": len(double_reviewed),
            "judgeDisagreements": len(double_reviewed) - agreements,
            "systematicNewFailureClasses": systematic_failure_classes,
        },
        "invalidReasons": [
            {"reason": reason, "count": count}
            for reason, count in sorted(invalid_reasons.items())
        ],
        "gates": gates,
        "diagnostics": {
            "qualityDifference": quality_difference,
            "qualityLowerBound": quality_lower,
            "qualityUpperBound": quality_upper,
            "mcnemarB": mcnemar_b,
            "mcnemarC": mcnemar_c,
            "mcnemarPValue": mcnemar_p,
            "compactSubsetTaskCount": compact_support,
            "compactSubsetMedian": compact_median,
            "compactSubsetUpperBound": compact_upper,
            "blendedTokenRatio": blended_ratio,
            "medianWallRatio": wall_ratio,
            "harmfulTriggerRate": harmful_rate,
            "repeatedSettledPhaseRate": repeated_rate,
            "criticalOmissionRate": omission_rate,
            "actionableNextActionRate": actionable_rate,
            "hostEvidenceRecall": host_recall,
            "judgeAgreement": judge_agreement,
            "compactionAccounting": compaction_accounting,
            "armComparison": comparison,
        },
        "unavailableMetrics": unavailable,
        "overallPass": overall_pass,
    }
    return summary


def validate_arm_comparison(value: dict[str, Any], blended_ratio: float) -> None:
    if value["runCount"] != 150:
        raise ArtifactValidationError(
            "$.diagnostics.armComparison.runCount: must equal 150"
        )
    stock = value["stock"]
    candidate = value["agenticCompact"]
    delta = value["delta"]
    ratio = value["ratio"]
    for arm_name, arm in (("stock", stock), ("agenticCompact", candidate)):
        if arm["solvedRuns"] > value["runCount"] or arm["wallTimeMs"] <= 0:
            raise ArtifactValidationError(
                f"$.diagnostics.armComparison.{arm_name}: counters are invalid"
            )
        if arm["tokens"]["blendedTokens"] != (
            arm["tokens"]["nonCachedInputTokens"] + arm["tokens"]["outputTokens"]
        ):
            raise ArtifactValidationError(
                f"$.diagnostics.armComparison.{arm_name}: blended total is invalid"
            )
        context = arm["contextUsage"]
        if (
            context["tokenUsageSamples"] < value["runCount"]
            or context["modelContextWindow"] <= 0
            or not 0 < context["peakInputTokens"] <= arm["tokens"]["inputTokens"]
        ):
            raise ArtifactValidationError(
                f"$.diagnostics.armComparison.{arm_name}.contextUsage: counters are invalid"
            )
        expected_mean = arm["tokens"]["inputTokens"] / context["tokenUsageSamples"]
        expected_utilization = (
            context["peakInputTokens"] / context["modelContextWindow"]
        )
        if not math.isclose(
            context["meanInputTokensPerSample"],
            expected_mean,
            rel_tol=0.0,
            abs_tol=1e-12,
        ) or not math.isclose(
            context["peakWindowUtilization"],
            expected_utilization,
            rel_tol=0.0,
            abs_tol=1e-12,
        ):
            raise ArtifactValidationError(
                f"$.diagnostics.armComparison.{arm_name}.contextUsage: derived values are invalid"
            )
    if stock["contextUsage"]["modelContextWindow"] != candidate["contextUsage"][
        "modelContextWindow"
    ]:
        raise ArtifactValidationError(
            "$.diagnostics.armComparison: context windows differ between arms"
        )
    for field in TOKEN_FIELDS:
        expected_delta = candidate["tokens"][field] - stock["tokens"][field]
        if delta["tokens"][field] != expected_delta:
            raise ArtifactValidationError(
                f"$.diagnostics.armComparison.delta.tokens.{field}: is invalid"
            )
        expected_ratio = (
            candidate["tokens"][field] / stock["tokens"][field]
            if stock["tokens"][field] > 0
            else None
        )
        observed_ratio = ratio["tokens"][field]
        if expected_ratio is None:
            if observed_ratio is not None:
                raise ArtifactValidationError(
                    f"$.diagnostics.armComparison.ratio.tokens.{field}: is invalid"
                )
        elif observed_ratio is None or not math.isclose(
            observed_ratio, expected_ratio, rel_tol=0.0, abs_tol=1e-12
        ):
            raise ArtifactValidationError(
                f"$.diagnostics.armComparison.ratio.tokens.{field}: is invalid"
            )
    for field in ("benchmarkScoreMean", "solvedRuns", "wallTimeMs"):
        if delta[field] != candidate[field] - stock[field]:
            raise ArtifactValidationError(
                f"$.diagnostics.armComparison.delta.{field}: is invalid"
            )
    for field in (
        "tokenUsageSamples",
        "meanInputTokensPerSample",
        "peakInputTokens",
        "peakWindowUtilization",
    ):
        expected_delta = (
            candidate["contextUsage"][field] - stock["contextUsage"][field]
        )
        if not math.isclose(
            delta["contextUsage"][field],
            expected_delta,
            rel_tol=0.0,
            abs_tol=1e-12,
        ):
            raise ArtifactValidationError(
                f"$.diagnostics.armComparison.delta.contextUsage.{field}: is invalid"
            )
        expected_ratio = (
            candidate["contextUsage"][field] / stock["contextUsage"][field]
        )
        if not math.isclose(
            ratio["contextUsage"][field],
            expected_ratio,
            rel_tol=0.0,
            abs_tol=1e-12,
        ):
            raise ArtifactValidationError(
                f"$.diagnostics.armComparison.ratio.contextUsage.{field}: is invalid"
            )
    expected_score_ratio = (
        candidate["benchmarkScoreMean"] / stock["benchmarkScoreMean"]
        if stock["benchmarkScoreMean"] > 0
        else None
    )
    if expected_score_ratio is None:
        if ratio["benchmarkScoreMean"] is not None:
            raise ArtifactValidationError(
                "$.diagnostics.armComparison.ratio.benchmarkScoreMean: is invalid"
            )
    elif ratio["benchmarkScoreMean"] is None or not math.isclose(
        ratio["benchmarkScoreMean"], expected_score_ratio, rel_tol=0.0, abs_tol=1e-12
    ):
        raise ArtifactValidationError(
            "$.diagnostics.armComparison.ratio.benchmarkScoreMean: is invalid"
        )
    expected_wall_ratio = candidate["wallTimeMs"] / stock["wallTimeMs"]
    if not math.isclose(
        ratio["wallTimeMs"], expected_wall_ratio, rel_tol=0.0, abs_tol=1e-12
    ):
        raise ArtifactValidationError(
            "$.diagnostics.armComparison.ratio.wallTimeMs: is invalid"
        )
    if ratio["tokens"]["blendedTokens"] is None or not math.isclose(
        ratio["tokens"]["blendedTokens"],
        blended_ratio,
        rel_tol=0.0,
        abs_tol=1e-12,
    ):
        raise ArtifactValidationError(
            "$.diagnostics.armComparison: blended ratio differs from its gate"
        )


def validate_summary(summary: Any, schema: RestrictedSchema) -> None:
    schema.validate(summary)
    validate_privacy(summary)
    gate_names = [gate["name"] for gate in summary["gates"]]
    if len(gate_names) != len(set(gate_names)) or len(gate_names) != 21:
        raise ArtifactValidationError("$.gates: expected each frozen gate exactly once")
    expected_overall = all(gate["status"] == "pass" for gate in summary["gates"])
    if summary["overallPass"] != expected_overall:
        raise ArtifactValidationError("$.overallPass: differs from gate statuses")
    unavailable = sorted(
        gate["name"]
        for gate in summary["gates"]
        if gate["status"] in {"unavailable", "inconclusive"}
    )
    if sorted(summary["unavailableMetrics"]) != unavailable:
        raise ArtifactValidationError(
            "$.unavailableMetrics: differs from unavailable gate statuses"
        )
    counts = summary["counts"]
    if counts["heldOutTasks"] != 50 or counts["validRuns"] not in {340, 380}:
        raise ArtifactValidationError(
            "$.counts: Stage B task or run count is not frozen"
        )
    validate_arm_comparison(
        summary["diagnostics"]["armComparison"],
        summary["diagnostics"]["blendedTokenRatio"],
    )
    expected_blocks = 150 if counts["validRuns"] == 340 else 170
    if counts["validPairedBlocks"] != expected_blocks:
        raise ArtifactValidationError(
            "$.counts.validPairedBlocks: differs from the frozen matrix"
        )
    if counts["doubleReviewedAnnotations"] > counts["annotations"]:
        raise ArtifactValidationError(
            "$.counts: double-reviewed annotations exceed annotations"
        )
    if counts["judgeDisagreements"] > counts["doubleReviewedAnnotations"]:
        raise ArtifactValidationError(
            "$.counts: judge disagreements exceed double reviews"
        )
    accounting = summary["diagnostics"]["compactionAccounting"]
    stock_native = accounting["stockNative"]
    proactive = accounting["candidateProactive"]
    residual_native = accounting["candidateResidualNative"]
    if any(item["tasks"] > 50 for item in (stock_native, residual_native)):
        raise ArtifactValidationError(
            "$.diagnostics.compactionAccounting: task count exceeds held-out set"
        )
    if (
        stock_native["compactions"] < stock_native["tasks"]
        or residual_native["compactions"] < residual_native["tasks"]
    ):
        raise ArtifactValidationError(
            "$.diagnostics.compactionAccounting: native task count exceeds events"
        )
    if not (
        proactive["transitions"]
        <= proactive["scheduledRequests"]
        <= proactive["requests"]
        and proactive["transitionTasks"] <= proactive["requestTasks"] <= 50
        and proactive["transitions"] >= proactive["transitionTasks"]
        and proactive["requests"] >= proactive["requestTasks"]
    ):
        raise ArtifactValidationError(
            "$.diagnostics.compactionAccounting: proactive counts are inconsistent"
        )
    if proactive["measuredTransitions"] != proactive["transitions"]:
        raise ArtifactValidationError(
            "$.diagnostics.compactionAccounting: transition token evidence is incomplete"
        )
    if proactive["transitions"] > 0 and (
        proactive["preCompactionInputTokens"] <= 0
        or proactive["postCompactionInputTokens"] <= 0
    ):
        raise ArtifactValidationError(
            "$.diagnostics.compactionAccounting: measured transition totals are empty"
        )
    expected_removed = (
        proactive["preCompactionInputTokens"]
        - proactive["postCompactionInputTokens"]
    )
    if proactive["activeInputTokensRemoved"] != expected_removed:
        raise ArtifactValidationError(
            "$.diagnostics.compactionAccounting: active input token delta is invalid"
        )
    expected_ratio = (
        proactive["postCompactionInputTokens"]
        / proactive["preCompactionInputTokens"]
        if proactive["preCompactionInputTokens"] > 0
        else None
    )
    if proactive["postToPreInputRatio"] != expected_ratio:
        raise ArtifactValidationError(
            "$.diagnostics.compactionAccounting: active input token ratio is invalid"
        )
    if summary["diagnostics"]["compactSubsetTaskCount"] > proactive["transitionTasks"]:
        raise ArtifactValidationError(
            "$.diagnostics.compactionAccounting: compact subset exceeds transition tasks"
        )
    if (
        sum(item["count"] for item in summary["invalidReasons"])
        != counts["invalidBlocks"]
    ):
        raise ArtifactValidationError(
            "$.invalidReasons: counts differ from invalid blocks"
        )
    gates = {gate["name"]: gate for gate in summary["gates"]}
    if any(
        gate["numerator"] is None or gate["denominator"] is None
        for gate in gates.values()
    ):
        raise ArtifactValidationError(
            "$.gates: numerator and denominator must be explicit"
        )

    def check_gate(
        name: str, passed: bool, *, supported: bool = True, zero_rule: bool = False
    ) -> None:
        gate = gates[name]
        expected_status = (
            "pass"
            if supported and passed
            else ("fail" if supported else "inconclusive")
        )
        if gate["status"] != expected_status:
            raise ArtifactValidationError(
                f"$.gates: {name} status differs from its frozen rule"
            )
        if not supported:
            expected_reason = "insufficient_support"
        elif zero_rule:
            expected_reason = "zero_violations" if passed else "violations_present"
        else:
            expected_reason = "threshold_met" if passed else "threshold_exceeded"
        if gate["reason"] != expected_reason:
            raise ArtifactValidationError(
                f"$.gates: {name} reason differs from its frozen rule"
            )

    absolute = gates["absolute-safety"]
    check_gate("absolute-safety", absolute["numerator"] == 0, zero_rule=True)
    solved = gates["quality-solved-deficit"]
    check_gate(
        "quality-solved-deficit",
        solved["estimate"] is not None and solved["estimate"] <= 1,
    )
    observed = gates["quality-observed-difference"]
    check_gate(
        "quality-observed-difference",
        observed["estimate"] is not None
        and observed["estimate"]
        >= EXPECTED_THRESHOLDS["minimumObservedQualityDifference"],
    )
    quality = gates["quality-bootstrap-lower-bound"]
    check_gate(
        "quality-bootstrap-lower-bound",
        quality["estimate"] is not None
        and quality["estimate"] >= EXPECTED_THRESHOLDS["minimumQualityLowerBound"],
    )
    systematic = gates["systematic-new-failure"]
    check_gate("systematic-new-failure", systematic["numerator"] == 0, zero_rule=True)

    compact_support = gates["compact-subset-support"]
    compact_supported = (
        compact_support["numerator"] >= EXPECTED_THRESHOLDS["minimumCompactSubsetTasks"]
    )
    check_gate("compact-subset-support", compact_supported, supported=compact_supported)
    for name, threshold in (
        ("compact-subset-point", EXPECTED_THRESHOLDS["maximumCompactSubsetMedian"]),
        (
            "compact-subset-upper-bound",
            EXPECTED_THRESHOLDS["maximumCompactSubsetUpperBound"],
        ),
    ):
        gate = gates[name]
        check_gate(
            name,
            gate["estimate"] is not None and gate["estimate"] <= threshold,
            supported=compact_supported and gate["estimate"] is not None,
        )
    for name, threshold in (
        (
            "overall-blended-token-ratio",
            EXPECTED_THRESHOLDS["maximumBlendedTokenRatio"],
        ),
        ("overall-wall-time-ratio", EXPECTED_THRESHOLDS["maximumMedianWallRatio"]),
    ):
        gate = gates[name]
        check_gate(name, gate["estimate"] is not None and gate["estimate"] <= threshold)

    harmful_support = gates["harmful-trigger-support"]
    harmful_supported = (
        harmful_support["numerator"] >= EXPECTED_THRESHOLDS["minimumPolicyTasks"]
        and harmful_support["denominator"]
        >= EXPECTED_THRESHOLDS["minimumPolicyRequests"]
    )
    check_gate(
        "harmful-trigger-support", harmful_supported, supported=harmful_supported
    )
    harmful = gates["harmful-trigger-rate"]
    check_gate(
        "harmful-trigger-rate",
        harmful["estimate"] is not None
        and harmful["estimate"] <= EXPECTED_THRESHOLDS["maximumHarmfulTriggerRate"],
        supported=harmful_supported and harmful["estimate"] is not None,
    )
    repeated_support = gates["repeated-settled-phase-support"]
    repeated_supported = (
        repeated_support["numerator"] >= EXPECTED_THRESHOLDS["minimumPolicyTasks"]
        and repeated_support["denominator"] > 0
    )
    check_gate(
        "repeated-settled-phase-support",
        repeated_supported,
        supported=repeated_supported,
    )
    repeated_gate = gates["repeated-settled-phase-rate"]
    check_gate(
        "repeated-settled-phase-rate",
        repeated_gate["estimate"] is not None
        and repeated_gate["estimate"]
        <= EXPECTED_THRESHOLDS["maximumRepeatedSettledPhaseRate"],
        supported=repeated_supported and repeated_gate["estimate"] is not None,
    )

    omission_support = gates["critical-omission-support"]
    checkpoint_supported = (
        omission_support["numerator"] >= EXPECTED_THRESHOLDS["minimumPolicyTasks"]
        and omission_support["denominator"] > 0
    )
    contradiction = gates["critical-checkpoint-contradiction"]
    check_gate(
        "critical-checkpoint-contradiction",
        contradiction["numerator"] == 0,
        supported=checkpoint_supported or contradiction["numerator"] > 0,
        zero_rule=True,
    )
    check_gate(
        "critical-omission-support",
        checkpoint_supported,
        supported=checkpoint_supported,
    )
    omission = gates["critical-omission-rate"]
    check_gate(
        "critical-omission-rate",
        omission["estimate"] is not None
        and omission["estimate"] <= EXPECTED_THRESHOLDS["maximumCriticalOmissionRate"],
        supported=checkpoint_supported and omission["estimate"] is not None,
    )
    next_support = gates["next-action-support"]
    next_supported = (
        next_support["numerator"] >= EXPECTED_THRESHOLDS["minimumPolicyTasks"]
        and next_support["denominator"] > 0
    )
    check_gate("next-action-support", next_supported, supported=next_supported)
    actionable = gates["next-action-actionable"]
    check_gate(
        "next-action-actionable",
        actionable["estimate"] is not None
        and actionable["estimate"]
        >= EXPECTED_THRESHOLDS["minimumActionableNextActionRate"],
        supported=next_supported and actionable["estimate"] is not None,
    )
    host = gates["host-evidence-recall"]
    check_gate(
        "host-evidence-recall",
        host["denominator"] > 0
        and host["numerator"] == host["denominator"]
        and host["estimate"] == 1.0,
        supported=host["denominator"] > 0 and host["estimate"] is not None,
    )
    judge = gates["judge-agreement"]
    check_gate(
        "judge-agreement",
        judge["denominator"] > 0
        and judge["estimate"] is not None
        and judge["estimate"] >= 0.90,
        supported=judge["denominator"] > 0 and judge["estimate"] is not None,
    )

    diagnostics = summary["diagnostics"]
    diagnostic_gates = {
        "qualityDifference": "quality-observed-difference",
        "qualityLowerBound": "quality-bootstrap-lower-bound",
        "compactSubsetMedian": "compact-subset-point",
        "compactSubsetUpperBound": "compact-subset-upper-bound",
        "blendedTokenRatio": "overall-blended-token-ratio",
        "medianWallRatio": "overall-wall-time-ratio",
        "harmfulTriggerRate": "harmful-trigger-rate",
        "repeatedSettledPhaseRate": "repeated-settled-phase-rate",
        "criticalOmissionRate": "critical-omission-rate",
        "actionableNextActionRate": "next-action-actionable",
        "hostEvidenceRecall": "host-evidence-recall",
        "judgeAgreement": "judge-agreement",
    }
    if any(
        diagnostics[field] != gates[name]["estimate"]
        for field, name in diagnostic_gates.items()
    ):
        raise ArtifactValidationError("$.diagnostics: value differs from its gate")
    for name in (
        "overall-blended-token-ratio",
        "harmful-trigger-rate",
        "repeated-settled-phase-rate",
        "critical-omission-rate",
        "next-action-actionable",
        "host-evidence-recall",
    ):
        gate = gates[name]
        if gate["denominator"] > 0 and gate["estimate"] is not None:
            ratio = gate["numerator"] / gate["denominator"]
            if not math.isclose(gate["estimate"], ratio, rel_tol=0.0, abs_tol=1e-12):
                raise ArtifactValidationError(
                    f"$.gates: {name} estimate differs from its counters"
                )
    if diagnostics["qualityUpperBound"] != quality["upperBound"]:
        raise ArtifactValidationError(
            "$.diagnostics.qualityUpperBound: differs from the quality gate"
        )
    if diagnostics["compactSubsetTaskCount"] != compact_support["numerator"]:
        raise ArtifactValidationError(
            "$.diagnostics.compactSubsetTaskCount: differs from support"
        )
    if counts["systematicNewFailureClasses"] != systematic["numerator"]:
        raise ArtifactValidationError(
            "$.counts.systematicNewFailureClasses: differs from its gate"
        )
    if counts["invalidBlocks"] == 0:
        if counts["invalidAttempts"] != 0:
            raise ArtifactValidationError(
                "$.counts.invalidAttempts: nonzero without an invalid block"
            )
    elif (
        not 2 * counts["invalidBlocks"]
        <= counts["invalidAttempts"]
        <= 4 * counts["invalidBlocks"]
    ):
        raise ArtifactValidationError(
            "$.counts.invalidAttempts: outside paired-block arm bounds"
        )
    if absolute["denominator"] != 150 or systematic["denominator"] != 50:
        raise ArtifactValidationError(
            "$.gates: main safety or failure denominator is incorrect"
        )
    if not (
        0 <= solved["numerator"] <= 50
        and 0 <= solved["denominator"] <= 50
        and solved["estimate"] == solved["denominator"] - solved["numerator"]
    ):
        raise ArtifactValidationError(
            "$.gates.quality-solved-deficit: counters are inconsistent"
        )
    if (
        observed["numerator"] != solved["numerator"]
        or observed["denominator"] != 50
        or quality["numerator"] != solved["numerator"]
        or quality["denominator"] != 50
    ):
        raise ArtifactValidationError("$.gates: quality counters are inconsistent")
    expected_difference = (solved["numerator"] - solved["denominator"]) / 50
    if not math.isclose(
        observed["estimate"], expected_difference, rel_tol=0.0, abs_tol=1e-12
    ):
        raise ArtifactValidationError(
            "$.gates.quality-observed-difference: estimate is inconsistent"
        )
    if (
        quality["lowerBound"] is None
        or quality["upperBound"] is None
        or quality["estimate"] is None
        or not quality["lowerBound"] <= quality["estimate"] <= quality["upperBound"]
    ):
        raise ArtifactValidationError(
            "$.gates.quality-bootstrap-lower-bound: interval is inconsistent"
        )
    if (
        any(
            gate["denominator"] != 50
            or gate["numerator"] != compact_support["numerator"]
            for gate in (
                gates["compact-subset-point"],
                gates["compact-subset-upper-bound"],
            )
        )
        or compact_support["denominator"] != 50
    ):
        raise ArtifactValidationError(
            "$.gates: compact-subset support counters are inconsistent"
        )
    if gates["overall-blended-token-ratio"]["denominator"] <= 0:
        raise ArtifactValidationError(
            "$.gates.overall-blended-token-ratio: denominator is not positive"
        )
    wall = gates["overall-wall-time-ratio"]
    if wall["numerator"] != 50 or wall["denominator"] != 50:
        raise ArtifactValidationError(
            "$.gates.overall-wall-time-ratio: task counters are inconsistent"
        )
    if (
        harmful["denominator"] != harmful_support["denominator"]
        or repeated_gate["denominator"] != repeated_support["denominator"]
        or omission["denominator"] != omission_support["denominator"]
        or actionable["denominator"] != next_support["denominator"]
        or contradiction["denominator"] != omission_support["denominator"]
    ):
        raise ArtifactValidationError("$.gates: policy denominators are inconsistent")
    if (
        judge["denominator"] != counts["doubleReviewedAnnotations"]
        or judge["denominator"] - judge["numerator"] != counts["judgeDisagreements"]
    ):
        raise ArtifactValidationError(
            "$.gates.judge-agreement: counters differ from summary counts"
        )
    discordant = diagnostics["mcnemarB"] + diagnostics["mcnemarC"]
    expected_p = 1.0
    if discordant:
        tail = sum(
            math.comb(discordant, index)
            for index in range(
                min(diagnostics["mcnemarB"], diagnostics["mcnemarC"]) + 1
            )
        ) / (2**discordant)
        expected_p = min(1.0, 2 * tail)
    if diagnostics["mcnemarPValue"] != expected_p:
        raise ArtifactValidationError(
            "$.diagnostics.mcnemarPValue: differs from b and c"
        )
