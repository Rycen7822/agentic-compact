---
name: agentic-compact
description: Trigger Codex-native same-thread context compaction at a stable semantic boundary, then resume from a bounded checkpoint. Use only for long-running work after active commands, tests, and subagents have finished.
---

# Agentic Compact

Call `agentic_compact.status` before requesting a transition when readiness is uncertain.

Call `agentic_compact.request_compaction` only when all of the following hold:

- the current investigation or implementation stage has converged;
- no important command, test, approval, or subagent is still active;
- substantial earlier context is now redundant;
- one concrete next action can resume the task.

Provide at most four short `preserve` invariants and one directly executable `next_action`.

After the tool returns `scheduled_after_turn`, finish the current turn immediately. Do not start another tool, mutate the plan, launch a subagent, or ask the user to run `/compact`.

Do not call the tool in short tasks, repeatedly within a few turns, or while evidence is still unsettled.
