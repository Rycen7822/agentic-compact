# Codex protocol baseline

## Installed target

The current executable baseline is `codex-cli 0.146.0` on Linux x86_64. Its JavaScript launcher and native executable have SHA-256 values:

```text
launcher  134063e133f0b4244fa3b251acf973d4fe4b4aeeacbdc135211bf480f59f1477
native    2e863156ed35ecc5253b1e2f907a9143077b9f7cb51942070c61996471ff6e04
```

The stable schema command generated 275 files. SHA-256 over sorted `relative-path NUL file-sha256 LF` records is `2a56e22ada954380862fe419c39825d45db2d2e7f7f096dd882fa6cc4600cc19`. The complete output and baseline metadata are frozen under `tests/fixtures/app-server/codex-cli-0.146.0/`; no experimental schema was used.

An isolated initialize handshake reported:

```text
agentic_compact/0.146.0 (Ubuntu 24.4.0; x86_64) WindowsTerminal (agentic_compact; 0.1.0)
```

Capability records bind to that exact app-server `userAgent`, not only semver.

The blocking stable-N-1 baseline is `codex-cli 0.145.0`: wrapper SHA-256 `134063e133f0b4244fa3b251acf973d4fe4b4aeeacbdc135211bf480f59f1477`, native SHA-256 `a2a05dafaa1acb002a45eaec0a462de5b13694fcfcd7bc43305f14781ce7be14`, 273 stable schema files and aggregate `72a461ea8036d16eb8403ceddfdc7e53905980e2873611e5ab0d6ec13a98e904`. Its regenerated schema, official plugin lifecycle, installer lifecycle, real app-server termination and authenticated 100-resume probes pass locally. Earlier versions are unsupported.

## Upstream source binding

The current executable is matched to upstream tag `rust-v0.146.0`, peeled commit `e363b08c9175ac1cbe5893615dd2cb9ddf95043b`; stable-N-1 is matched to tag `rust-v0.145.0`, commit `25af12f7e61572b0bc18ddb1008be543b91519b0`. The reviewed current-version owners are:

```text
codex-rs/app-server/src/request_processors/thread_processor.rs:1832-1841
  native compact submits Op::Compact
codex-rs/app-server/src/request_processors/thread_processor.rs:2197-2215
  loaded-thread data is a sorted Vec<String>
codex-rs/app-server/src/request_processors/thread_processor.rs:2410-2420
  ephemeral includeTurns is rejected
codex-rs/app-server/src/request_processors/thread_processor.rs:3340-3370
  resume returns active model/cwd/approval/sandbox/effort settings
codex-rs/app-server/src/request_processors/turn_processor.rs:474-585
  empty input remains a valid Op::UserInput turn
codex-rs/app-server/src/request_processors/turn_processor.rs:873-899
  inject parses raw ResponseItem values and appends them to model history
codex-rs/app-server-protocol/src/protocol/v2/thread.rs:1359-1384
  stable read and inject request shapes
codex-rs/tui/src/lib.rs:267-270,868-876
  Embedded/LocalDaemon/Remote targets and default-socket LocalDaemon selection
```

## Runtime lifecycle gate

An isolated authenticated Codex 0.146.0 stock-TUI run proved the second app-server
connection can resume the exact active thread while the MCP tool call is still
pending. The source turn completed normally; the same rollout then recorded one
native compaction, the hash-matched developer/assistant checkpoint pair, and an
empty continuation in the same thread.

The original MCP/orchestrator PID remained alive through injection,
continuation, durable `COOLDOWN`, and TUI turn completion. From source
completion onward, Codex's structured log recorded zero MCP runtime refreshes,
child exits, service cancellations, or WebSocket receive errors. This clears
the process-lifetime release gate.

The Phase 3 state-machine contract treats duplicate notifications for the same compaction turn/item as idempotent, but rejects completion-before-start ordering, unrelated candidate-turn items, multiple compaction items, and competing turns.

## Stable methods used

```text
initialize
initialized
thread/loaded/list
thread/read
thread/resume
thread/start                 doctor disposable probe only
thread/delete                doctor cleanup only
thread/compact/start
thread/inject_items
turn/start
thread/unsubscribe
```

## Notifications used

```text
turn/started
turn/completed
item/started
item/completed
thread/status/changed
thread/tokenUsage/updated
```

Unknown notifications are ignored. Server-initiated requests on the control connection abort the active transition because the plugin must not answer approvals on the TUI's behalf.

`item/started` and `item/completed` fixtures include the schema-required
`startedAtMs` and `completedAtMs` fields.

## Fixed projections

