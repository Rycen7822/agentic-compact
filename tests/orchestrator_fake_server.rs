#![cfg(unix)]

use agentic_compact::checkpoint::CompactionIntent;
use agentic_compact::error::Result;
use agentic_compact::journal::{JournalStore, TransitionJournal, TransitionState};
use agentic_compact::metadata::BoundInvocation;
use agentic_compact::observability::hash_identifier;
use agentic_compact::orchestrator::{Orchestrator, ScheduleResult};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::{advance, pause, timeout};

mod support;

use support::{EnvironmentGuard, FakeServer, write_ready_capability};

async fn request(server: &mut FakeServer, method: &str) -> Value {
    let request = server.next_request().await;
    assert_eq!(request["method"], method);
    request
}

async fn respond(server: &FakeServer, request: &Value, result: Value) {
    server
        .send(json!({"id": request["id"], "result": result}))
        .await;
}

async fn acknowledge_unsubscribe(server: &mut FakeServer, thread_id: &str) {
    let unsubscribe = request(server, "thread/unsubscribe").await;
    assert_eq!(unsubscribe["params"]["threadId"], thread_id);
    respond(server, &unsubscribe, json!({})).await;
}

fn journal_path(state_root: &Path, thread_id: &str) -> PathBuf {
    state_root
        .join("journals")
        .join(format!("{}.json", hash_identifier(thread_id)))
}

fn persist_terminal_journal(
    state_root: &Path,
    thread_id: &str,
    state: TransitionState,
    continuation_turn_id: Option<&str>,
) -> (PathBuf, Vec<u8>) {
    let mut journal = TransitionJournal::new(
        thread_id.to_owned(),
        "prior-source".to_owned(),
        "prior-receipt".to_owned(),
        "prior-checkpoint".to_owned(),
        CompactionIntent {
            preserve: Vec::new(),
            next_action: "prior continuation".to_owned(),
        },
    )
    .unwrap();
    journal.state = state;
    journal.continuation_turn_id = continuation_turn_id.map(str::to_owned);
    journal.reason_code =
        (state == TransitionState::FailedSafe).then(|| "prior_terminal".to_owned());
    let store = JournalStore::open().unwrap();
    store.save(&journal).unwrap();
    let path = journal_path(state_root, thread_id);
    let bytes = std::fs::read(&path).unwrap();
    (path, bytes)
}

fn schedule(thread_id: &str) -> JoinHandle<Result<ScheduleResult>> {
    let orchestrator = Orchestrator::new().unwrap();
    let thread_id = thread_id.to_owned();
    tokio::spawn(async move {
        orchestrator
            .schedule(
                BoundInvocation {
                    thread_id,
                    turn_id: "source".to_owned(),
                    model: None,
                    reasoning_effort: None,
                },
                CompactionIntent {
                    preserve: Vec::new(),
                    next_action: "continue".to_owned(),
                },
            )
            .await
    })
}

fn thread(id: &str, status: &str, turns: Value) -> Value {
    json!({
        "thread": {
            "id": id,
            "status": {"type": status},
            "turns": turns
        }
    })
}

#[tokio::test]
async fn exercises_schedule_acceptance_and_preflight_rejections() {
    completes_schedule_through_same_thread_cooldown(false).await;
    completes_schedule_through_same_thread_cooldown(true).await;
    cooldown_rejection_preserves_prior_journal_bytes().await;
    repeated_cooldown_rejections_cannot_erase_continuation_anchor().await;
    root_rejection_does_not_create_journal().await;
    active_descendant_rejection_does_not_create_journal().await;
    prepare_timeout_does_not_create_journal().await;
}

