# Agentic Compact v0.2 evaluation contract

This directory defines the offline release gate for Agentic Compact v0.2. Production code never imports it, launches a judge, uploads telemetry, or enables an evaluation mode. The external release-gate harness runs tasks and writes artifacts; the repository evaluator only validates those artifacts and derives deterministic statistics.

The committed task manifest is intentionally absent until the corpus, stock baseline, judge backend, and every frozen hash are available. Do not create placeholder tasks or replace unavailable evidence with synthetic data. `python3 eval/score.py --self-test` uses in-memory fixtures only and is not benchmark evidence.

## Frozen runtime

Every arm uses Codex `0.146.0`, model `gpt-5.6-luna`, reasoning effort `high`, and the same frozen service tier, sandbox mode, approval policy, wall/token budget, environment image, dependency cache, and resolved context limit. A run that switches model or reasoning effort is invalid.

The confirmed Luna context window is `W=258400`. Reset-1 screened the descending grid `[193800, 131072, 98304, 65536, 49152]` on the fixed quarantined controls `find-network-alignments`, `make-mips-interpreter`, `circuit-fibsqrt`, `schemelike-metacircular-eval`, and `regex-chess`. The first `49152` run exposed native compaction on 5/5 controls and the identical confirmation exposed it on 3/5; both preserved the single app-server/MCP process in 5/5 runs, so the manifest schema fixes `fixed-fallback=49152`. The optional `no-forced` diagnostic remains `floor(0.90 * W)=232560`; any unattributed compaction invalidates its entire paired block.

## Responsibilities

The external harness owns all stateful execution:

1. Resolve the exact manifest task without writing its prompt, expected patch, test answer, repository path, or secret into this repository or normalized artifacts.
2. Create a clean worktree at the task base commit and an isolated `CODEX_HOME`, session store, and authentication context for every run.
3. Start the frozen Codex binary and exactly the arm selected by the manifest-seeded block order. Do not change model, effort, task environment, or budget inside a block.
4. Execute the task, observe one target thread through stable app-server events and `thread/read`, collect the final benchmark outcome and cumulative token usage, then stop the isolated runtime.
5. Validate and normalize one run artifact. If any arm is invalid, discard the whole block from final results and rerun every arm with the next shared attempt number. A block has at most three total attempts.
6. Run the frozen out-of-band judge only after trajectory completion, write eligible annotations, and remove raw bundles according to the controlled runner retention policy.

The repository evaluator does not launch Codex or a TUI, create worktrees, submit tasks, call the judge, read a session database, parse rollout SQLite, or discover configuration outside the committed manifest and explicit CLI arguments.

## Arms

| Arm | Required behavior |
|---|---|
| `stock` | Frozen Codex without the Agentic Compact plugin or MCP. Agentic request and transition counters are zero. |
| `surface-sham` | Candidate skill/default prompt/initialize/tool surface with the frozen sham MCP. Every call returns the production `shared_app_server_unavailable` non-error rejection and never compacts, injects, continues, returns `_meta`, or writes a journal. |
| `v0.1` | Exact public v0.1.0 binary, plugin, manifest, and hashes. |
| `v0.2` | Exact candidate commit, binary, plugin, surface, and configuration hashes. |

The sham and production initialize payload plus `request_compaction` title, description, input schema, and output schema must be equal as parsed JSON. There is no evaluation-only production branch or reason code.

## Observer contract

The adapter consumes stable app-server lifecycle events for exactly one target thread. Events are wake-up and ordering signals; an event-local item view is not authoritative. Read the chronological full snapshot with `thread/read(includeTurns=true, itemsView="full")` before deriving identities or transitions.

The normalized lifecycle counters are:

- `turnsStarted`: target-thread `turn/started` events.
- `itemsCompleted`: target-thread `item/completed` events.
- `toolItemsCompleted`: completed `commandExecution`, `fileChange`, `mcpToolCall`, `dynamicToolCall`, `collabAgentToolCall`, and `imageGeneration` items.
- `agenticRequests`: completed calls to the target Agentic Compact server and tool.
- `scheduledAgenticRequests`: agentic requests whose structured status is `scheduled`.
- `contextCompactions`: every `contextCompaction` item in completed turns, regardless of item status.

The observer must confirm final token usage, final outcome, monotonic counters, a stable target thread identity, and exactly one target thread. A disconnect, counter rollback, missing final field, inconsistent thread, multiple target threads, ambiguous ordering, schema failure, privacy failure, missing `traceRef`, or hash mismatch invalidates the whole paired block.