### `thread/loaded/list`

The current generated schema returns:

```json
{
  "data": ["thread-id"],
  "nextCursor": null
}
```

`data` contains strings, not thread objects. This was the final protocol mismatch identified during implementation and is reflected in `protocol::loaded_thread_page`.

### Thread status

Thread status is an object whose discriminator is read from `status.type`; a plain string is accepted only as a conservative compatibility projection.

### Empty continuation

The current `TurnStartParams` schema requires `input: Array<UserInput>` and accepts an empty array. Source inspection is not treated as a release proof. `doctor --probe` creates a disposable ephemeral thread, injects a production checkpoint, starts an empty turn, waits for completion, and rejects any `userMessage` item observed in the subscribed event stream. Codex 0.146.0 does not permit `thread/read(includeTurns=true)` for ephemeral threads, so the probe does not depend on that operation.

### Phase 6 validation

The production orchestrator, UDS transport, atomic journal and three-regular-turn cooldown completed 100 sequential and 8 × 10 parallel deterministic transitions with no duplicate compact, lost checkpoint, synthetic user message, cross-thread action or resource leak. Current real 0.146.0 probes separately prove native compact, same-page stock-TUI rendering, preserved scrollback, hidden checkpoint content, one native marker and exact empty-continuation output.

### Plugin CLI

Codex 0.146.0 and 0.145.0 use `plugin marketplace add/remove`, `plugin add/remove` and `mcp get <name> --json`; they do not use install/uninstall verbs. Exact JSON command vectors and output key contracts are frozen per version under `tests/fixtures/codex-cli/`. The ownership-aware installer invokes those commands directly, validates its MCP fields with target-version overrides in a disposable `CODEX_HOME`, and verifies the effective stdio server after the real TOML merge. Repeated install plus add/list/remove/uninstall cycles pass in isolated homes without direct cache writes. A Codex-spawned stdio MCP receives a non-default `CODEX_HOME` only when the managed server section forwards it through `env_vars`; an isolated 0.145.0 stock-TUI transition passed with that whitelist and no `$HOME/.codex` fallback.

### Launcher classification

Both allowed forms reached the stock Codex 0.146.0 UI: zero arguments in the
active home and one initial prompt in an isolated unauthenticated home. Unknown
options, model overrides, and Codex subcommands were rejected before creating a
socket. Each launcher-owned app-server socket was absent after exit.

### MCP metadata

Codex supplies:

```json
{
  "threadId": "...",
  "x-codex-turn-metadata": {
    "thread_id": "...",
    "turn_id": "...",
    "model": "...",
    "reasoning_effort": "..."
  }
}
```

The outer and inner thread IDs must match exactly. The model never supplies the target thread ID as a tool argument.

## Size limits

```text
WebSocket message          128 MiB
normal RPC response          4 MiB
thread snapshot             32 MiB
checkpoint injection        64 KiB
MCP stdin message            1 MiB
journal                     64 KiB
checkpoint capsule           8 KiB
```

Evidence projection is bounded before it is retained: user objective text is capped at 512 Unicode scalars, changed-file projection keeps at most 64 bounded paths, and MCP results retain only up to eight syntactically valid receipt IDs. Verification evidence stores a fixed allowlisted label, item ID, kind, status and exit code; raw commands, arguments, results, errors and output never enter the checkpoint.

When the serialized capsule would exceed 8 KiB, construction removes the oldest verification entries, then the oldest changed files, then truncates the last user objective. Model-provided `preserve` and `nextAction` are never trimmed. The 0.146.0 `plan` thread item is explicitly marked experimental in the generated schema, so the stable contract omits structured plan evidence rather than parsing prose or subscribing to an unstable notification.

`thread/inject_items` returns an empty success object, and stable `thread/read` does not project the injected developer/assistant history even on a completed persistent thread. Phase 4 therefore verifies the exact locally hashed payload through the non-ambiguous response correlated to its single mutation request. A timeout or disconnect is not post-hoc recoverable through stable reads: it produces `injection_failed`, starts no continuation, and is never replayed.

Phase 5 persists the complete bounded capsule before the journal enters `INJECTING_CHECKPOINT`. Restart recovery may use that capsule only from the preceding `AWAIT_COMPACTION_TURN_COMPLETED` state after a fresh resume proves the thread is idle and its exact last turn is the one successful bound compaction. `INJECTING_CHECKPOINT` and `STARTING_CONTINUATION` remain fail-safe fences; `AWAIT_CONTINUATION_STARTED` reaches cooldown only when stable `thread/read` contains its exact stored continuation turn ID.
