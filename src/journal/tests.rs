use super::*;
use crate::checkpoint::Evidence;

fn journal() -> TransitionJournal {
    TransitionJournal::new(
        "thread".to_owned(),
        "turn".to_owned(),
        "receipt".to_owned(),
        "checkpoint".to_owned(),
        CompactionIntent {
            preserve: Vec::new(),
            next_action: "continue".to_owned(),
        },
    )
    .unwrap()
}

#[test]
fn validates_state_sequence_and_set_once_bindings() {
    let mut transition = journal();
    transition
        .transition(TransitionState::AwaitSourceTurnCompleted, "attached")
        .unwrap();
    assert!(
        transition
            .transition(TransitionState::InjectingCheckpoint, "invalid")
            .is_err()
    );

    transition.set_compact_turn("compact".to_owned()).unwrap();
    transition.set_compact_turn("compact".to_owned()).unwrap();
    let error = transition
        .set_compact_turn("different".to_owned())
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::RecoveryAmbiguous);

    let mut user_wins = journal();
    user_wins.state = TransitionState::StartingContinuation;
    user_wins
        .transition(
            TransitionState::Cooldown,
            "user turn consumed the checkpoint",
        )
        .unwrap();
}

#[test]
fn atomically_round_trips_and_filters_terminal_journals() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("journals");
    let store = JournalStore::for_test(root.clone()).unwrap();
    let mut journal = journal();
    store.save(&journal).unwrap();

    let loaded = store.load("thread").unwrap().unwrap();
    assert_eq!(loaded.thread_id, "thread");
    assert_eq!(loaded.state, TransitionState::Attaching);
    assert_eq!(store.nonterminal().unwrap().len(), 1);

    let entries = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].to_string_lossy().ends_with(".json"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(store.path_for("thread"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    journal.cancel("test_complete");
    store.save(&journal).unwrap();
    assert!(store.nonterminal().unwrap().is_empty());
}

#[test]
fn rejects_oversized_serialized_journal() {
    let directory = tempfile::tempdir().unwrap();
    let store = JournalStore::for_test(directory.path().join("journals")).unwrap();
    let mut journal = journal();
    journal.intent.next_action = "x".repeat(70 * 1024);
    let error = store.save(&journal).unwrap_err();
    assert_eq!(error.code, ErrorCode::Internal);
}

#[test]
fn persists_one_identity_bound_checkpoint_capsule() {
    let directory = tempfile::tempdir().unwrap();
    let store = JournalStore::for_test(directory.path().join("journals")).unwrap();
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
    journal.set_compact_turn("compact".to_owned()).unwrap();
    let checkpoint = Checkpoint::build(
        journal.checkpoint_id.clone(),
        journal.receipt_id.clone(),
        journal.thread_id.clone(),
        journal.source_turn_id.clone(),
        "compact".to_owned(),
        journal.intent.clone(),
        Evidence::default(),
    )
    .unwrap();
    journal.set_checkpoint(checkpoint.clone()).unwrap();
    store.save(&journal).unwrap();

    let restored = store.load("thread").unwrap().unwrap();
    assert_eq!(
        restored.checkpoint.as_ref().unwrap().sha256,
        checkpoint.sha256
    );
    restored.checkpoint.as_ref().unwrap().verify().unwrap();

    let mismatched = Checkpoint::build(
        format!("cp_{}", "c".repeat(32)),
        journal.receipt_id.clone(),
        journal.thread_id.clone(),
        journal.source_turn_id.clone(),
        "compact".to_owned(),
        journal.intent.clone(),
        Evidence::default(),
    )
    .unwrap();
    assert_eq!(
        journal.set_checkpoint(mismatched).unwrap_err().code,
        ErrorCode::RecoveryAmbiguous
    );
}
