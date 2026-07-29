# Changelog

## 0.1.0 — unreleased

- Implemented model-triggered same-thread native compaction, bounded checkpoint injection and empty-input continuation through one MCP/orchestrator process.
- Added strict Codex metadata binding, bounded MCP/app-server transport, per-thread registry and lease, atomic journal, conservative recovery, LocalDaemon launcher, doctor and ownership-aware installer.
- Frozen the supported window to Codex CLI 0.146.0 and 0.145.0 with stable schema, source, binary, launcher and plugin-CLI fixtures.
- Verified real reentrant attach, 100 repeated resume/unsubscribe cycles, stock-TUI native compaction, hash-matched injection, empty continuation and preservation of the same target MCP PID through TUI completion.
- Completed the Phase 3 race matrix for reordered, duplicate, unknown, competing, failed, missing and multiple compaction events.
- Completed Phase 4 with bounded evidence projection, no raw command/RPC payload retention, priority trimming to 8 KiB, a current real injection/empty-continuation probe and explicit non-ambiguous injection-ack semantics.
- Completed Phase 5 with durable full-capsule journaling, fixed crash-state dispositions, uniquely provable pre-injection and continuation recovery, active-descendant guards, user-wins integration coverage and server-request fail-closed tests.
- Completed Phase 6 with malformed transport and process-kill chaos coverage, 180 production-path cooldown transitions, parallel thread isolation, latency/journal/RSS SLOs and a fresh stock-TUI scrollback/rendering capture.
- Added a complete installer-emitted marketplace root, official Codex plugin add/remove lifecycle, atomic ownership preflight, loaded-thread upgrade deferral, authenticated installed-binary doctor validation, blocking stable/stable-N-1 CI and reproducible two-contract artifact provenance/workflow definitions.
- Phase 7 workflow execution, macOS arm64 manual validation and publication remain pending.
