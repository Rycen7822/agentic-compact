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
        receipt_id: None,
        has_error: false,
        safe_evidence: Vec::new(),
    }
}

fn request_item(receipt_id: &str) -> ItemRef {
    ItemRef {
        server: Some("agentic-compact".to_owned()),
        tool: Some("request_compaction".to_owned()),
        receipt_id: Some(receipt_id.to_owned()),
        ..item("request", "mcpToolCall", Some("completed"))
    }
}

async fn source_result(events: Vec<AppEvent>) -> Result<()> {
    let (sender, mut receiver) = broadcast::channel(32);
    for event in events {
        sender.send(event).unwrap();
    }
    await_source_turn(&mut receiver, &bound()).await
}

fn source_snapshot(items: Vec<ItemRef>) -> ThreadRef {
    ThreadRef {
        id: "thread".to_owned(),
        parent_thread_id: None,
        status: "idle".to_owned(),
        turns: vec![TurnRef {
            items,
            ..turn("source", "completed")
        }],
    }
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
async fn source_wait_uses_only_lifecycle_and_accepts_event_lag() {
    source_result(vec![
        AppEvent::RequestCompactionResultInvalid {
            thread_id: "thread".to_owned(),
            turn_id: "source".to_owned(),
        },
        AppEvent::ItemCompleted {
            thread_id: "thread".to_owned(),
            turn_id: "source".to_owned(),
            item: request_item("stale-event-receipt"),
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

#[test]
fn final_snapshot_requires_one_successful_matching_receipt() {
    for observed in [None, Some("other-receipt")] {
        let mut request = request_item("receipt");
        request.receipt_id = observed.map(str::to_owned);
        assert_eq!(
            validate_final_source_snapshot(&source_snapshot(vec![request]), "source", "receipt")
                .unwrap_err()
                .code,
            ErrorCode::Protocol
        );
    }

    let mut failed = request_item("receipt");
    failed.has_error = true;
    assert_eq!(
        validate_final_source_snapshot(&source_snapshot(vec![failed]), "source", "receipt")
            .unwrap_err()
            .code,
        ErrorCode::SourceTurnFailed
    );
    assert_eq!(
        validate_final_source_snapshot(
            &source_snapshot(vec![request_item("receipt"), request_item("receipt")]),
            "source",
            "receipt",
        )
        .unwrap_err()
        .code,
        ErrorCode::RecoveryAmbiguous
    );
}

#[test]
fn final_snapshot_accepts_passive_items_after_the_receipt() {
    let snapshot = source_snapshot(vec![
        item("before", "commandExecution", Some("completed")),
        request_item("receipt"),
        item("reasoning", "reasoning", Some("completed")),
        item("answer", "agentMessage", Some("completed")),
    ]);
    validate_final_source_snapshot(&snapshot, "source", "receipt").unwrap();
}

#[test]
fn continuation_reconciliation_requires_a_unique_turn() {
    let mut snapshot = crate::protocol::ThreadRef {
        id: "thread".to_owned(),
        parent_thread_id: None,
        status: "active".to_owned(),
        turns: vec![turn("continuation", "inProgress")],
    };

    assert!(snapshot.unique_turn("continuation").is_ok());
    assert!(snapshot.unique_turn("other").is_err());
    snapshot.turns.push(turn("continuation", "completed"));
    assert!(snapshot.unique_turn("continuation").is_err());
}

#[test]
fn mutation_boundaries_require_unique_completed_exact_last_turns() {
    let source = source_snapshot(vec![request_item("receipt")]);
    assert!(validate_final_source_snapshot(&source, "source", "receipt").is_ok());
    let mut active_source = source.clone();
    active_source.status = "active".to_owned();
    assert_eq!(
        validate_final_source_snapshot(&active_source, "source", "receipt")
            .unwrap_err()
            .code,
        ErrorCode::RaceLost
    );
    assert_eq!(
        validate_final_source_snapshot(&source, "missing", "receipt")
            .unwrap_err()
            .code,
        ErrorCode::RecoveryAmbiguous
    );

    let mut duplicate = source.clone();
    duplicate.turns.push(turn("source", "completed"));
    assert_eq!(
        validate_final_source_snapshot(&duplicate, "source", "receipt")
            .unwrap_err()
            .code,
        ErrorCode::RecoveryAmbiguous
    );

    let mut newer = source.clone();
    newer.turns.push(turn("newer", "completed"));
    assert_eq!(
        validate_final_source_snapshot(&newer, "source", "receipt")
            .unwrap_err()
            .code,
        ErrorCode::RecoveryAmbiguous
    );

    let compact = crate::protocol::ThreadRef {
        turns: vec![TurnRef {
            items: vec![item("compact-item", "contextCompaction", None)],
            ..turn("compact", "completed")
        }],
        ..source
    };
    assert!(require_completed_compaction_boundary(&compact, "compact").is_ok());

    let mut impure = compact.clone();
    impure.turns[0]
        .items
        .push(item("message", "agentMessage", Some("completed")));
    assert_eq!(
        require_completed_compaction_boundary(&impure, "compact")
            .unwrap_err()
            .code,
        ErrorCode::CompactionFailed
    );

    let mut active_compact = compact.clone();
    active_compact.status = "active".to_owned();
    assert_eq!(
        require_completed_compaction_boundary(&active_compact, "compact")
            .unwrap_err()
            .code,
        ErrorCode::CompactionFailed
    );
}

#[test]
fn final_snapshot_rejects_non_passive_work_after_the_receipt() {
    let snapshot = source_snapshot(vec![
        request_item("receipt"),
        item("command", "commandExecution", Some("completed")),
    ]);
    assert_eq!(
        validate_final_source_snapshot(&snapshot, "source", "receipt")
            .unwrap_err()
            .code,
        ErrorCode::QuiescenceViolation
    );
}

#[test]
fn final_snapshot_rejects_native_compaction_before_or_after_request() {
    for items in [
        vec![
            item("compact", "contextCompaction", None),
            request_item("receipt"),
        ],
        vec![
            request_item("receipt"),
            item("compact", "contextCompaction", None),
        ],
    ] {
        assert_eq!(
            validate_final_source_snapshot(&source_snapshot(items), "source", "receipt")
                .unwrap_err()
                .code,
            ErrorCode::RecentNativeCompaction
        );
    }
}

#[test]
fn final_snapshot_maps_active_work_to_policy_rejection() {
    let snapshot = source_snapshot(vec![
        item("work", "commandExecution", Some("inProgress")),
        request_item("receipt"),
    ]);
    assert_eq!(
        validate_final_source_snapshot(&snapshot, "source", "receipt")
            .unwrap_err()
            .code,
        ErrorCode::ActiveWork
    );
}

#[test]
fn final_snapshot_policy_rejections_cancel_the_accepted_transition() {
    assert!(is_cancellation_error(ErrorCode::RecentNativeCompaction));
    assert!(is_cancellation_error(ErrorCode::ActiveWork));
    assert!(!is_cancellation_error(ErrorCode::Protocol));
}

#[tokio::test]
async fn competing_turn_wins_before_source_completion() {
    let error = source_result(vec![AppEvent::TurnStarted {
        thread_id: "thread".to_owned(),
        turn: turn("user", "inProgress"),
    }])
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