The observer hook writes a dedicated privacy-safe `context-telemetry.jsonl` containing the `last`, `total`, and `modelContextWindow` counters reported directly by each target-thread Codex `thread/tokenUsage/updated` notification plus hashed `contextCompaction` lifecycle markers. It never tokenizes or estimates from conversation, tool, or item trajectory content, and the general `projected-events.jsonl` intentionally contains no token-usage fields. Each usage-event hash is SHA-256 over its canonical telemetry JSON bytes without a newline; the telemetry hash is SHA-256 over all canonical telemetry records in order with one trailing LF per record. `codex_context_telemetry.py` preserves duplicate notifications in that hash but counts only samples whose cumulative `total.inputTokens` advances, requires every advance to equal the directly reported `last.inputTokens`, records the peak active input, and pairs each compaction with the last positive reported input before it and the first positive reported input after it. A positive `activeInputTokensRemoved` is an active-context contraction diagnostic, not total task savings; only the paired final `ThreadTokenUsage.total` values in `armComparison` determine the stock-versus-agentic-compact consumption ratio.

An `agenticTransition` exists only when a scheduled source call is followed in the final chronological snapshot by one unique completed pure context-compaction turn and then the first regular auto or user-wins continuation after the injection acknowledgement. Source, compaction, and continuation positions must be adjacent according to the frozen transition rule, uniquely attributed, and consistent with the terminal journal copy. `unattributedCompactions = contextCompactions - agenticTransitions`; a negative value or duplicate attribution is invalid.

The final summary reports stock native compactions/tasks, v0.2 proactive requests/scheduled requests/transitions and request/transition tasks, and v0.2 residual-native compactions/tasks as separate aggregates. For paired stock and v0.2 runs it also reports distinct token-usage sample count, mean input per sample, peak active input, and peak window utilization together with their absolute deltas and ratios. Total `contextCompactions` alone is never proactive evidence; the minimum 15 stock-fallback tasks and minimum 15 successful proactive-transition tasks are independent support requirements.

For every scheduled transition, the controlled observer immediately copies the latest terminal journal after terminal completion and before another transition can overwrite it. It verifies schema, receipt/source/thread identity, checkpoint hash, terminal state, and the at-most-once fields. Journal copies validate individual transitions only; event counters remain the sole run-level counter source.

## Semantic bundles

For public tasks, the harness may retain a source-truncated pre-call full snapshot, final full snapshot, and terminal journal copies under ignored `eval/raw/`. The trigger judge receives the chronological trajectory with arm and outcome hidden; checkpoint and continuation bundles contain the verified views for exactly one transition.

For private tasks, the harness reads source snapshots and journals in memory, emits only content hashes, whitelisted counters, and deterministic host-evidence booleans, then discards content. A metric that requires a persisted raw bundle is `unavailable`; it must never be guessed from normalized counters, a final patch, or a model summary.

`traceRef` is the lowercase SHA-256 content hash of the controlled local bundle, never a path. Turn, item, journal, checkpoint, continuity-view, thread, run, and annotation identities are opaque IDs or lowercase SHA-256 values exactly where the schemas permit them. Raw source IDs must be hashed before normalization.

## Token and transition derivation

Token fields come from final cumulative `ThreadTokenUsage.total`:

```text
nonCachedInputTokens = max(inputTokens - cachedInputTokens, 0)
blendedTokens = nonCachedInputTokens + max(outputTokens, 0)
```

`reasoningOutputTokens` is an output-token subdivision and is not added again. The formula is a stable evaluation measure, not a price estimate. Every run also records the benchmark evaluator's normalized `[0,1]` `benchmarkScore`; `solved` is true exactly for a full score. The final `diagnostics.armComparison` reports stock and Agentic Compact totals for every token field, solved-run counts and wall time, both arms' mean benchmark score, and the exact candidate-minus-stock deltas and candidate/stock ratios across the 150 main paired runs.

Every successful transition has terminal state `cooldown`, one injection, one continuation, a surviving MCP process, the same thread, non-null compact/continuation/checkpoint/continuity hashes, and `hostEvidenceMatch=true`. Failed or cancelled transitions remain explicit and must not be reported as successful.

## Manifest freeze

