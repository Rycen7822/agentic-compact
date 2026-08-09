# Trigger judge: semantic-boundary-v2

Judge whether an Agentic Compact request occurred at the correct semantic boundary. Do not use the final patch, final outcome, arm name, or information after the candidate anchor.

## Discover mode

Input is one complete chronological raw trajectory with outcome and arm hidden. Return every actual `request_compaction` anchor, every clear omitted boundary, and an earlier preferred anchor for each late request. An anchor is the stable turn hash, item hash, and chronological position supplied by the adapter. Do not invent anchors or emit duplicates.

An actual request must be an `mcpToolCall` to the Agentic Compact server and `request_compaction` tool. A `contextCompaction` item is native compaction and is never an actual request.

For an actual request or missing boundary, set `sourcePosition` equal to `position`. For a preferred boundary, set `position` to the earlier boundary and `sourcePosition` to the related late request. Emit that earlier boundary only as preferred, never also as missing.

Use a boundary only when the current phase has a decisive conclusion and substantial work remains in a different phase. Exploration without a conclusion, active edits, active verification, a recent compaction, or an unstable state is not a valid boundary.

## Score mode

Input is the history prefix ending exactly at one discovered anchor. Return one label:

- `KEEP`: an actual request at a valid boundary.
- `DELETE`: an actual request with no valid boundary.
- `INSERT`: a valid omitted boundary with no actual request.
- `MOVE_EARLIER`: an actual request that should have occurred at the supplied unique earlier anchor.

The supplied candidate fields bind the label and taxonomy:

- `actualRequest=false`: `INSERT`, `missed_boundary`, and no preferred position.
- Supplied `preferredAnchorPosition`: `MOVE_EARLIER`, `late_boundary`, and echo that position.
- Other actual requests: `KEEP` with no taxonomy or `DELETE` with `harmful_trigger`; no preferred position.

Attributes always describe the supplied candidate anchor. For `MOVE_EARLIER`, do not copy the state of the preferred earlier boundary.

Record `phaseBefore`, `phaseAfter`, `substantialWorkRemaining`, `stateStable`, `activeWork`, and `recentCompaction`. Use only the supplied evidence. The rationale must be short, factual, and contain no command, arguments, content, diff, patch, path, secret, or raw identifier.

Return only one object conforming to the frozen annotation adapter contract. Do not score utility, token savings, or whether the task eventually succeeded.
