#!/usr/bin/env python3
"""Deterministic, standard-library evaluator for Agentic Compact v0.2."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Any

from eval_contract import (
    ArtifactValidationError,
    DEFAULT_MANIFEST,
    EvaluationError,
    RestrictedSchema,
    canonical_json,
    load_json,
    load_schemas,
    sha256_file,
    validate_manifest,
    validate_prompt_files,
    validate_result_tree,
)
from eval_metrics import derive_summary, validate_summary
from eval_selftest import run_self_test


def _validated_inputs(
    root: Path, candidate: str, schemas: dict[str, RestrictedSchema]
) -> tuple[
    dict[tuple[str, str, str, int], dict[str, Any]],
    list[dict[str, Any]],
    dict[str, Any],
    str,
]:
    manifest = load_json(DEFAULT_MANIFEST)
    validate_manifest(manifest, schemas["manifest"])
    validate_prompt_files(manifest)
    runs, annotations = validate_result_tree(root, candidate, DEFAULT_MANIFEST, schemas)
    return runs, annotations, manifest, sha256_file(DEFAULT_MANIFEST)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    operation = parser.add_mutually_exclusive_group(required=True)
    operation.add_argument("--self-test", action="store_true")
    operation.add_argument("--validate-manifest", type=Path)
    operation.add_argument("--validate-results", type=Path)
    operation.add_argument("--write-summary", type=Path)
    operation.add_argument("--validate-summary", type=Path)
    parser.add_argument("--candidate")
    parser.add_argument("--output", type=Path)
    arguments = parser.parse_args(argv)

    try:
        schemas = load_schemas()
        if arguments.self_test:
            run_self_test()
            print("evaluation self-test: ok")
            return 0
        if arguments.validate_manifest is not None:
            manifest = load_json(arguments.validate_manifest)
            validate_manifest(manifest, schemas["manifest"])
            validate_prompt_files(manifest)
            print(f"manifest valid: {sha256_file(arguments.validate_manifest)}")
            return 0
        if arguments.validate_summary is not None:
            summary = load_json(arguments.validate_summary)
            validate_summary(summary, schemas["summary"])
            if not summary["overallPass"]:
                raise ArtifactValidationError("summary does not pass every release gate")
            print("evaluation summary: valid and passing")
            return 0
        if arguments.candidate is None:
            parser.error("--candidate is required for result validation and summary generation")
        root = arguments.validate_results or arguments.write_summary
        assert root is not None
        runs, annotations, manifest, manifest_sha256 = _validated_inputs(root, arguments.candidate, schemas)
        if arguments.validate_results is not None:
            print(f"evaluation results valid: {len(runs)} runs, {len(annotations)} annotations")
            return 0
        if arguments.output is None:
            parser.error("--output is required with --write-summary")
        summary = derive_summary(runs, annotations, manifest, manifest_sha256, arguments.candidate)
        validate_summary(summary, schemas["summary"])
        if not summary["overallPass"]:
            raise ArtifactValidationError("candidate does not pass every frozen release gate")
        arguments.output.parent.mkdir(parents=True, exist_ok=True)
        arguments.output.write_text(canonical_json(summary) + "\n", encoding="utf-8")
        print(f"evaluation summary written: {arguments.output.name}")
        return 0
    except EvaluationError as error:
        print(f"evaluation failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
