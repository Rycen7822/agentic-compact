"""Frozen artifact contracts for the Agentic Compact v0.2 evaluator."""

from __future__ import annotations

import hashlib
import json
import math
import re
from pathlib import Path
from typing import Any

SCHEMA_DIRECTORY = Path(__file__).with_name("schemas")
DEFAULT_MANIFEST = Path(__file__).with_name("manifests") / "v0.2.json"
PROMPT_FILES = {
    "trigger": Path(__file__).with_name("prompts") / "trigger-judge.md",
    "checkpoint": Path(__file__).with_name("prompts") / "checkpoint-judge.md",
    "continuation": Path(__file__).with_name("prompts") / "continuation-judge.md",
}
SCHEMA_FILES = {
    "manifest": "task-manifest.schema.json",
    "run": "normalized-run.schema.json",
    "annotation": "annotation.schema.json",
    "summary": "evaluation-summary.schema.json",
}
SCHEMA_DIALECT = "https://json-schema.org/draft/2020-12/schema"
SCHEMA_KEYWORDS = frozenset(
    {
        "$schema",
        "$defs",
        "$ref",
        "title",
        "description",
        "type",
        "const",
        "enum",
        "required",
        "properties",
        "additionalProperties",
        "items",
        "minItems",
        "maxItems",
        "uniqueItems",
        "minLength",
        "maxLength",
        "pattern",
        "minimum",
        "maximum",
        "anyOf",
        "oneOf",
        "allOf",
    }
)
JSON_TYPES = frozenset(
    {"object", "array", "string", "integer", "number", "boolean", "null"}
)
COMMIT_PATTERN = re.compile(r"^[0-9a-f]{40}$")
V01_SOURCE_COMMIT = "898061f019ddca8599debd7b15a204040bd6b349"
RESET2_SELECTION_RULE = (
    "reset2-swe-v1:native15:heldsemantic20:held50:devsemantic20"
)
RESET2_CORPUS_PROVENANCE_SHA256 = (
    "1572d1a100b5c40da2f2ea45befe59341b26629632d9b03e969e00bec263b871"
)
ABSOLUTE_PATH_PATTERN = re.compile(
    r"(?:^|[\s'\"(=])(?:/[A-Za-z0-9._-]|~[\\/]|[A-Za-z]:[\\/]|\\\\[^\\/\s]+[\\/])"
)
DENIED_KEYS = frozenset({"arguments", "command", "content", "diff", "patch", "path"})
ALLOWED_OPAQUE_IDS = frozenset(
    {"annotationId", "backendId", "manifestId", "runId", "taskId"}
)
EXPECTED_THRESHOLDS = {
    "bootstrapSamples": 10_000,
    "maximumSolvedTaskDeficit": 1,
    "minimumObservedQualityDifference": -0.02,
    "minimumQualityLowerBound": -0.10,
    "minimumCompactSubsetTasks": 15,
    "maximumCompactSubsetMedian": 0.90,
    "maximumCompactSubsetUpperBound": 0.97,
    "maximumBlendedTokenRatio": 1.03,
    "maximumMedianWallRatio": 1.10,
    "minimumPolicyTasks": 15,
    "minimumPolicyRequests": 15,
    "maximumHarmfulTriggerRate": 0.05,
    "maximumRepeatedSettledPhaseRate": 0.05,
    "maximumCriticalOmissionRate": 0.05,
    "minimumActionableNextActionRate": 0.95,
    "minimumHostEvidenceRecall": 1.0,
}


class EvaluationError(Exception):
    """Base class for safe evaluator failures."""


class SchemaDefinitionError(EvaluationError):
    """The repository schema uses a keyword outside the frozen subset."""


class ArtifactValidationError(EvaluationError):
    """An artifact does not satisfy its repository schema."""


def load_json(path: Path) -> Any:
    def reject_constant(_: str) -> None:
        raise ValueError("non-finite JSON number")

    try:
        with path.open("r", encoding="utf-8") as handle:
            return json.load(handle, parse_constant=reject_constant)
    except (OSError, json.JSONDecodeError, UnicodeError, ValueError) as error:
        raise EvaluationError(
            f"cannot read strict JSON artifact: {path.name}"
        ) from error


def canonical_json(value: Any) -> str:
    return json.dumps(
        value, ensure_ascii=True, sort_keys=True, separators=(",", ":"), allow_nan=False
    )


def _schema_path(path: str, component: str) -> str:
    return f"{path}/{component}"


