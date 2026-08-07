# Trigger judge: semantic-boundary-v2

Judge whether an Agentic Compact request occurred at the correct semantic boundary. Do not use the final patch, final outcome, arm name, or information after the candidate anchor.

## Discover mode

Input is one complete chronological raw trajectory with outcome and arm hidden. Return every actual `request_compaction` anchor, every clear omitted boundary, and an earlier preferred anchor for each late request. An anchor is the stable turn hash, item hash, and chronological position supplied by the adapter. Do not invent anchors or emit duplicates.

An actual request must be an `mcpToolCall` to the Agentic Compact server and `request_compaction` tool. A `contextCompaction` item is native compaction and is never an actual request.

Use a boundary only when the current phase has a decisive conclusion and substantial work remains in a different phase. Exploration without a conclusion, active edits, active verification, a recent compaction, or an unstable state is not a valid boundary.

## Score mode

Input is the history prefix ending exactly at one discovered anchor. Return one label:

- `KEEP`: an actual request at a valid boundary.
- `DELETE`: an actual request with no valid boundary.
- `INSERT`: a valid omitted boundary with no actual request.
- `MOVE_EARLIER`: an actual request that should have occurred at the supplied unique earlier anchor.

Record `phaseBefore`, `phaseAfter`, `substantialWorkRemaining`, `stateStable`, `activeWork`, and `recentCompaction`. Use only the supplied evidence. The rationale must be short, factual, and contain no command, arguments, content, diff, patch, path, secret, or raw identifier.

Return only one object conforming to the frozen annotation adapter contract. Do not score utility, token savings, or whether the task eventually succeeded.