The failed 193800-token epoch, the exhausted reset-1 order, and every inspected task remain quarantined control/dev-only material. Reset-1 produced only 57 active-context tasks, so any 50-task held-out left at most `6 + 2 + (57 - 50) = 15` dev tasks and failed permanently. Authorized reset-2 freezes 190 untouched `15 min - 1 hour` SWE-bench Verified tasks from revision `91aa3ed51b709be6457e12d00300a6a596d4c6a3`, after conservatively excluding 116 visible tasks and all 194 remaining `<15 min fix` tasks. The result-blind repository-round-robin order SHA-256 is `4554a8cb950edb12cb477972c53b56c6248943e8c952e74b00239fa8c60af037`. Each prefix unions its first 15 valid native-fallback tasks and first 20 valid, reproducible, semantic-eligible active-context tasks, then fills to 50 with the earliest remaining active-context tasks; the first qualifying 50 freeze permanently. The ignored `reset2-qualification-ledger.json` is atomically regenerated from all canonical qualifier jobs plus sealed preflight and capability reports, rejects duplicate or non-contiguous order evidence, and emits no tentative IDs while `heldoutReady=false`; it never contains verifier output, reward, patch, raw snapshot/trajectory text, or absolute paths. The dev master is the old 161-task control/dev prefix, SHA-256 `c115a4910880f2bd2c6e7974d2708a69e6fb0ffe1131e1ceab2622b088d32e7c`, followed by the reset-2 order. After held-out freezes, its IDs are skipped and the first 20 valid, active-context-qualified, reproducible, semantic-eligible dev tasks are selected; later reset-2 tasks may fill dev without changing held-out. The manifest fixes `selectionRule=reset2-swe-v1:native15:heldsemantic20:held50:devsemantic20` and canonical report SHA-256 `1572d1a100b5c40da2f2ea45befe59341b26629632d9b03e969e00bec263b871` as `corpusProvenanceSha256`. Screening disables verification; mechanically qualified tasks receive one sealed verifier preflight. Pier may physically preserve verifier fields in ignored raw/sealed files, but the frozen top-level projector, status extractor, and membership collector may deserialize only task/status/config-whitelist fields and hashes; `reset2-preflight-status-freeze.json` pins all three, and any drift invalidates the derived status and capability reports without authorizing a task rerun. Reward, verifier result, console content, and agent result never enter membership data flow. Every selected task must expose a real stock native fallback or reach the fixed `39322` peak active-input floor. Exhausting all 190 tasks without every held-out/dev/semantic count is a final Phase 0B failure; no reset-3, short-task fallback, source addition, or gate reduction is allowed.

Benchmark acquisition is verifier-blind and reuse-first: inspect local Docker tags, exact digests, containers, shared layers, dependency caches, and build cache before every batch. Terminal-Bench and dev-control tasks reuse existing images or build from pinned local bases with `--pull=false`; a failed local build never falls back to a prebuilt task image. SWE-bench Verified uses the adapter-defined canonical instance image: freeze its registry digest, reuse it when locally present, and pull each missing digest exactly once instead of rebuilding dependencies. Retain selected images for every repetition and remove only unreferenced task-specific or derived artifacts. Calibration and qualification use one Pier process with `--n-concurrent 2` and low-frequency polling; formal concurrency may rise to four only after a fixed resource canary leaves at least 16 GiB available with no swap growth, OOM, or Docker error, and any failure reduces it to one.

Each selected task must reproduce under stock Codex. Active-context evidence comes only from the peak `ThreadTokenUsage.last.inputTokens` and real compaction items; cumulative token totals, expert estimates, or completed-item totals cannot stand in for context pressure. Broken environments, unavailable dependencies, and severely nondeterministic tests are excluded before final freeze, never after v0.2 results are visible.

The manifest stores only opaque task IDs, benchmark, role, base commit, eligibility labels, selection evidence, task configuration hashes, seeds, runtime hashes, the public v0.1.0 source/binary/plugin/surface/capability baseline hashes, judge hashes, thresholds, selection rule, and corpus provenance hash. A real Agentic Compact arm receives one exact ready capability record generated once outside the benchmark by its exact binary against Codex 0.146.0; each run hash-validates and copies that record into its isolated `CODEX_HOME`, while stock and surface-sham reject it and no run repeats the billable doctor probe. All 20 dev and 50 held-out tasks come from the frozen SWE-bench Verified reset-2 order. Per-task selection evidence contains exactly one of `native-fallback` or `active-context-pressure`, plus `semantic-bundle` when eligible. `semanticBundleEligible` is produced before membership by a result-blind standard-library validator over public-raw policy, target-thread identity, parseable and hash-bound initial/final snapshots, general projection, direct context telemetry, terminal state, privacy support, and the exact `reset2-bundle-capability-freeze.json` hashes for the validator, Phase 0B runner/projector, annotation schema, and currently calibrated judge config/contract/runner/RPC; the judge config separately pins all three prompt hashes. Any mismatch blocks a verdict. This field is not a language-judge label, and later annotations, agreement, verifier data, patch content, and benchmark outcomes cannot change membership. A missing request or transition in the stock qualifier is a valid zero-event stream, not fabricated evidence or automatic ineligibility. The scorer verifies the exact v0.1.0 source commit, 20/50 role counts, reset-2 order provenance, independent passive/proactive support counts, active-context qualification, the 20-task semantic-eligible diagnostic subset, calibrated context grid membership, `0.10` double-review rate, `0.90` minimum agreement, and every release threshold.

The three judge prompt hashes are SHA-256 over the exact committed file bytes. `taskManifestSha256` is SHA-256 over the exact committed manifest bytes. Other binary, plugin, surface, environment, dependency-cache, judge-settings, calibration, and configuration hashes are SHA-256 over the externally frozen byte artifact or canonical bundle defined by that system; the external harness must use one definition consistently and record it before candidate evaluation.