def _check_schema_definition(schema: Any, path: str = "$") -> None:
    if not isinstance(schema, dict):
        raise SchemaDefinitionError(f"{path}: schema node must be an object")
    unknown = set(schema).difference(SCHEMA_KEYWORDS)
    if unknown:
        raise SchemaDefinitionError(f"{path}: schema uses an unknown keyword")
    if "$schema" in schema and schema["$schema"] != SCHEMA_DIALECT:
        raise SchemaDefinitionError(f"{path}: unsupported schema dialect")
    if "$ref" in schema:
        reference = schema["$ref"]
        if not isinstance(reference, str) or not re.fullmatch(
            r"#/\$defs/[A-Za-z][A-Za-z0-9]*", reference
        ):
            raise SchemaDefinitionError(
                f"{path}: only direct local $defs references are allowed"
            )
    if "$defs" in schema:
        definitions = schema["$defs"]
        if not isinstance(definitions, dict) or not definitions:
            raise SchemaDefinitionError(f"{path}: $defs must be a non-empty object")
        for name, definition in definitions.items():
            if not re.fullmatch(r"[A-Za-z][A-Za-z0-9]*", name):
                raise SchemaDefinitionError(f"{path}: invalid local definition name")
            _check_schema_definition(definition, _schema_path(path, f"$defs/{name}"))
    if "type" in schema:
        declared = schema["type"]
        declared_types = [declared] if isinstance(declared, str) else declared
        if (
            not isinstance(declared_types, list)
            or not declared_types
            or any(
                not isinstance(item, str) or item not in JSON_TYPES
                for item in declared_types
            )
            or len(set(declared_types)) != len(declared_types)
        ):
            raise SchemaDefinitionError(f"{path}: invalid type declaration")
    if "enum" in schema:
        values = schema["enum"]
        if not isinstance(values, list) or not values:
            raise SchemaDefinitionError(f"{path}: enum must be a non-empty array")
        if len({canonical_json(value) for value in values}) != len(values):
            raise SchemaDefinitionError(f"{path}: enum values must be unique")
    if "required" in schema:
        required = schema["required"]
        if (
            not isinstance(required, list)
            or any(not isinstance(item, str) for item in required)
            or len(set(required)) != len(required)
        ):
            raise SchemaDefinitionError(f"{path}: required must contain unique strings")
    if "properties" in schema:
        properties = schema["properties"]
        if not isinstance(properties, dict):
            raise SchemaDefinitionError(f"{path}: properties must be an object")
        undeclared = set(schema.get("required", ())).difference(properties)
        if undeclared:
            raise SchemaDefinitionError(
                f"{path}: required names must be declared in properties"
            )
        for name, definition in properties.items():
            _check_schema_definition(
                definition, _schema_path(path, f"properties/{name}")
            )
    if "additionalProperties" in schema and not isinstance(
        schema["additionalProperties"], bool
    ):
        raise SchemaDefinitionError(f"{path}: additionalProperties must be boolean")
    if "items" in schema:
        _check_schema_definition(schema["items"], _schema_path(path, "items"))
    for keyword in ("anyOf", "oneOf", "allOf"):
        if keyword in schema:
            branches = schema[keyword]
            if not isinstance(branches, list) or not branches:
                raise SchemaDefinitionError(
                    f"{path}: {keyword} must be a non-empty array"
                )
            for index, branch in enumerate(branches):
                _check_schema_definition(
                    branch, _schema_path(path, f"{keyword}/{index}")
                )
    for keyword in ("minItems", "maxItems", "minLength", "maxLength"):
        if keyword in schema and (
            not isinstance(schema[keyword], int)
            or isinstance(schema[keyword], bool)
            or schema[keyword] < 0
        ):
            raise SchemaDefinitionError(
                f"{path}: {keyword} must be a non-negative integer"
            )
    for lower, upper in (("minItems", "maxItems"), ("minLength", "maxLength")):
        if lower in schema and upper in schema and schema[lower] > schema[upper]:
            raise SchemaDefinitionError(f"{path}: {lower} exceeds {upper}")
    if "uniqueItems" in schema and not isinstance(schema["uniqueItems"], bool):
        raise SchemaDefinitionError(f"{path}: uniqueItems must be boolean")
    if "pattern" in schema:
        if not isinstance(schema["pattern"], str):
            raise SchemaDefinitionError(f"{path}: pattern must be a string")
        try:
            re.compile(schema["pattern"])
        except re.error as error:
            raise SchemaDefinitionError(f"{path}: invalid pattern") from error
    for keyword in ("minimum", "maximum"):
        value = schema.get(keyword)
        if value is not None and (
            not isinstance(value, (int, float))
            or isinstance(value, bool)
            or not math.isfinite(value)
        ):
            raise SchemaDefinitionError(f"{path}: {keyword} must be a finite number")
    if (
        "minimum" in schema
        and "maximum" in schema
        and schema["minimum"] > schema["maximum"]
    ):
        raise SchemaDefinitionError(f"{path}: minimum exceeds maximum")


def _matches_type(value: Any, declared: str) -> bool:
    if declared == "object":
        return isinstance(value, dict)
    if declared == "array":
        return isinstance(value, list)
    if declared == "string":
        return isinstance(value, str)
    if declared == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if declared == "number":
        return (
            isinstance(value, (int, float))
            and not isinstance(value, bool)
            and math.isfinite(value)
        )
    if declared == "boolean":
        return isinstance(value, bool)
    return value is None


