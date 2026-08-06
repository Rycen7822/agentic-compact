use super::*;
use crate::checkpoint::{Checkpoint, CompactionIntent, Evidence};
use crate::journal::TransitionJournal;
use serde_json::json;

fn journal(state: TransitionState) -> TransitionJournal {
    let mut journal = TransitionJournal::new(
        "thread".to_owned(),
        "source".to_owned(),
        format!("rcpt_{}", "a".repeat(32)),
        format!("cp_{}", "b".repeat(32)),
        CompactionIntent {
            preserve: Vec::new(),
            next_action: "continue".to_owned(),
        },
    )
    .unwrap();
    journal.state = state;
    journal
}

#[test]
fn every_state_has_a_fixed_recovery_disposition() {
    use RecoveryDisposition::*;
    use TransitionState::*;
    for state in [Attaching, AwaitSourceTurnCompleted, ReadyToCompact] {
        assert_eq!(recovery_disposition(&journal(state)), CancelBeforeMutation);
    }
    for state in [
        CompactRequestSent,
        AwaitCompactionItem,
        AwaitCompactionTurnCompleted,
        InjectingCheckpoint,
        StartingContinuation,
        AwaitContinuationStarted,
    ] {
        assert_eq!(recovery_disposition(&journal(state)), FailAmbiguous);
    }
    for state in [Cooldown, Cancelled, FailedSafe] {
        assert_eq!(recovery_disposition(&journal(state)), IgnoreTerminal);
    }

    let mut continuation = journal(AwaitContinuationStarted);
    continuation.continuation_turn_id = Some("continuation".to_owned());
    assert_eq!(recovery_disposition(&continuation), ConfirmContinuation);

    let mut checkpoint = journal(AwaitCompactionTurnCompleted);
    checkpoint.compact_turn_id = Some("compact".to_owned());
    checkpoint
        .set_checkpoint(
            Checkpoint::build(
                checkpoint.checkpoint_id.clone(),
                checkpoint.receipt_id.clone(),
                checkpoint.thread_id.clone(),
                checkpoint.source_turn_id.clone(),
                "compact".to_owned(),
                checkpoint.intent.clone(),
                Evidence::default(),
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(recovery_disposition(&checkpoint), ResumePersistedCheckpoint);
}

#[test]
fn checkpoint_recovery_requires_idle_exact_last_compaction_turn() {
    let mut journal = journal(TransitionState::AwaitCompactionTurnCompleted);
    journal.compact_turn_id = Some("compact".to_owned());
    let thread = ThreadRef::from_response(
        &json!({"thread":{
            "id":"thread",
            "status":{"type":"idle"},
            "turns":[{
            "id":"compact",
            "status":"completed",
            "items":[{"id":"item","type":"contextCompaction"}]
            }]
        }}),
        true,
    )
    .unwrap();
    assert!(safe_checkpoint_recovery_snapshot(&journal, &thread));

    let changed = ThreadRef::from_response(
        &json!({"thread":{
            "id":"thread",
            "status":{"type":"idle"},
            "turns":[
                {
                "id":"compact",
                "status":"completed",
                "items":[{"id":"item","type":"contextCompaction"}]
                },
                {"id":"user","status":"completed","items":[]}
            ]
        }}),
        true,
    )
    .unwrap();
    assert!(!safe_checkpoint_recovery_snapshot(&journal, &changed));

    let mut duplicate = thread.clone();
    duplicate.turns.push(duplicate.turns[0].clone());
    assert!(!safe_checkpoint_recovery_snapshot(&journal, &duplicate));

    journal.compact_turn_id = Some("missing".to_owned());
    assert!(!safe_checkpoint_recovery_snapshot(&journal, &thread));
}
