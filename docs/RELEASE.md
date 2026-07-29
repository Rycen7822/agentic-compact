# Release gates

A release tag must not be created until every blocking item below passes against a named Codex build.

Current state: Phase 0–6 pass against Codex CLI 0.146.0, and the local stable-N-1 contract/installer/real-process probes pass against 0.145.0. Phase 7 GitHub matrix execution, cross-platform artifacts, macOS experience and publication gates remain open.

## Static and unit gates

```bash
cargo fmt --all -- --check
cargo check --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

## Fake app-server contract gates

- initialize/initialized handshake;
- out-of-order JSON-RPC responses;
- unknown notifications;
- `-32001` read retry and no mutating retry;
- connection loss at each state;
- receipt binding and source quiescence;
- user-wins race cases;
- exactly one successful compaction item;
- injection failure;
- empty continuation rejection;
- journal and lease recovery.

## Real Codex gates

- TUI is connected as `LocalDaemon`, not Embedded or explicit Remote;
- second connection can resume the exact live thread while the MCP call is in flight;
- source turn is never interrupted by scheduling;
- compact activity appears in the same TUI page;
- injected checkpoint is model-visible but not rendered as a large transcript message;
- empty continuation creates no `userMessage`;
- model executes `nextAction` without repeating the completed stage;
- prior scrollback remains visible, the page does not switch, the checkpoint body stays hidden and one native compaction marker is shown;
- approval, sandbox, cwd, model and effort remain unchanged;
- active subagents defer the transition;
- user input wins every tested boundary race.

## Reliability and performance gates

- 100 sequential production-cooldown transitions: zero duplicate compact, zero lost checkpoint, zero synthetic user message;
- 8 parallel threads × 10 transitions: zero cross-thread action;
- no leaked lock, task or app-server process;
- warm scheduling p95 below 1.5 s;
- source completed → compact request p95 below 150 ms;
- compact completed → injection p95 below 150 ms;
- injection → continuation request p95 below 150 ms.

Latest measured Phase 6 evidence: 180/180 transitions passed with zero integrity or leak failures; p95 warm scheduling 16.53 ms, source-to-compact 25.59 ms, compact-to-injection 38.65 ms and injection-to-continuation 16.58 ms; journal p99 2.63 ms; idle RSS 4,984 KiB; 31 MiB snapshot-path peak RSS 133,588 KiB. The 180-transition run uses the real production orchestrator, UDS transport, journal and cooldown against a deterministic protocol peer; current real-Codex probes separately prove native compact and TUI behavior.

## Installation gates

The isolated installer lifecycle passes against Codex 0.146.0 and 0.145.0: complete marketplace root, official plugin enable/remove, idempotent reinstall, one config backup, unrelated-TOML preservation, loaded-thread upgrade deferral, all-owned-surface preflight before deletion, and zero partial uninstall after managed-state modification. A fresh current-version installed binary passes authenticated disposable doctor probes with `ready: true`.

## Artifacts

Build and hash:

- Linux x86_64 musl;
- Linux arm64 musl;
- macOS arm64;
- macOS x86_64.

Publish SHA-256 and a provenance file containing both supported Codex user-agent/schema/source contracts, Rust version and test summary.

`.github/workflows/ci.yml` and `.github/workflows/release-artifacts.yml` define blocking 0.146.0/0.145.0 schema and quality matrices; release CI also defines four native runner builds, rebuild-and-compare reproducibility checks and two-contract provenance generation. `.github/workflows/codex-main-canary.yml` defines the nonblocking daily upstream-main stable-schema surface check. These workflows, the macOS arm64 manual experience gate and a real project source commit remain external blockers; no local result is represented as a published artifact.