class RestrictedSchema:
    def __init__(self, schema: Any) -> None:
        _check_schema_definition(schema)
        self.root = schema

    @classmethod
    def from_file(cls, path: Path) -> "RestrictedSchema":
        return cls(load_json(path))

    def validate(self, value: Any) -> None:
        self._validate(value, self.root, "$")

    def _resolve(self, reference: str) -> dict[str, Any]:
        name = reference.removeprefix("#/$defs/")
        definition = self.root.get("$defs", {}).get(name)
        if not isinstance(definition, dict):
            raise SchemaDefinitionError("$ref targets a missing local definition")
        return definition

    def _validate(self, value: Any, schema: dict[str, Any], path: str) -> None:
        if "$ref" in schema:
            self._validate(value, self._resolve(schema["$ref"]), path)
        for keyword in ("allOf", "anyOf", "oneOf"):
            if keyword not in schema:
                continue
            matches = 0
            for branch in schema[keyword]:
                try:
                    self._validate(value, branch, path)
                    matches += 1
                except ArtifactValidationError:
                    pass
            if keyword == "allOf" and matches != len(schema[keyword]):
                raise ArtifactValidationError(f"{path}: allOf constraint failed")
            if keyword == "anyOf" and matches == 0:
                raise ArtifactValidationError(f"{path}: anyOf constraint failed")
            if keyword == "oneOf" and matches != 1:
                raise ArtifactValidationError(f"{path}: oneOf constraint failed")
        if "const" in schema and canonical_json(value) != canonical_json(
            schema["const"]
        ):
            raise ArtifactValidationError(f"{path}: const constraint failed")
        if "enum" in schema and canonical_json(value) not in {
            canonical_json(item) for item in schema["enum"]
        }:
            raise ArtifactValidationError(f"{path}: enum constraint failed")
        if "type" in schema:
            declared = schema["type"]
            declared_types = [declared] if isinstance(declared, str) else declared
            if not any(_matches_type(value, item) for item in declared_types):
                raise ArtifactValidationError(f"{path}: wrong JSON type")
        if isinstance(value, dict):
            required = schema.get("required", [])
            if any(name not in value for name in required):
                raise ArtifactValidationError(f"{path}: missing required property")
            properties = schema.get("properties", {})
            if schema.get("additionalProperties") is False and set(value).difference(
                properties
            ):
                raise ArtifactValidationError(f"{path}: contains undeclared properties")
            for name, item in value.items():
                if name in properties:
                    self._validate(item, properties[name], _schema_path(path, name))
        if isinstance(value, list):
            if "minItems" in schema and len(value) < schema["minItems"]:
                raise ArtifactValidationError(f"{path}: too few array items")
            if "maxItems" in schema and len(value) > schema["maxItems"]:
                raise ArtifactValidationError(f"{path}: too many array items")
            if schema.get("uniqueItems") and len(
                {canonical_json(item) for item in value}
            ) != len(value):
                raise ArtifactValidationError(f"{path}: array items must be unique")
            if "items" in schema:
                for index, item in enumerate(value):
                    self._validate(
                        item, schema["items"], _schema_path(path, str(index))
                    )
        if isinstance(value, str):
            if "minLength" in schema and len(value) < schema["minLength"]:
                raise ArtifactValidationError(f"{path}: string is too short")
            if "maxLength" in schema and len(value) > schema["maxLength"]:
                raise ArtifactValidationError(f"{path}: string is too long")
            if "pattern" in schema and re.search(schema["pattern"], value) is None:
                raise ArtifactValidationError(
                    f"{path}: string pattern constraint failed"
                )
        if isinstance(value, (int, float)) and not isinstance(value, bool):
            if "minimum" in schema and value < schema["minimum"]:
                raise ArtifactValidationError(f"{path}: number is below minimum")
            if "maximum" in schema and value > schema["maximum"]:
                raise ArtifactValidationError(f"{path}: number is above maximum")


def load_schemas() -> dict[str, RestrictedSchema]:
    return {
        name: RestrictedSchema.from_file(SCHEMA_DIRECTORY / filename)
        for name, filename in SCHEMA_FILES.items()
    }


def sha256_json(value: Any) -> str:
    return hashlib.sha256(canonical_json(value).encode("ascii")).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    try:
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(128 * 1024), b""):
                digest.update(chunk)
    except OSError as error:
        raise EvaluationError(f"cannot hash artifact: {path.name}") from error
    return digest.hexdigest()


def validate_privacy(value: Any, path: str = "$") -> None:
    if isinstance(value, dict):
        for key, item in value.items():
            normalized = re.sub(r"[-_]", "", key).lower()
            if normalized in DENIED_KEYS:
                raise ArtifactValidationError(f"{path}: contains a denied field")
            if key.lower().endswith("id") and key not in ALLOWED_OPAQUE_IDS:
                raise ArtifactValidationError(
                    f"{path}: contains an unhashed identifier field"
                )
            validate_privacy(item, _schema_path(path, key))
        return
    if isinstance(value, list):
        for index, item in enumerate(value):
            validate_privacy(item, _schema_path(path, str(index)))
        return
    if isinstance(value, str) and ABSOLUTE_PATH_PATTERN.search(value):
        raise ArtifactValidationError(f"{path}: contains an absolute path")


def validate_manifest(manifest: Any, schema: RestrictedSchema) -> dict[str, Any]:
    schema.validate(manifest)
    validate_privacy(manifest)
    if manifest["selectionRule"] != RESET2_SELECTION_RULE:
        raise ArtifactValidationError("$.selectionRule: differs from reset-2 freeze")
    if manifest["corpusProvenanceSha256"] != RESET2_CORPUS_PROVENANCE_SHA256:
        raise ArtifactValidationError(
            "$.corpusProvenanceSha256: differs from reset-2 freeze"
        )
    tasks = manifest["tasks"]
    ids = [task["taskId"] for task in tasks]
    if len(set(ids)) != len(ids):
        raise ArtifactValidationError("$.tasks: task IDs must be unique")

    dev = [task for task in tasks if task["role"] == "dev"]
    held_out = [task for task in tasks if task["role"] == "held-out"]
    if len(dev) != 20 or len(held_out) != 50:
        raise ArtifactValidationError(
            "$.tasks: expected exactly 20 dev and 50 held-out tasks"
        )
    benchmarks = {task["benchmark"] for task in tasks}
    if benchmarks != {"swe-bench-verified"}:
        raise ArtifactValidationError(
            "$.tasks: reset-2 tasks must all come from SWE-bench Verified"
        )
    if not all(task["semanticBundleEligible"] for task in dev):
        raise ArtifactValidationError(
            "$.tasks: every dev task must be semantic-bundle eligible"
        )
    if sum(task["semanticBundleEligible"] for task in held_out) < 20:
        raise ArtifactValidationError(
            "$.tasks: held-out semantic support is below 20 tasks"
        )
    if sum(task["fallbackExposed"] for task in held_out) < 15:
        raise ArtifactValidationError(
            "$.tasks: held-out fallback support is below 15 tasks"
        )
    for task in tasks:
        expected_evidence = [
            "native-fallback"
            if task["fallbackExposed"]
            else "active-context-pressure"
        ]
        if task["semanticBundleEligible"]:
            expected_evidence.append("semantic-bundle")
        if task["selectionEvidence"] != expected_evidence:
            raise ArtifactValidationError(
                "$.tasks: selection evidence does not match the canonical labels"
            )
    diagnostic = [task for task in tasks if task["diagnosticSubset"]]
    if len(diagnostic) != 20 or any(task["role"] != "held-out" for task in diagnostic):
        raise ArtifactValidationError(
            "$.tasks: diagnostic subset must be 20 held-out tasks"
        )
    if not all(task["semanticBundleEligible"] for task in diagnostic):
        raise ArtifactValidationError(
            "$.tasks: every diagnostic task must be semantic-bundle eligible"
        )

    runtime = manifest["runtime"]
    window = runtime["targetContextWindow"]
    if window != 258_400:
        raise ArtifactValidationError(
            "$.runtime.targetContextWindow: does not match frozen Luna evidence"
        )
    no_forced = runtime["noForcedLimit"]
    if no_forced is not None and no_forced != math.floor(0.90 * window):
        raise ArtifactValidationError(
            "$.runtime.noForcedLimit: does not match the frozen formula"
        )
    if manifest["judge"]["doubleReviewRate"] != 0.10:
        raise ArtifactValidationError("$.judge.doubleReviewRate: must equal 0.10")
    if manifest["judge"]["minimumAgreement"] != 0.90:
        raise ArtifactValidationError("$.judge.minimumAgreement: must equal 0.90")
    if manifest["v01Baseline"]["sourceCommit"] != V01_SOURCE_COMMIT:
        raise ArtifactValidationError(
            "$.v01Baseline.sourceCommit: differs from the public v0.1.0 release"
        )
    if manifest["thresholds"] != EXPECTED_THRESHOLDS:
        raise ArtifactValidationError(
            "$.thresholds: values differ from the frozen release gates"
        )
    return {task["taskId"]: task for task in tasks}