async fn completes_schedule_through_same_thread_cooldown(user_wins_after_injection: bool) {
    let mut server = FakeServer::start().await;
    let state_root = server.codex_home().parent().unwrap().join("state");
    let _codex_home = EnvironmentGuard::set("CODEX_HOME", server.codex_home());
    let _state_root = EnvironmentGuard::set("AGENTIC_COMPACT_STATE_DIR", &state_root);
    write_ready_capability(server.codex_home());

    let orchestrator = Orchestrator::new().unwrap();
    let scheduling = tokio::spawn(async move {
        orchestrator
            .schedule(
                BoundInvocation {
                    thread_id: "thread".to_owned(),
                    turn_id: "source".to_owned(),
                    model: None,
                    reasoning_effort: None,
                },
                CompactionIntent {
                    preserve: vec!["keep invariant".to_owned()],
                    next_action: "run verification".to_owned(),
                },
            )
            .await
    });

    server.initialize_connection().await;
    let resume = request(&mut server, "thread/resume").await;
    respond(
        &server,
        &resume,
        thread(
            "thread",
            "active",
            json!([{"id": "source", "status": "inProgress", "items": []}]),
        ),
    )
    .await;
    let loaded = request(&mut server, "thread/loaded/list").await;
    respond(
        &server,
        &loaded,
        json!({"data": ["thread"], "nextCursor": null}),
    )
    .await;

    let scheduled = scheduling.await.unwrap().unwrap();
    let accepted = JournalStore::open()
        .unwrap()
        .load("thread")
        .unwrap()
        .unwrap();
    assert_eq!(accepted.state, TransitionState::AwaitSourceTurnCompleted);
    assert_eq!(accepted.receipt_id, scheduled.receipt_id);
    assert!(accepted.checkpoint_id.starts_with("cp_"));

    server
        .send(json!({
            "method": "item/completed",
            "params": {
                "threadId": "thread",
                "turnId": "source",
                "completedAtMs": 1,
                "item": {
                    "id": "request",
                    "type": "mcpToolCall",
                    "status": "completed",
                    "server": "agentic-compact",
                    "tool": "request_compaction",
                    "result": {
                        "_meta": {"agenticCompact": {"receiptId": scheduled.receipt_id}}
                    }
                }
            }
        }))
        .await;
    server
        .send(json!({
            "method": "turn/completed",
            "params": {
                "threadId": "thread",
                "turn": {"id": "source", "status": "completed", "items": []}
            }
        }))
        .await;

    let source_read = request(&mut server, "thread/read").await;
    respond(
        &server,
        &source_read,
        thread(
            "thread",
            "idle",
            json!([{
                "id": "source",
                "status": "completed",
                "items": [{
                    "id": "request",
                    "type": "mcpToolCall",
                    "status": "completed",
                    "server": "agentic-compact",
                    "tool": "request_compaction",
                    "result": {
                        "_meta": {"agenticCompact": {"receiptId": scheduled.receipt_id}}
                    }
                }]
            }]),
        ),
    )
    .await;
    let loaded = request(&mut server, "thread/loaded/list").await;
    respond(
        &server,
        &loaded,
        json!({"data": ["thread"], "nextCursor": null}),
    )
    .await;
    let compact = request(&mut server, "thread/compact/start").await;
    respond(&server, &compact, json!({})).await;

    server
        .send(json!({
            "method": "turn/started",
            "params": {
                "threadId": "thread",
                "turn": {"id": "compact", "status": "inProgress", "items": []}
            }
        }))
        .await;
    server
        .send(json!({
            "method": "item/started",
            "params": {
                "threadId": "thread",
                "turnId": "compact",
                "startedAtMs": 2,
                "item": {"id": "compact-item", "type": "contextCompaction"}
            }
        }))
        .await;
    server
        .send(json!({
            "method": "item/completed",
            "params": {
                "threadId": "thread",
                "turnId": "compact",
                "completedAtMs": 3,
                "item": {
                    "id": "compact-item",
                    "type": "contextCompaction"
                }
            }
        }))
        .await;
    server
        .send(json!({
            "method": "turn/completed",
            "params": {
                "threadId": "thread",
                "turn": {"id": "compact", "status": "completed", "items": []}
            }
        }))
        .await;

    let post_compact = request(&mut server, "thread/read").await;
    respond(
        &server,
        &post_compact,
        thread(
            "thread",
            "idle",
            json!([
                {"id": "source", "status": "completed", "items": []},
                {
                    "id": "compact",
                    "status": "completed",
                    "items": [{
                        "id": "compact-item",
                        "type": "contextCompaction"
                    }]
                }
            ]),
        ),
    )
    .await;

    let loaded = request(&mut server, "thread/loaded/list").await;
    respond(
        &server,
        &loaded,
        json!({"data": ["thread"], "nextCursor": null}),
    )
    .await;
    let injection = request(&mut server, "thread/inject_items").await;
    assert_eq!(injection["params"]["threadId"], "thread");
    assert_eq!(injection["params"]["items"].as_array().unwrap().len(), 2);
    if user_wins_after_injection {
        server
            .send(json!({
                "method": "turn/started",
                "params": {
                    "threadId": "thread",
                    "turn": {
                        "id": "user",
                        "status": "inProgress",
                        "items": []
                    }
                }
            }))
            .await;
    }
    respond(&server, &injection, json!({})).await;

    let expected_continuation = if user_wins_after_injection {
        "user"
    } else {
        let continuation = request(&mut server, "turn/start").await;
        assert_eq!(continuation["params"]["input"], json!([]));
        respond(
            &server,
            &continuation,
            json!({
                "turn": {
                    "id": "continuation",
                    "status": "inProgress",
                    "items": []
                }
            }),
        )
        .await;
        server
            .send(json!({
                "method": "turn/started",
                "params": {
                    "threadId": "thread",
                    "turn": {
                        "id": "continuation",
                        "status": "inProgress",
                        "items": []
                    }
                }
            }))
            .await;
        "continuation"
    };

    let unsubscribe = request(&mut server, "thread/unsubscribe").await;
    respond(&server, &unsubscribe, json!({})).await;

    let journals = JournalStore::open().unwrap();
    let journal = timeout(Duration::from_secs(2), async {
        loop {
            if let Some(journal) = journals.load("thread").unwrap() {
                if journal.state == TransitionState::Cooldown {
                    break journal;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "journal did not reach cooldown: {:?}",
            journals
                .load("thread")
                .unwrap()
                .map(|journal| (journal.state, journal.reason_code))
        )
    });
    assert_eq!(journal.source_turn_id, "source");
    assert_eq!(journal.compact_turn_id.as_deref(), Some("compact"));
    assert_eq!(
        journal.continuation_turn_id.as_deref(),
        Some(expected_continuation)
    );
    assert!(journal.checkpoint.is_some());
    assert!(journal.checkpoint_sha256.is_some());
    assert!(journal.reason_code.is_none());
}

async fn cooldown_rejection_preserves_prior_journal_bytes() {
    cooldown_rejection_case(1).await;
}

async fn repeated_cooldown_rejections_cannot_erase_continuation_anchor() {
    cooldown_rejection_case(2).await;
}

async fn cooldown_rejection_case(repetitions: usize) {
    let state_directory = tempfile::tempdir().unwrap();
    let state_root = state_directory.path().join("state");
    let _state_root = EnvironmentGuard::set("AGENTIC_COMPACT_STATE_DIR", &state_root);
    let (path, prior_bytes) = persist_terminal_journal(
        &state_root,
        "cooldown",
        TransitionState::Cooldown,
        Some("continuation"),
    );

    for _ in 0..repetitions {
        let mut server = FakeServer::start().await;
        let _codex_home = EnvironmentGuard::set("CODEX_HOME", server.codex_home());
        write_ready_capability(server.codex_home());
        let scheduling = schedule("cooldown");

        server.initialize_connection().await;
        let resume = request(&mut server, "thread/resume").await;
        respond(
            &server,
            &resume,
            thread(
                "cooldown",
                "active",
                json!([
                    {"id":"continuation","status":"completed","items":[]},
                    {"id":"source","status":"inProgress","items":[]}
                ]),
            ),
        )
        .await;
        acknowledge_unsubscribe(&mut server, "cooldown").await;
        let error = scheduling.await.unwrap().unwrap_err();
        assert_eq!(
            error.code,
            agentic_compact::error::ErrorCode::CooldownActive
        );
        assert_eq!(std::fs::read(&path).unwrap(), prior_bytes);
        assert_eq!(
            JournalStore::open()
                .unwrap()
                .load("cooldown")
                .unwrap()
                .unwrap()
                .continuation_turn_id
                .as_deref(),
            Some("continuation")
        );
    }
}

async fn root_rejection_does_not_create_journal() {
    let mut server = FakeServer::start().await;
    let state_root = server.codex_home().parent().unwrap().join("state");
    let _codex_home = EnvironmentGuard::set("CODEX_HOME", server.codex_home());
    let _state_root = EnvironmentGuard::set("AGENTIC_COMPACT_STATE_DIR", &state_root);
    write_ready_capability(server.codex_home());

    let scheduling = schedule("child");

    server.initialize_connection().await;
    let resume = request(&mut server, "thread/resume").await;
    respond(
        &server,
        &resume,
        json!({"thread":{
            "id":"child",
            "parentThreadId":"root",
            "status":{"type":"active"},
            "turns":[{"id":"source","status":"inProgress","items":[]}]
        }}),
    )
    .await;
    acknowledge_unsubscribe(&mut server, "child").await;
    let error = scheduling.await.unwrap().unwrap_err();
    assert_eq!(error.code, agentic_compact::error::ErrorCode::NotRootThread);
    assert!(
        JournalStore::open()
            .unwrap()
            .load("child")
            .unwrap()
            .is_none()
    );
}

async fn active_descendant_rejection_does_not_create_journal() {
    let mut server = FakeServer::start().await;
    let state_root = server.codex_home().parent().unwrap().join("state");
    let _codex_home = EnvironmentGuard::set("CODEX_HOME", server.codex_home());
    let _state_root = EnvironmentGuard::set("AGENTIC_COMPACT_STATE_DIR", &state_root);
    write_ready_capability(server.codex_home());
    let (prior_path, prior_bytes) =
        persist_terminal_journal(&state_root, "root", TransitionState::FailedSafe, None);

    let scheduling = schedule("root");

    server.initialize_connection().await;
    let resume = request(&mut server, "thread/resume").await;
    respond(
        &server,
        &resume,
        thread(
            "root",
            "active",
            json!([{"id":"source","status":"inProgress","items":[]}]),
        ),
    )
    .await;
    let loaded = request(&mut server, "thread/loaded/list").await;
    respond(
        &server,
        &loaded,
        json!({"data":["root","child"],"nextCursor":null}),
    )
    .await;
    let child = request(&mut server, "thread/read").await;
    respond(
        &server,
        &child,
        json!({"thread":{
            "id":"child",
            "parentThreadId":"root",
            "status":{"type":"active"},
            "turns":[]
        }}),
    )
    .await;

    acknowledge_unsubscribe(&mut server, "root").await;
    let error = scheduling.await.unwrap().unwrap_err();
    assert_eq!(
        error.code,
        agentic_compact::error::ErrorCode::ActiveSubagents
    );
    assert!(server.try_next_request().is_none());
    assert_eq!(std::fs::read(prior_path).unwrap(), prior_bytes);
}

async fn prepare_timeout_does_not_create_journal() {
    let mut server = FakeServer::start().await;
    let state_root = server.codex_home().parent().unwrap().join("state");
    let _codex_home = EnvironmentGuard::set("CODEX_HOME", server.codex_home());
    let _state_root = EnvironmentGuard::set("AGENTIC_COMPACT_STATE_DIR", &state_root);
    write_ready_capability(server.codex_home());

    let scheduling = schedule("deadline");
    server.initialize_connection().await;
    let resume = request(&mut server, "thread/resume").await;
    assert_eq!(resume["params"]["threadId"], "deadline");
    pause();
    advance(Duration::from_secs(6)).await;
    tokio::task::yield_now().await;

    let unsubscribe = request(&mut server, "thread/unsubscribe").await;
    assert_eq!(unsubscribe["params"]["threadId"], "deadline");
    advance(Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    let error = scheduling.await.unwrap().unwrap_err();
    assert_eq!(error.code, agentic_compact::error::ErrorCode::Timeout);
    assert!(
        JournalStore::open()
            .unwrap()
            .load("deadline")
            .unwrap()
            .is_none()
    );
}
