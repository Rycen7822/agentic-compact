# Failure modes

| Condition | Result |
|---|---|
| Missing or mismatched Codex MCP metadata | Reject tool call; no thread operation |
| TUI uses an unreachable embedded app-server | `shared_app_server_unavailable` |
| Launcher option is unknown or can diverge app-server settings | Reject before starting Codex |
| Capability record does not match active Codex build | `unsupported_codex` |
| Non-root thread requests compaction | `not_root_thread` |
| Active descendant thread exists | `active_subagents` |
| Another transition holds the thread lease | `transition_pending` |
| Source turn starts any non-agent-message item after the request tool | `quiescence_violation`; cancel |
| Source turn fails or is interrupted | `source_turn_failed`; cancel |
| User or another turn starts before compact request | `race_lost`; user wins |
| Compact candidate contains unrelated items | `race_lost`; fail closed |
| Zero or multiple compaction items | `recovery_ambiguous`; fail closed |
| Compaction item or turn fails | `compaction_failed`; no injection |
| User turn starts after injection | Do not create automatic continuation; user turn consumes checkpoint |
| Injection result is ambiguous | `injection_failed`; never repeat blindly |
| Capsule still exceeds 8 KiB after all evidence is removed | `checkpoint_too_large`; zero injection |
| Codex refreshes MCP runtime during injection | Unsupported host contract; fail the release gate and do not add an out-of-plan controller |
| Empty continuation is rejected | `continuation_unsupported` |
| Empty continuation emits a `userMessage` item | `continuation_unsupported` |
| App-server disconnects after mutation | `recovery_ambiguous` |
| MCP process restarts before native compact | Cancel stale intent |
| MCP process restarts after compact with a persisted capsule and unchanged exact boundary | Resume that capsule once |
| MCP process restarts when injection or unbound continuation may have been accepted | `recovery_ambiguous`; never replay |
| MCP process restarts after an acknowledged continuation ID | Exact turn present: cooldown; otherwise `recovery_ambiguous` |
| Journal or ownership hash mismatch | Preserve user data; report error |

Duplicate notifications for an already-bound compaction turn/item are idempotent. Completion-before-start ordering and unknown items inside the candidate compaction turn fail closed instead of being reconstructed heuristically.

## Unavoidable external race

There is a TOCTOU interval between the final idle `thread/read` and `thread/compact/start`. App-server does not currently offer an atomic `compact-if-revision` operation. Event draining and user-wins checks reduce this window but cannot prove it absent. A Core-level atomic transition primitive is required to remove it.

The frozen Codex 0.146.0 build preserves the original MCP/orchestrator process through `thread/inject_items`, empty continuation, durable cooldown, and TUI turn completion. Process survival remains a release gate: a future host that refreshes the MCP runtime during the transition is unsupported because startup recovery cannot safely replay an ambiguous mutation.

Agentic Compact emits no desktop or terminal completion notification of its own. The isolated headless TUI regression shows exactly one stock `Context compacted` marker; OS-level desktop notification coalescing remains owned by stock Codex and is not claimed by this plugin.