def validate_prompt_files(manifest: dict[str, Any]) -> None:
    expected = manifest["judge"]["promptSha256"]
    for stream, path in PROMPT_FILES.items():
        if sha256_file(path) != expected[stream]:
            raise ArtifactValidationError(
                f"$.judge.promptSha256.{stream}: differs from the frozen prompt file"
            )


def _validate_runtime(run: dict[str, Any], manifest: dict[str, Any]) -> None:
    runtime = manifest["runtime"]
    for field in (
        "codexVersion",
        "model",
        "reasoningEffort",
        "serviceTier",
        "sandboxMode",
        "approvalPolicy",
    ):
        if run[field] != runtime[field]:
            raise ArtifactValidationError(
                f"$.{field}: differs from the frozen manifest"
            )
    expected_limit = runtime["fixedFallbackLimit"]
    if run["contextRegime"] == "no-forced":
        expected_limit = runtime["noForcedLimit"]
        if expected_limit is None:
            raise ArtifactValidationError(
                "$.contextRegime: no-forced regime was not frozen"
            )
    if run["resolvedContextLimit"] != expected_limit:
        raise ArtifactValidationError(
            "$.resolvedContextLimit: differs from the frozen manifest"
        )
    hashes = run["configHashes"]
    if hashes["codexBinarySha256"] != runtime["codexBinarySha256"]:
        raise ArtifactValidationError(
            "$.configHashes.codexBinarySha256: differs from the manifest"
        )
    if hashes["configSha256"] != runtime["runtimeConfigSha256"]:
        raise ArtifactValidationError(
            "$.configHashes.configSha256: differs from the manifest"
        )


def _validate_transition(transition: dict[str, Any], expected_ordinal: int) -> None:
    if transition["ordinal"] != expected_ordinal:
        raise ArtifactValidationError(
            "$.transitions: ordinals must be contiguous and ordered"
        )
    if transition["preCompactionInputTokens"] <= 0:
        raise ArtifactValidationError(
            "$.transitions: source token-usage evidence must be positive"
        )
    if transition["terminalState"] != "cooldown":
        if any(
            transition[field] is not None
            for field in (
                "postCompactionInputTokens",
                "postCompactionUsageEventHash",
            )
        ):
            raise ArtifactValidationError(
                "$.transitions: unsuccessful transition contains post-compaction evidence"
            )
        return
    required = (
        "compactionTurnHash",
        "compactionItemHash",
        "compactionPosition",
        "continuationTurnHash",
        "continuationPosition",
        "postCompactionInputTokens",
        "postCompactionUsageEventHash",
        "continuationKind",
        "checkpointSha256",
        "continuityViewSha256",
    )
    if any(transition[field] is None for field in required):
        raise ArtifactValidationError(
            "$.transitions: successful transition is incomplete"
        )
    if (
        transition["compactionPosition"] != transition["sourcePosition"] + 1
        or transition["continuationPosition"] != transition["compactionPosition"] + 1
    ):
        raise ArtifactValidationError(
            "$.transitions: source, compact, and continuation order is invalid"
        )
    if transition["postCompactionInputTokens"] <= 0:
        raise ArtifactValidationError(
            "$.transitions: continuation token-usage evidence must be positive"
        )


