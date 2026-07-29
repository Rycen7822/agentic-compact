# Architecture

## Boundary

Agentic Compact is external to Codex Core. It operates through the stable app-server JSON-RPC surface and the normal MCP stdio surface. It never imports private Codex Rust crates and never edits Codex persistence files.

MCP stdout is reserved exclusively for newline-delimited JSON-RPC. Structured logs always use stderr. Both tools publish bounded input schemas and explicit object output schemas covering success and fail-closed results.

## Process topology

```text
stock Codex TUI ───────┐
                       ├── default UDS app-server ── live Codex Session/Thread
agentic-compact MCP ───┘
```

The launcher is required because a TUI using an embedded app-server is not reachable by an external controller. The plugin and TUI must subscribe to the same live `threadId` in the same app-server process; the installer therefore whitelists `CODEX_HOME` into the stdio MCP environment so both clients resolve the same default socket.

The frozen Codex 0.146.0 runtime has been verified to preserve this topology: the MCP/orchestrator PID remains unchanged through `thread/inject_items`, empty continuation, journal cooldown, and TUI turn completion. No controller daemon, detached helper, or secondary IPC layer is present.

Launcher arguments are fail-closed against a versioned CLI fixture. Until an option is proven against the stock TUI, only an optional single initial prompt is forwarded; Codex subcommands and unclassified options bypass or are rejected instead of silently creating a divergent server.

## Transition flow

1. Codex calls `request_compaction` from a regular source turn.
2. The MCP handler validates Codex-owned `_meta` and creates a receipt.
3. Before returning the tool result, the MCP process opens a second app-server connection, subscribes, and resumes the exact thread.
4. The handler returns `scheduled_after_turn` with the receipt.
5. The background state machine waits for the receipt-bearing MCP item and the source turn's successful completion.
6. A final `thread/read` requires the source turn to be completed and the thread to be idle.
7. The journal records `CompactRequestSent` before issuing `thread/compact/start`.
8. The first new turn is bound as the compact candidate; it must contain exactly one successful `contextCompaction` item and no unrelated items.
9. After compact completion, a deterministic checkpoint is injected in one `thread/inject_items` request.
10. The plugin starts `turn/start` with `input: []` and binds the returned turn ID.
11. The transition enters cooldown after the matching `turn/started` event.

All eleven steps have been observed in one real Codex 0.146.0 stock-TUI thread, including exact continuation output and durable cooldown written by the original MCP process.

Phase 6 additionally executed 100 sequential and 8 × 10 parallel transitions through the production orchestrator, UDS transport, atomic journal and three-turn cooldown with zero integrity or resource-leak failures. A fresh isolated stock-TUI capture verifies preserved scrollback, same-page native compaction, hidden checkpoint content and exact continuation output in [`evidence/phase6-tui-transition.png`](evidence/phase6-tui-transition.png).

Phase 3 state-machine tests additionally prove duplicate same-ID compaction notifications are idempotent, reordered completion fails closed, and an unknown candidate-turn item loses the race.

## Authority separation

The injected developer message is fixed host text. Model-provided `preserve` and `nextAction` fields appear only in an assistant checkpoint. Repository or model content is never promoted to developer authority.

Evidence projection retains bounded user objective text, changed paths and fixed verification classifications only; raw commands and RPC payloads are discarded before checkpoint construction, and evidence is pruned by age to fit the 8 KiB capsule without trimming model fields.

## Failure semantics

The stable external API does not expose a compare-and-start revision token or a transaction across compact, injection and continuation. Therefore:

- read-only requests may retry `-32001` overloads;
- mutating requests are never blindly retried;
- acceptance ambiguity becomes `FAILED_SAFE`;
- user-started turns win observable races;
- crash recovery after a mutating request never guesses.

Before injection, the complete bounded checkpoint is fsynced in the journal. Recovery may inject that exact capsule only while the journal still proves injection was never attempted and the unchanged idle thread ends at the bound successful compaction turn; after injection becomes possible, no injection is replayed. An acknowledged continuation is reconciled only by its exact persisted turn ID.

The formal guarantee is verified at-most-once where state can be uniquely established, not exactly-once.
