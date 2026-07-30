# Agentic Compact

Agentic Compact is an external Codex plugin prototype that lets the model request Codex-native context compaction at a stable semantic boundary, injects a bounded continuity checkpoint, and resumes work in the same Codex thread without a synthetic user message.

The design deliberately does **not** patch Codex Core, edit rollout files, modify SQLite state, simulate TUI keystrokes, or call the private `/responses/compact` endpoint directly. It controls the stable Codex app-server surface and delegates compaction to Codex's native `CompactTask`.

## Current status

Project status: **development build** targeting the frozen Codex 0.146.0 and stable-N-1 0.145.0 contracts. This is not a release build.

Implemented:

- MCP stdio tools: `status` and `request_compaction`;
- strict Codex turn/thread metadata binding;
- shared Unix-socket app-server client;
- per-thread lease, journal and conservative recovery;
- source-turn quiescence and user-wins guards;
- native `thread/compact/start` orchestration;
- unique `contextCompaction` validation;
- bounded, hashed checkpoint injection;
- bounded deterministic evidence with 8 KiB priority pruning;
- same-thread empty-input continuation;
- launcher, doctor and ownership-aware installer with official plugin lifecycle;
- Codex plugin manifest and focused skill.

Validated against the frozen Codex 0.146.0 build:

- format, build, strict Clippy, deterministic fake app-server tests and focused real-Codex probes;
- 100 repeated same-thread resume/unsubscribe cycles with exact active-setting preservation;
- pending-tool-call reentrant attach with zero source-turn interruption;
- native compaction, bounded checkpoint injection and empty continuation in one live stock-TUI thread;
- current 0.146.0 disposable doctor probe accepts the production checkpoint and completes an empty continuation without `userMessage`;
- the same MCP/orchestrator PID survives checkpoint injection, continuation, journal `COOLDOWN`, and stock-TUI turn completion;
- 100 sequential and 8 × 10 parallel production-path transitions complete with zero duplicate compact, lost checkpoint, synthetic user message, cross-thread action or resource leak, while every latency, journal and RSS SLO passes;
- an isolated stock-TUI evidence capture confirms preserved scrollback, one compact tool card, hidden checkpoint content, one native compaction marker and exact continuation output.

The frozen 0.145.0 contract independently passes regenerated stable-schema comparison, official plugin and installer lifecycles, real app-server termination handling and authenticated 100-resume preservation. Earlier Codex versions fail closed as unsupported.

Release boundary:

- tag only a candidate whose blocking stable/stable-N-1 CI, current-main canary, three reproducible native artifacts and commit-bound provenance all pass;
- v0.1.0 publishes Linux x86_64 musl, Linux arm64 musl and macOS x86_64 only; it does not publish or claim macOS arm64/Apple Silicon support.

## Architecture

```text
Codex model
    │ MCP tools/call
    ▼
agentic-compact mcp
    ├── validates _meta.threadId + x-codex-turn-metadata
    ├── resumes the exact live thread through the shared app-server
    ├── waits for the source turn to complete quiescently
    ├── calls thread/compact/start
    ├── validates one successful contextCompaction item
    ├── injects developer wrapper + assistant checkpoint
    └── calls turn/start with input: []

Codex TUI ───────────────┐
                         ├── same local app-server / same threadId
agentic-compact control ─┘
```

The source turn, compact turn and continuation turn are separate turns under one `threadId`. Codex 0.146.0 preserves the MCP/orchestrator process across that transition. The stable API still cannot provide a host-atomic transaction across `idle check → compact → inject → continue`; ambiguous states fail closed.

## Build

Requirements:

- Rust 1.85 or newer;
- Unix-domain sockets (Linux, WSL2 or macOS for v0.1.0);
- a Codex build whose stable app-server API includes:
  - `thread/loaded/list`
  - `thread/read`
  - `thread/resume`
  - `thread/compact/start`
  - `thread/inject_items`
  - `turn/start`
  - `thread/unsubscribe`

```bash
cargo fmt --all -- --check
cargo check --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release
```

## Commands

```text
agentic-compact mcp
agentic-compact codex -- <normal Codex TUI arguments>
agentic-compact doctor [--probe] [--ack-reentrant-attach] [--ack-hidden-checkpoint]
agentic-compact install
agentic-compact uninstall
agentic-compact version
```

### MCP configuration

The installer writes one explicit user configuration section:

```toml
[mcp_servers."agentic-compact"]
command = "/absolute/path/to/agentic-compact"
args = ["mcp"]
env_vars = ["CODEX_HOME"]
default_tools_approval_mode = "approve"
```

The `CODEX_HOME` whitelist keeps the launcher and Codex-spawned MCP on the same default socket when a non-default Codex home is active. The plugin manifest intentionally does not declare the MCP server, preventing duplicate definitions while keeping executable ownership explicit.

`install` validates the managed fields through the target Codex config parser before writing, verifies the effective server afterward, emits a complete local marketplace root and invokes the frozen Codex 0.146.0/0.145.0 `plugin marketplace add` and `plugin add` commands; `uninstall` uses the matching official remove commands and never writes the plugin cache directly.

### TUI launcher

Use the generated `codex-agentic` wrapper or run:

```bash
agentic-compact codex --
```

The launcher starts or reuses Codex's default Unix-socket app-server and then launches the stock TUI. Arguments that can force a divergent app-server configuration are rejected; move those settings into `config.toml`.

## Model tools

### `status`

Read-only readiness check. It reports shared-server availability, root-thread status, active descendants, transition state, cooldown and capability gates.

### `request_compaction`

```json
{
  "preserve": [
    "Keep the public API backward compatible",
    "Do not remove the parser fallback"
  ],
  "next_action": "Run the full test suite and fix the first real failure"
}
```

After the tool returns `scheduled_after_turn`, the model must end the current turn immediately. Starting another command, test, plan mutation or subagent invalidates quiescence and cancels the transition.

## Safety properties

- exact thread and turn binding from Codex-owned metadata;
- at most one non-terminal transition per thread;
- OS file lock across MCP instances;
- bounded arguments, snapshots, events, journal and checkpoint;
- credential-like checkpoint input is rejected;
- model-authored content remains in an assistant checkpoint, never developer authority;
- mutating app-server requests are never blindly retried;
- user turns always win observable races;
- ambiguous crash recovery fails closed;
- full checkpoint capsule persistence and uniquely provable crash recovery;
- no cross-thread heuristic selection.

## Repository map

```text
src/app_server.rs          bounded UDS WebSocket JSON-RPC client
src/mcp.rs                 MCP stdio protocol and tools
src/metadata.rs            Codex-owned thread/turn binding
src/orchestrator.rs        scheduling, guards and status
src/orchestrator/runner.rs happy-path transition state machine
src/orchestrator/recovery.rs conservative restart handling
src/checkpoint.rs          intent validation and checkpoint capsule
src/journal.rs             atomic transition journal
src/lease.rs               per-thread OS file lock
src/launcher.rs            shared LocalDaemon topology
src/doctor.rs              capability record and disposable probe
src/install.rs             ownership-aware local installation
plugins/agentic-compact/   Codex plugin manifest and skill
```

## License

MIT. See [`LICENSE`](LICENSE).