def validate_run(
    run: Any,
    schema: RestrictedSchema,
    manifest: dict[str, Any],
    tasks: dict[str, dict[str, Any]],
    manifest_sha256: str,
    candidate: str,
) -> None:
    schema.validate(run)
    validate_privacy(run)
    if run["candidateCommit"] != candidate:
        raise ArtifactValidationError(
            "$.candidateCommit: does not match the requested candidate"
        )
    task = tasks.get(run["taskId"])
    if task is None or run["benchmark"] != task["benchmark"]:
        raise ArtifactValidationError(
            "$.taskId: missing from manifest or benchmark mismatch"
        )
    if run["configHashes"]["taskManifestSha256"] != manifest_sha256:
        raise ArtifactValidationError(
            "$.configHashes.taskManifestSha256: differs from the manifest artifact"
        )
    _validate_runtime(run, manifest)
    prior_reasons = run["priorInvalidBlocks"]
    reason_codes = [item["reasonCode"] for item in prior_reasons]
    if len(set(reason_codes)) != len(reason_codes) or any(
        item["count"] < 1 for item in prior_reasons
    ):
        raise ArtifactValidationError(
            "$.priorInvalidBlocks: reasons must be unique with positive counts"
        )
    if sum(item["count"] for item in prior_reasons) != run["attempt"] - 1:
        raise ArtifactValidationError(
            "$.priorInvalidBlocks: counts do not match the final attempt"
        )
    hashes = run["configHashes"]
    surface_fields = ("binarySha256", "pluginSha256", "surfaceSha256")
    capability_field = "capabilityRecordSha256"
    if run["arm"] == "stock" and any(
        hashes[field] is not None for field in (*surface_fields, capability_field)
    ):
        raise ArtifactValidationError(
            "$.configHashes: stock arm contains Agentic Compact artifact hashes"
        )
    if run["arm"] != "stock" and any(hashes[field] is None for field in surface_fields):
        raise ArtifactValidationError(
            "$.configHashes: non-stock arm lacks a frozen artifact hash"
        )
    if (run["arm"] in {"v0.1", "v0.2"}) != (
        hashes[capability_field] is not None
    ):
        raise ArtifactValidationError(
            "$.configHashes: capability record presence differs from the arm contract"
        )
    if run["arm"] == "v0.1" and any(
        hashes[field] != manifest["v01Baseline"][field]
        for field in (*surface_fields, capability_field)
    ):
        raise ArtifactValidationError(
            "$.configHashes: v0.1 arm differs from the frozen baseline"
        )

    tokens = run["tokens"]
    if tokens["nonCachedInputTokens"] != max(
        tokens["inputTokens"] - tokens["cachedInputTokens"], 0
    ):
        raise ArtifactValidationError(
            "$.tokens.nonCachedInputTokens: derived value is incorrect"
        )
    if tokens["blendedTokens"] != tokens["nonCachedInputTokens"] + max(
        tokens["outputTokens"], 0
    ):
        raise ArtifactValidationError(
            "$.tokens.blendedTokens: derived value is incorrect"
        )
    if tokens["reasoningOutputTokens"] > tokens["outputTokens"]:
        raise ArtifactValidationError(
            "$.tokens.reasoningOutputTokens: exceeds output tokens"
        )
    if run["solved"] != math.isclose(
        run["benchmarkScore"], 1.0, rel_tol=0.0, abs_tol=1e-12
    ):
        raise ArtifactValidationError(
            "$.benchmarkScore: solved must equal an exact full benchmark score"
        )

    observer = run["observer"]
    if (
        not all(
            observer[field]
            for field in (
                "finalTokenUsagePresent",
                "finalOutcomePresent",
                "counterMonotonic",
                "threadConsistent",
            )
        )
        or observer["targetThreadCount"] != 1
    ):
        raise ArtifactValidationError(
            "$.observer: target-thread observation is incomplete"
        )
    if (
        observer["tokenUsageSamples"] < 1
        or not 0 < observer["peakActiveInputTokens"] <= tokens["inputTokens"]
        or observer["peakActiveInputTokens"] > observer["modelContextWindow"]
    ):
        raise ArtifactValidationError(
            "$.observer: direct context telemetry does not reconcile with final tokens"
        )
    counters = run["counters"]
    if counters["toolItemsCompleted"] > counters["itemsCompleted"]:
        raise ArtifactValidationError(
            "$.counters: completed tool items exceed completed items"
        )
    if counters["scheduledAgenticRequests"] > counters["agenticRequests"]:
        raise ArtifactValidationError(
            "$.counters: scheduled requests exceed agentic requests"
        )
    if counters["agenticTransitions"] > counters["scheduledAgenticRequests"]:
        raise ArtifactValidationError(
            "$.counters: transitions exceed scheduled requests"
        )
    if (
        counters["unattributedCompactions"]
        != counters["contextCompactions"] - counters["agenticTransitions"]
    ):
        raise ArtifactValidationError(
            "$.counters.unattributedCompactions: attribution formula failed"
        )
    rejected = sum(item["count"] for item in run["rejections"])
    if rejected != counters["agenticRequests"] - counters["scheduledAgenticRequests"]:
        raise ArtifactValidationError(
            "$.rejections: counts do not cover rejected requests"
        )
    if len(run["transitions"]) != counters["scheduledAgenticRequests"]:
        raise ArtifactValidationError(
            "$.transitions: count differs from scheduledAgenticRequests"
        )
    successful_transitions = sum(
        item["terminalState"] == "cooldown" for item in run["transitions"]
    )
    if successful_transitions != counters["agenticTransitions"]:
        raise ArtifactValidationError(
            "$.transitions: successful count differs from agenticTransitions"
        )
    if run["contextRegime"] == "no-forced" and counters["unattributedCompactions"] != 0:
        raise ArtifactValidationError(
            "$.counters.unattributedCompactions: invalid in no-forced regime"
        )
    if run["arm"] == "stock" and any(
        counters[field]
        for field in (
            "agenticRequests",
            "scheduledAgenticRequests",
            "agenticTransitions",
        )
    ):
        raise ArtifactValidationError("$.counters: stock arm contains agentic activity")
    if run["arm"] == "stock" and run["rejections"]:
        raise ArtifactValidationError(
            "$.rejections: stock arm contains agentic rejections"
        )
    if run["arm"] == "surface-sham" and (
        counters["scheduledAgenticRequests"] != 0 or counters["agenticTransitions"] != 0
    ):
        raise ArtifactValidationError("$.counters: surface sham scheduled a transition")
    if run["arm"] == "surface-sham" and any(
        item["reasonCode"] != "shared_app_server_unavailable"
        for item in run["rejections"]
    ):
        raise ArtifactValidationError(
            "$.rejections: surface sham used a non-production reason"
        )

    compaction_hashes: set[str] = set()
    source_hashes: set[str] = set()
    journal_hashes: set[str] = set()
    pre_usage_hashes: set[str] = set()
    post_usage_hashes: set[str] = set()
    last_position = -1
    for ordinal, transition in enumerate(run["transitions"], start=1):
        _validate_transition(transition, ordinal)
        if transition["sourcePosition"] <= last_position:
            raise ArtifactValidationError(
                "$.transitions: sources are not strictly chronological"
            )
        last_position = transition["sourcePosition"]
        if transition["terminalState"] == "cooldown":
            last_position = transition["continuationPosition"]
        if (
            transition["sourceItemHash"] in source_hashes
            or transition["journalSha256"] in journal_hashes
            or transition["preCompactionUsageEventHash"] in pre_usage_hashes
        ):
            raise ArtifactValidationError(
                "$.transitions: source, pre-usage event, or journal was reused"
            )
        source_hashes.add(transition["sourceItemHash"])
        journal_hashes.add(transition["journalSha256"])
        pre_usage_hashes.add(transition["preCompactionUsageEventHash"])
        compaction_hash = (
            transition["compactionItemHash"]
            if transition["terminalState"] == "cooldown"
            else None
        )
        if compaction_hash is not None and compaction_hash in compaction_hashes:
            raise ArtifactValidationError(
                "$.transitions: compaction item was attributed more than once"
            )
        if compaction_hash is not None:
            compaction_hashes.add(compaction_hash)
        post_usage_hash = transition["postCompactionUsageEventHash"]
        if post_usage_hash is not None and post_usage_hash in post_usage_hashes:
            raise ArtifactValidationError(
                "$.transitions: post-compaction usage event was reused"
            )
        if post_usage_hash is not None:
            post_usage_hashes.add(post_usage_hash)
    observed_safety = {
        "crossThreadActions": sum(
            not item["sameThread"] for item in run["transitions"]
        ),
        "processSurvivalFailures": sum(
            not item["processSurvived"] for item in run["transitions"]
        ),
        "atMostOnceFailures": sum(
            item["injectionCount"] > 1 or item["continuationCount"] > 1
            for item in run["transitions"]
        ),
        "lostCheckpoints": sum(
            item["terminalState"] == "cooldown" and item["injectionCount"] == 0
            for item in run["transitions"]
        ),
        "duplicateCheckpoints": sum(
            item["injectionCount"] > 1 for item in run["transitions"]
        ),
    }
    for field, observed in observed_safety.items():
        if run["safety"][field] < observed:
            raise ArtifactValidationError(
                f"$.safety.{field}: omits an observable transition failure"
            )
    if run["solved"] and run["failureClass"] is not None:
        raise ArtifactValidationError(
            "$.failureClass: solved run cannot carry a failure class"
        )
    if (run["failureClass"] is not None) != run["failureReviewed"]:
        raise ArtifactValidationError(
            "$.failureReviewed: must match the presence of a reviewed failure class"
        )
    if run["candidateCaused"] and (
        run["failureClass"] is None or not run["failureReviewed"]
    ):
        raise ArtifactValidationError(
            "$.candidateCaused: candidate failure lacks reviewed classification"
        )
    if run["candidateCaused"] and run["arm"] != "v0.2":
        raise ArtifactValidationError(
            "$.candidateCaused: only the candidate arm may carry this attribution"
        )


