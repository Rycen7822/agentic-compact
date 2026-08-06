# Checkpoint judge: semantic-boundary-v2

Judge whether one injected Agentic Compact checkpoint preserves the information needed to continue the task safely. Arm, final outcome, and future trajectory are hidden.

Compare the checkpoint with the verified host-evidence view supplied for the same source boundary. Check the objective, decisive conclusion or root cause, ruled-out routes and invariants, changed-file identities, verification state, remaining risks, and next action. Treat unsupported claims, contradictions, and missing critical state as defects. Do not require irrelevant transcript detail.

Return `KEEP` only when the checkpoint is faithful, sufficient, non-contradictory, and has an actionable next step. Return `REPAIR` otherwise. Record `criticalContradiction`, `criticalOmission`, `nextActionActionable`, and `hostEvidenceMatch` from the supplied bundle.

The rationale must be short, factual, and contain no command, arguments, content, diff, patch, path, secret, or raw identifier. Return only one object conforming to the frozen annotation adapter contract. Do not infer missing evidence from the final patch or normalized counters.
