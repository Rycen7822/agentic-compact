# Continuation judge: semantic-boundary-v2

Judge the first substantive action after one verified Agentic Compact continuation. Arm and final outcome are hidden.

Compare the action with the verified checkpoint and continuity view. Return `KEEP` when it advances the stated next action without reopening a settled phase, returning to a ruled-out route, violating an invariant, misstating verification, or asking the user for information already preserved. Return `REPAIR` when any of those failures occurs.

Record `repeatedSettledPhase` and the relevant phase attributes using only the complete raw continuation bundle. Repeated reads, searches, or tests cannot be inferred from normalized counters; if the raw bundle is unavailable, the adapter must not create this annotation.

The rationale must be short, factual, and contain no command, arguments, content, diff, patch, path, secret, or raw identifier. Return only one object conforming to the frozen annotation adapter contract. Do not score total task utility or token savings.