def validate_annotation(
    annotation: Any,
    schema: RestrictedSchema,
    manifest: dict[str, Any],
    run: dict[str, Any],
    candidate: str,
) -> None:
    schema.validate(annotation)
    validate_privacy(annotation)
    for field in ("runId", "taskId", "contextRegime", "arm", "repetition"):
        if annotation[field] != run[field]:
            raise ArtifactValidationError(f"$.{field}: differs from the normalized run")
    if annotation["candidateCommit"] != candidate:
        raise ArtifactValidationError(
            "$.candidateCommit: differs from the requested candidate"
        )
    stream = annotation["stream"]
    allowed_labels = {
        "trigger": {"KEEP", "DELETE", "INSERT", "MOVE_EARLIER"},
        "checkpoint": {"KEEP", "REPAIR"},
        "continuation": {"KEEP", "REPAIR"},
    }[stream]
    for field in ("primary", "final"):
        if annotation[field] not in allowed_labels:
            raise ArtifactValidationError(
                f"$.{field}: label is invalid for the annotation stream"
            )
    for field in ("secondary", "adjudicated"):
        if annotation[field] is not None and annotation[field] not in allowed_labels:
            raise ArtifactValidationError(
                f"$.{field}: label is invalid for the annotation stream"
            )
    if annotation["doubleReviewed"]:
        if annotation["secondary"] is None or annotation["adjudicated"] is None:
            raise ArtifactValidationError(
                "$: double-reviewed annotation lacks secondary or adjudicated label"
            )
        if annotation["final"] != annotation["adjudicated"]:
            raise ArtifactValidationError("$.final: must equal the adjudicated label")
    elif annotation["secondary"] is not None or annotation["adjudicated"] is not None:
        raise ArtifactValidationError(
            "$: single-reviewed annotation contains secondary review fields"
        )
    elif annotation["final"] != annotation["primary"]:
        raise ArtifactValidationError("$.final: must equal the primary label")
    if (annotation["preferredAnchor"] is not None) != (
        annotation["final"] == "MOVE_EARLIER"
    ):
        raise ArtifactValidationError(
            "$.preferredAnchor: must exist only for MOVE_EARLIER"
        )
    source_kind = annotation["sourceAnchor"]["kind"]
    if stream == "trigger":
        expected_kind = (
            "missing-boundary" if annotation["final"] == "INSERT" else "actual-request"
        )
    else:
        expected_kind = stream
    if source_kind != expected_kind:
        raise ArtifactValidationError(
            "$.sourceAnchor.kind: differs from the stream and final label"
        )
    if annotation["preferredAnchor"] is not None:
        if annotation["preferredAnchor"]["kind"] != "preferred-boundary":
            raise ArtifactValidationError(
                "$.preferredAnchor.kind: must be preferred-boundary"
            )
        if (
            annotation["preferredAnchor"]["position"]
            >= annotation["sourceAnchor"]["position"]
        ):
            raise ArtifactValidationError(
                "$.preferredAnchor: MOVE_EARLIER anchor is not earlier"
            )
    attributes = annotation["attributes"]
    required_attributes = {
        "trigger": (
            "phaseBefore",
            "phaseAfter",
            "substantialWorkRemaining",
            "stateStable",
            "activeWork",
            "recentCompaction",
        ),
        "checkpoint": (
            "criticalContradiction",
            "criticalOmission",
            "nextActionActionable",
            "hostEvidenceMatch",
        ),
        "continuation": ("repeatedSettledPhase",),
    }[stream]
    if any(attributes[field] is None for field in required_attributes):
        raise ArtifactValidationError(
            "$.attributes: required stream attributes are unavailable"
        )
    if stream == "trigger":
        expected_taxonomy = {
            "KEEP": None,
            "DELETE": "harmful_trigger",
            "INSERT": "missed_boundary",
            "MOVE_EARLIER": "late_boundary",
        }[annotation["final"]]
    elif stream == "checkpoint":
        failures = (
            attributes["criticalContradiction"] is True
            or attributes["criticalOmission"] is True
            or attributes["nextActionActionable"] is False
            or attributes["hostEvidenceMatch"] is False
        )
        if (annotation["final"] == "KEEP") == failures:
            raise ArtifactValidationError(
                "$.final: checkpoint label contradicts its attributes"
            )
        expected_taxonomy = (
            "critical_omission"
            if attributes["criticalOmission"] is True
            else "non_actionable_next_action"
            if attributes["nextActionActionable"] is False
            else None
        )
    else:
        repeated = attributes["repeatedSettledPhase"] is True
        if annotation["final"] == "KEEP" and repeated:
            raise ArtifactValidationError(
                "$.final: continuation label contradicts its attributes"
            )
        expected_taxonomy = "repeated_settled_phase" if repeated else None
    if annotation["taxonomy"] != expected_taxonomy:
        raise ArtifactValidationError(
            "$.taxonomy: differs from the final label and attributes"
        )
    judge = manifest["judge"]
    if annotation["promptSha256"] != judge["promptSha256"][stream]:
        raise ArtifactValidationError(
            "$.promptSha256: differs from the frozen judge prompt"
        )
    if annotation["judgeConfigSha256"] != judge["judgeConfigSha256"]:
        raise ArtifactValidationError(
            "$.judgeConfigSha256: differs from the frozen judge configuration"
        )
    if not annotation["rawBundleAvailable"] or not run["semanticAvailability"][stream]:
        raise ArtifactValidationError(
            "$: annotation exists without an eligible raw semantic bundle"
        )