## Result matrix and paths

Stage B fixed-fallback results contain 50 tasks × `stock/v0.2` × 3 repetitions = 300 runs. Rep-1 blocks for the frozen 20-task diagnostic subset add `surface-sham/v0.1` = 40 runs. If `noForcedLimit` is non-null, the same subset adds one `stock/v0.2` diagnostic block = 40 runs. The exact valid total is therefore 340 or 380.

Final valid paths are:

```text
eval/results/<candidate>/<regime>/<arm>/<task-id>/rep-<n>.json
eval/results/<candidate>/annotations/<regime>/<arm>/<stream>/<task-id>/rep-<n>-<ordinal>.json
```

`candidate` is the exact 40-character lowercase commit SHA. Every path component must equal the artifact identity and manifest. Missing or extra JSON artifacts are invalid.

`attempt` is shared by every arm in a paired block. `priorInvalidBlocks` contains privacy-safe reason/count pairs for all earlier invalid attempts; every arm must carry the same canonical list and its counts must total `attempt - 1`. This lets the final scorer report actual invalid block attrition without reading raw attempts. Invalid raw attempts remain only in controlled storage as content-addressed evidence.

## Annotation contract

Trigger annotations use `KEEP`, `DELETE`, `INSERT`, or `MOVE_EARLIER`. Every actual request has exactly one non-INSERT annotation. `MOVE_EARLIER` includes a unique earlier preferred anchor; only a missing boundary can be `INSERT`.

Checkpoint and continuation annotations use `KEEP` or `REPAIR`. Every successful transition in an available semantic stream has exactly one checkpoint and one continuation annotation. Annotation ordinals are contiguous per run and stream, source anchors are unique, and all prompt/judge hashes match the manifest.

Each stream double-reviews `max(1, ceil(0.10 * N))` samples. Sort that stream by `(taskId, repetition, ordinal, annotationId)`, initialize one `random.Random(f"{judgeSamplingSeed}:{stream}:double-review")`, and select `sample(range(N), count)`; no alternative sample is valid. A double-reviewed record has primary, secondary, blind adjudicated, and final labels; final equals adjudicated. A single-reviewed record has no secondary/adjudicated label and final equals primary. The minimum raw agreement across the three streams must be at least 90 percent. Incorrect sample membership, unresolved critical disagreement, unavailable semantic support, or an under-threshold stream prevents a passing summary.

The frozen prompts are:

- `prompts/trigger-judge.md`: two-step discover then prefix-only score.
- `prompts/checkpoint-judge.md`: preservation and next-action review against verified host evidence.
- `prompts/continuation-judge.md`: first-substantive-action and repeated-work review.

## Privacy

Schemas set `additionalProperties=false` on normalized objects. The recursive denylist additionally rejects fields named `command`, `arguments`, `content`, `diff`, `patch`, or `path`, absolute Unix or Windows paths in any string, and identifier fields not explicitly allowed as opaque IDs. Artifacts and rationales must not contain task text, source/response content, patches, secrets, authentication data, local paths, or raw tool results.

`eval/raw/` and `eval/results/` are ignored and must not be committed, uploaded as release assets, or printed to public logs. Only a validated, redacted `evaluation-summary.json` crosses into the protected build/release job.

## Restricted schema subset

The standard-library validator implements only `$schema`, `$defs`, `$ref`, `title`, `description`, `type`, `const`, `enum`, `required`, `properties`, `additionalProperties`, `items`, `minItems`, `maxItems`, `uniqueItems`, `minLength`, `maxLength`, `pattern`, `minimum`, `maximum`, `anyOf`, `oneOf`, and `allOf`.

`$ref` may target only a direct local `#/$defs/Name`. Remote references, unknown keywords, non-boolean `additionalProperties`, and undeclared `format` fail closed. This is a frozen project validator, not a general JSON Schema implementation.

## Commands

Run the local deterministic contract tests:

```bash
python3 eval/score.py --self-test
```

After the real manifest, corpus, judge, and prompts are frozen:

```bash
python3 eval/score.py --validate-manifest eval/manifests/v0.2.json
```

The protected Stage B runner uses the only result/summary interface:

```bash
python3 eval/score.py --validate-results eval/results --candidate "$GITHUB_SHA"
python3 eval/score.py --write-summary eval/results --candidate "$GITHUB_SHA" --output dist/evaluation-summary.json
python3 eval/score.py --validate-summary dist/evaluation-summary.json
```

Summary generation refuses to write a non-passing candidate. Validation requires all 21 frozen gates to pass; diagnostic intervals and McNemar output cannot override a failed safety, quality, cost, policy, support, or judge gate.
