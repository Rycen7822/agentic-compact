use super::*;
use crate::checkpoint::CompactionIntent;
use crate::protocol::{ItemRef, TurnRef};

fn bound() -> BoundInvocation {
    BoundInvocation {
        thread_id: "thread".to_owned(),
        turn_id: "source".to_owned(),
        model: None,
        reasoning_effort: None,
    }
}

fn turn(id: &str, status: &str) -> TurnRef {
    TurnRef {
        id: id.to_owned(),
        status: status.to_owned(),
        items: Vec::new(),
    }
}

fn item(id: &str, item_type: &str, status: Option<&str>) -> ItemRef {
    ItemRef {
        id: id.to_owned(),
        item_type: item_type.to_owned(),
        status: status.map(str::to_owned),
        server: None,
        tool: None,
        receipt_ids: Vec::new(),
        has_error: false,
        safe_evidence: Vec::new(),
    }
}

fn request_item(receipt_id: &str) -> ItemRef {
    ItemRef {
        server: Some("agentic-compact".to_owned()),
        tool: Some("request_compaction".to_owned()),
        receipt_ids: vec![receipt_id.to_owned()],
        ..item("request", "mcpToolCall", Some("completed"))
    }
}

async fn source_result(events: Vec<AppEvent>) -> Result<()> {
    let (sender, mut receiver) = broadcast::channel(32);
    for event in events {
        sender.send(event).unwrap();
    }
    let mut evidence = Evidence::default();
    await_source_turn(&mut receiver, &bound(), "receipt", &mut evidence).await
}

fn compaction_journal() -> TransitionJournal {
    let mut journal = TransitionJournal::new(
        "thread".to_owned(),
        "source".to_owned(),
        "receipt".to_owned(),
        "checkpoint".to_owned(),
        CompactionIntent {
            preserve: Vec::new(),
            next_action: "continue".to_owned(),
        },
    )
    .unwrap();
    journal
        .transition(TransitionState::AwaitSourceTurnCompleted, "attached")
        .unwrap();
    journal
        .transition(TransitionState::ReadyToCompact, "source completed")
        .unwrap();
    journal
        .transition(TransitionState::CompactRequestSent, "compact requested")
        .unwrap();
    journal
}

async fn compaction_result(events: Vec<AppEvent>) -> (Result<String>, TransitionJournal) {
    let directory = tempfile::tempdir().unwrap();
    let journals = JournalStore::for_test(directory.path().join("journals")).unwrap();
    let mut journal = compaction_journal();
    journals.save(&journal).unwrap();
    let (sender, mut receiver) = broadcast::channel(32);
    for event in events {
        sender.send(event).unwrap();
    }
    let result = await_compaction(&mut receiver, "thread", "source", &mut journal, &journals).await;
    (result, journal)
}

fn compact_started() -> AppEvent {
    AppEvent::TurnStarted {
        thread_id: "thread".to_owned(),
        turn: turn("compact", "inProgress"),
    }
}

#[tokio::test]
async fn binds_receipt_and_accepts_quiescent_source_completion() {
    source_result(vec![
        AppEvent::ItemCompleted {
            thread_id: "thread".to_owned(),
            turn_id: "source".to_owned(),
            item: request_item("receipt"),
        },
        AppEvent::ItemCompleted {
            thread_id: "thread".to_owned(),
            turn_id: "source".to_owned(),
            item: item("answer", "agentMessage", Some("completed")),
        },
        AppEvent::TurnCompleted {
            thread_id: "thread".to_owned(),
            turn: turn("source", "completed"),
        },
    ])
    .await
    .unwrap();
}

#[tokio::test]
async fn permits_passive_reasoning_after_the_receipt() {
    source_result(vec![
        AppEvent::ItemCompleted {
            thread_id: "thread".to_owned(),
            turn_id: "source".to_owned(),
            item: request_item("receipt"),
        },
        AppEvent::ItemStarted {
            thread_id: "thread".to_owned(),
            turn_id: "source".to_owned(),
            item: item("reasoning", "reasoning", None),
        },
        AppEvent::ItemCompleted {
            thread_id: "thread".to_owned(),
            turn_id: "source".to_owned(),
            item: item("reasoning", "reasoning", Some("completed")),
        },
        AppEvent::TurnCompleted {
            thread_id: "thread".to_owned(),
            turn: turn("source", "completed"),
        },
    ])
    .await
    .unwrap();
}

#[test]
fn continuation_reconciliation_requires_the_exact_turn() {
    let snapshot = crate::protocol::ThreadRef {
        id: "thread".to_owned(),
        parent_thread_id: None,
        status: "active".to_owned(),
        turns: vec![turn("continuation", "inProgress")],
    };

    assert!(contains_exact_turn(&snapshot, "continuation"));
    assert!(!contains_exact_turn(&snapshot, "other"));
}