def expected_result_keys(manifest: dict[str, Any]) -> set[tuple[str, str, str, int]]:
    keys: set[tuple[str, str, str, int]] = set()
    held_out = [task for task in manifest["tasks"] if task["role"] == "held-out"]
    for task in held_out:
        for repetition in range(1, 4):
            for arm in ("stock", "v0.2"):
                keys.add(("fixed-fallback", arm, task["taskId"], repetition))
        if task["diagnosticSubset"]:
            for arm in ("surface-sham", "v0.1"):
                keys.add(("fixed-fallback", arm, task["taskId"], 1))
            if manifest["runtime"]["noForcedLimit"] is not None:
                for arm in ("stock", "v0.2"):
                    keys.add(("no-forced", arm, task["taskId"], 1))
    return keys


def result_path(root: Path, candidate: str, key: tuple[str, str, str, int]) -> Path:
    regime, arm, task_id, repetition = key
    return root / candidate / regime / arm / task_id / f"rep-{repetition}.json"


def validate_block_attempts(
    runs: dict[tuple[str, str, str, int], dict[str, Any]],
) -> tuple[int, int]:
    blocks: dict[tuple[str, str, int], list[dict[str, Any]]] = {}
    for (regime, _arm, task_id, repetition), run in runs.items():
        blocks.setdefault((regime, task_id, repetition), []).append(run)
    invalid_attempts = 0
    invalid_blocks = 0
    for block in blocks.values():
        attempts = {run["attempt"] for run in block}
        if len(attempts) != 1:
            raise ArtifactValidationError(
                "paired block arms were not rerun as one unit"
            )
        prior_attempts = attempts.pop() - 1
        reason_sets = {canonical_json(run["priorInvalidBlocks"]) for run in block}
        if len(reason_sets) != 1:
            raise ArtifactValidationError(
                "paired block arms disagree on prior invalid reasons"
            )
        invalid_blocks += prior_attempts
        invalid_attempts += prior_attempts * len(block)
    return invalid_attempts, invalid_blocks


