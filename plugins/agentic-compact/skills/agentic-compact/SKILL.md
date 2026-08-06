---
name: agentic-compact
description: Schedule Codex-native same-thread compaction at a settled phase boundary, inject bounded continuity state, and continue automatically. Use only after active commands, tests, approvals, edits, and subagents have finished.
---

# Agentic Compact

Call `agentic_compact.request_compaction` only when all of the following hold:

- the current phase has reached a stable conclusion;
- earlier detail is less valuable than that conclusion;
- substantial work remains to amortize the transition;
- at most four preserve facts and one next action can hand off the work accurately;
- no command, test, approval, file change, or subagent is active.

A valid boundary can be converged exploration before editing, a disproved route before a new route, implementation before broader verification, or completed verification before a new substantive repair or extension.

Do not call during root-cause investigation, conflicting evidence, editing, test waits, failure debugging, unstable goals or acceptance criteria, active work, or descendant-agent work. Do not call when only a short test, reply, or commit remains, soon after another compaction, or merely because context is long.

Use `preserve` only for facts the host cannot infer, ordered as: decisive conclusion or root cause; ruled-out route or invariant; interface or behavior constraint; unresolved risk or verification obligation. Use `next_action` for one concrete step that remains valid after checking the workspace.

If scheduled, end the turn immediately without starting another tool or subagent. If rejected, continue the task and do not retry in that turn.