#[tokio::test]
async fn rejects_activity_after_the_receipt() {
    let error = source_result(vec![
        AppEvent::ItemCompleted {
            thread_id: "thread".to_owned(),
            turn_id: "source".to_owned(),
            item: request_item("receipt"),
        },
        AppEvent::ItemStarted {
            thread_id: "thread".to_owned(),
            turn_id: "source".to_owned(),
            item: item("command", "commandExecution", None),
        },
    ])
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::QuiescenceViolation);
}

#[tokio::test]
async fn competing_turn_wins_before_source_completion() {
    let error = source_result(vec![
        AppEvent::ItemCompleted {
            thread_id: "thread".to_owned(),
            turn_id: "source".to_owned(),
            item: request_item("receipt"),
        },
        AppEvent::TurnStarted {
            thread_id: "thread".to_owned(),
            turn: turn("user", "inProgress"),
        },
    ])
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::RaceLost);
}

#[tokio::test]
async fn connection_loss_while_awaiting_source_is_unavailable() {
    let error = source_result(vec![AppEvent::ConnectionClosed {
        reason: "test close".to_owned(),
    }])
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::SharedAppServerUnavailable);
}

#[tokio::test]
async fn accepts_exactly_one_successful_compaction_item() {
    let (result, journal) = compaction_result(vec![
        compact_started(),
        AppEvent::ItemStarted {
            thread_id: "thread".to_owned(),
            turn_id: "compact".to_owned(),
            item: item("compact-item", "contextCompaction", None),
        },
        AppEvent::ItemCompleted {
            thread_id: "thread".to_owned(),
            turn_id: "compact".to_owned(),
            item: item("compact-item", "contextCompaction", Some("completed")),
        },
        AppEvent::TurnCompleted {
            thread_id: "thread".to_owned(),
            turn: turn("compact", "completed"),
        },
    ])
    .await;
    assert_eq!(result.unwrap(), "compact");
    assert_eq!(journal.state, TransitionState::AwaitCompactionTurnCompleted);
    assert_eq!(journal.compact_turn_id.as_deref(), Some("compact"));
}

#[tokio::test]
async fn duplicate_compaction_notifications_are_idempotent() {
    let (result, journal) = compaction_result(vec![
        compact_started(),
        compact_started(),
        AppEvent::ItemStarted {
            thread_id: "thread".to_owned(),
            turn_id: "compact".to_owned(),
            item: item("compact-item", "contextCompaction", None),
        },
        AppEvent::ItemStarted {
            thread_id: "thread".to_owned(),
            turn_id: "compact".to_owned(),
            item: item("compact-item", "contextCompaction", None),
        },
        AppEvent::ItemCompleted {
            thread_id: "thread".to_owned(),
            turn_id: "compact".to_owned(),
            item: item("compact-item", "contextCompaction", Some("completed")),
        },
        AppEvent::ItemCompleted {
            thread_id: "thread".to_owned(),
            turn_id: "compact".to_owned(),
            item: item("compact-item", "contextCompaction", Some("completed")),
        },
        AppEvent::TurnCompleted {
            thread_id: "thread".to_owned(),
            turn: turn("compact", "completed"),
        },
    ])
    .await;
    assert_eq!(result.unwrap(), "compact");
    assert_eq!(journal.state, TransitionState::AwaitCompactionTurnCompleted);
}

#[tokio::test]
async fn compaction_completion_before_start_fails_closed() {
    let (result, _) = compaction_result(vec![
        compact_started(),
        AppEvent::ItemCompleted {
            thread_id: "thread".to_owned(),
            turn_id: "compact".to_owned(),
            item: item("compact-item", "contextCompaction", Some("completed")),
        },
    ])
    .await;
    assert_eq!(result.unwrap_err().code, ErrorCode::RecoveryAmbiguous);
}

#[tokio::test]
async fn unknown_compaction_item_loses_the_race() {
    let (result, _) = compaction_result(vec![
        compact_started(),
        AppEvent::ItemStarted {
            thread_id: "thread".to_owned(),
            turn_id: "compact".to_owned(),
            item: item("unexpected", "commandExecution", None),
        },
    ])
    .await;
    assert_eq!(result.unwrap_err().code, ErrorCode::RaceLost);
}

#[tokio::test]
async fn rejects_compaction_turn_without_compaction_item() {
    let (result, _) = compaction_result(vec![
        compact_started(),
        AppEvent::TurnCompleted {
            thread_id: "thread".to_owned(),
            turn: turn("compact", "completed"),
        },
    ])
    .await;
    assert_eq!(result.unwrap_err().code, ErrorCode::RecoveryAmbiguous);
}

#[tokio::test]
async fn rejects_second_compaction_item() {
    let (result, _) = compaction_result(vec![
        compact_started(),
        AppEvent::ItemStarted {
            thread_id: "thread".to_owned(),
            turn_id: "compact".to_owned(),
            item: item("compact-one", "contextCompaction", None),
        },
        AppEvent::ItemStarted {
            thread_id: "thread".to_owned(),
            turn_id: "compact".to_owned(),
            item: item("compact-two", "contextCompaction", None),
        },
    ])
    .await;
    assert_eq!(result.unwrap_err().code, ErrorCode::RecoveryAmbiguous);
}