def validate_result_tree(
    root: Path,
    candidate: str,
    manifest_path: Path,
    schemas: dict[str, RestrictedSchema],
) -> tuple[dict[tuple[str, str, str, int], dict[str, Any]], list[dict[str, Any]]]:
    if COMMIT_PATTERN.fullmatch(candidate) is None:
        raise ArtifactValidationError(
            "candidate must be a 40-character lowercase commit SHA"
        )
    manifest = load_json(manifest_path)
    tasks = validate_manifest(manifest, schemas["manifest"])
    manifest_sha256 = sha256_file(manifest_path)
    expected = expected_result_keys(manifest)
    runs: dict[tuple[str, str, str, int], dict[str, Any]] = {}
    run_ids: dict[str, dict[str, Any]] = {}
    for key in sorted(expected):
        path = result_path(root, candidate, key)
        run = load_json(path)
        validate_run(run, schemas["run"], manifest, tasks, manifest_sha256, candidate)
        if (run["contextRegime"], run["arm"], run["taskId"], run["repetition"]) != key:
            raise ArtifactValidationError(
                f"{path.name}: path and artifact identity differ"
            )
        if run["runId"] in run_ids:
            raise ArtifactValidationError(
                "normalized results contain a duplicate run ID"
            )
        runs[key] = run
        run_ids[run["runId"]] = run

    candidate_root = root / candidate
    actual = {
        path
        for path in candidate_root.rglob("*.json")
        if "annotations" not in path.relative_to(candidate_root).parts
    }
    expected_paths = {result_path(root, candidate, key) for key in expected}
    if actual != expected_paths:
        raise ArtifactValidationError(
            "result tree contains missing or unexpected normalized artifacts"
        )
    validate_block_attempts(runs)
    held_out = [task for task in manifest["tasks"] if task["role"] == "held-out"]
    for task in held_out:
        for repetition in range(1, 4):
            stock = runs[("fixed-fallback", "stock", task["taskId"], repetition)]
            if (
                stock["tokens"]["nonCachedInputTokens"] <= 0
                or stock["tokens"]["blendedTokens"] <= 0
            ):
                raise ArtifactValidationError(
                    "stock paired token denominator is not positive"
                )
            if stock["wallTimeMs"] <= 0:
                raise ArtifactValidationError(
                    "stock paired wall-time denominator is not positive"
                )
            candidate_run = runs[("fixed-fallback", "v0.2", task["taskId"], repetition)]
            if task["semanticBundleEligible"] and not all(
                candidate_run["semanticAvailability"].values()
            ):
                raise ArtifactValidationError(
                    "semantic-eligible candidate run lacks a frozen judge stream"
                )
    for arm in ("stock", "surface-sham", "v0.1", "v0.2"):
        arm_hashes = {
            canonical_json(run["configHashes"])
            for key, run in runs.items()
            if key[1] == arm
        }
        if len(arm_hashes) != 1:
            raise ArtifactValidationError(
                f"{arm} runs disagree on frozen artifact hashes"
            )
    for task in held_out:
        if not task["diagnosticSubset"]:
            continue
        sham = runs[("fixed-fallback", "surface-sham", task["taskId"], 1)]
        candidate_run = runs[("fixed-fallback", "v0.2", task["taskId"], 1)]
        if (
            sham["configHashes"]["surfaceSha256"]
            != candidate_run["configHashes"]["surfaceSha256"]
        ):
            raise ArtifactValidationError(
                "surface sham differs from the candidate model surface"
            )

    annotations: list[dict[str, Any]] = []
    annotation_root = candidate_root / "annotations"
    if annotation_root.exists():
        seen_annotation_ids: set[str] = set()
        seen_source_anchors: set[tuple[str, str, str]] = set()
        grouped_ordinals: dict[tuple[str, str], list[int]] = {}
        for path in sorted(annotation_root.rglob("*.json")):
            annotation = load_json(path)
            run_id = annotation.get("runId") if isinstance(annotation, dict) else None
            run = run_ids.get(run_id) if isinstance(run_id, str) else None
            if run is None:
                raise ArtifactValidationError(
                    f"{path.name}: annotation references an unknown run"
                )
            validate_annotation(
                annotation, schemas["annotation"], manifest, run, candidate
            )
            expected_path = (
                annotation_root
                / annotation["contextRegime"]
                / annotation["arm"]
                / annotation["stream"]
                / annotation["taskId"]
                / f"rep-{annotation['repetition']}-{annotation['ordinal']}.json"
            )
            if path != expected_path:
                raise ArtifactValidationError(
                    f"{path.name}: annotation path and artifact identity differ"
                )
            if annotation["annotationId"] in seen_annotation_ids:
                raise ArtifactValidationError(
                    "annotations contain a duplicate annotation ID"
                )
            seen_annotation_ids.add(annotation["annotationId"])
            anchor_key = (
                annotation["runId"],
                annotation["stream"],
                canonical_json(annotation["sourceAnchor"]),
            )
            if anchor_key in seen_source_anchors:
                raise ArtifactValidationError(
                    "annotations contain a duplicate source anchor"
                )
            seen_source_anchors.add(anchor_key)
            grouped_ordinals.setdefault(
                (annotation["runId"], annotation["stream"]), []
            ).append(annotation["ordinal"])
            annotations.append(annotation)
        for ordinals in grouped_ordinals.values():
            if sorted(ordinals) != list(range(1, len(ordinals) + 1)):
                raise ArtifactValidationError(
                    "annotation ordinals must be contiguous within each run and stream"
                )
    annotation_groups: dict[tuple[str, str], list[dict[str, Any]]] = {}
    for annotation in annotations:
        annotation_groups.setdefault(
            (annotation["runId"], annotation["stream"]), []
        ).append(annotation)
    for run in runs.values():
        successful = sum(
            item["terminalState"] == "cooldown" for item in run["transitions"]
        )
        for stream, expected_count in (
            ("trigger", run["counters"]["agenticRequests"]),
            ("checkpoint", successful),
            ("continuation", successful),
        ):
            if not run["semanticAvailability"][stream]:
                continue
            group = annotation_groups.get((run["runId"], stream), [])
            actual_count = (
                sum(item["final"] != "INSERT" for item in group)
                if stream == "trigger"
                else len(group)
            )
            if actual_count != expected_count:
                raise ArtifactValidationError(
                    f"{stream} annotations do not cover the normalized run"
                )
    return runs, annotations