#[tokio::test]
async fn rejects_failed_compaction_item() {
    let mut failed = item("compact-item", "contextCompaction", Some("failed"));
    failed.has_error = true;
    let (result, _) = compaction_result(vec![
        compact_started(),
        AppEvent::ItemStarted {
            thread_id: "thread".to_owned(),
            turn_id: "compact".to_owned(),
            item: item("compact-item", "contextCompaction", None),
        },
        AppEvent::ItemCompleted {
            thread_id: "thread".to_owned(),
            turn_id: "compact".to_owned(),
            item: failed,
        },
        AppEvent::TurnCompleted {
            thread_id: "thread".to_owned(),
            turn: turn("compact", "completed"),
        },
    ])
    .await;
    assert_eq!(result.unwrap_err().code, ErrorCode::CompactionFailed);
}

#[tokio::test]
async fn user_item_wins_compaction_race() {
    let (result, _) = compaction_result(vec![
        AppEvent::TurnStarted {
            thread_id: "thread".to_owned(),
            turn: turn("user", "inProgress"),
        },
        AppEvent::ItemStarted {
            thread_id: "thread".to_owned(),
            turn_id: "user".to_owned(),
            item: item("user-message", "userMessage", None),
        },
    ])
    .await;
    assert_eq!(result.unwrap_err().code, ErrorCode::RaceLost);
}

#[tokio::test]
async fn connection_loss_during_compaction_is_ambiguous() {
    let (result, _) = compaction_result(vec![
        compact_started(),
        AppEvent::ConnectionClosed {
            reason: "test close".to_owned(),
        },
    ])
    .await;
    assert_eq!(result.unwrap_err().code, ErrorCode::RecoveryAmbiguous);
}

#[tokio::test]
async fn connection_loss_before_continuation_start_is_ambiguous() {
    let (sender, mut receiver) = broadcast::channel(4);
    sender
        .send(AppEvent::ConnectionClosed {
            reason: "test close".to_owned(),
        })
        .unwrap();
    let error = await_continuation_started(&mut receiver, "thread", "continuation")
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::RecoveryAmbiguous);
}

#[tokio::test]
async fn server_requests_abort_every_event_boundary() {
    let server_request = || AppEvent::ServerRequest {
        method: "item/commandExecution/requestApproval".to_owned(),
        id: serde_json::Value::from(1),
    };
    assert_eq!(
        source_result(vec![server_request()])
            .await
            .unwrap_err()
            .code,
        ErrorCode::ServerRequestReceived
    );
    assert_eq!(
        compaction_result(vec![server_request()])
            .await
            .0
            .unwrap_err()
            .code,
        ErrorCode::ServerRequestReceived
    );

    let (sender, mut receiver) = broadcast::channel(2);
    sender.send(server_request()).unwrap();
    assert_eq!(
        await_continuation_started(&mut receiver, "thread", "continuation")
            .await
            .unwrap_err()
            .code,
        ErrorCode::ServerRequestReceived
    );

    let (sender, mut receiver) = broadcast::channel(2);
    sender.send(server_request()).unwrap();
    assert_eq!(
        reject_queued_competing_turn(&mut receiver, "thread", "compact")
            .unwrap_err()
            .code,
        ErrorCode::ServerRequestReceived
    );

    let (sender, mut receiver) = broadcast::channel(2);
    sender.send(server_request()).unwrap();
    assert_eq!(
        drain_started_turn(&mut receiver, "thread")
            .unwrap_err()
            .code,
        ErrorCode::ServerRequestReceived
    );
}

#[test]
fn queued_user_turn_wins_before_and_after_checkpoint_injection() {
    let user_turn = || AppEvent::TurnStarted {
        thread_id: "thread".to_owned(),
        turn: turn("user", "inProgress"),
    };
    let (sender, mut receiver) = broadcast::channel(2);
    sender.send(user_turn()).unwrap();
    assert_eq!(
        reject_queued_competing_turn(&mut receiver, "thread", "compact")
            .unwrap_err()
            .code,
        ErrorCode::RaceLost
    );

    let (sender, mut receiver) = broadcast::channel(2);
    sender.send(user_turn()).unwrap();
    assert_eq!(
        drain_started_turn(&mut receiver, "thread")
            .unwrap()
            .as_deref(),
        Some("user")
    );
}

#[test]
fn connection_loss_at_transition_boundaries_fails_closed() {
    let (sender, mut receiver) = broadcast::channel(4);
    sender
        .send(AppEvent::ConnectionClosed {
            reason: "test close".to_owned(),
        })
        .unwrap();
    let error = reject_queued_competing_turn(&mut receiver, "thread", "source").unwrap_err();
    assert_eq!(error.code, ErrorCode::SharedAppServerUnavailable);

    let (sender, mut receiver) = broadcast::channel(4);
    sender
        .send(AppEvent::ConnectionClosed {
            reason: "test close".to_owned(),
        })
        .unwrap();
    let error = drain_started_turn(&mut receiver, "thread").unwrap_err();
    assert_eq!(error.code, ErrorCode::RecoveryAmbiguous);
}
