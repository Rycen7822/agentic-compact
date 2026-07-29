#![cfg(unix)]

use agentic_compact::checkpoint::CompactionIntent;
use agentic_compact::journal::{JournalStore, TransitionState};
use agentic_compact::metadata::BoundInvocation;
use agentic_compact::orchestrator::Orchestrator;
use serde_json::{Value, json};
use std::time::Duration;
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
async fn exercises_happy_path_and_active_descendant_guard() {
    completes_schedule_through_same_thread_cooldown(false).await;
    completes_schedule_through_same_thread_cooldown(true).await;
    active_descendant_blocks_before_compaction().await;
    scheduling_deadline_fails_closed().await;
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
    assert_eq!(scheduled.status, "scheduled_after_turn");

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
                    "result": {"receiptId": scheduled.receipt_id}
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
            json!([{"id": "source", "status": "completed", "items": []}]),
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
                    "type": "contextCompaction",
                    "status": "completed"
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
                        "type": "contextCompaction",
                        "status": "completed"
                    }]
                }
            ]),
        ),
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

async fn active_descendant_blocks_before_compaction() {
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
                    thread_id: "root".to_owned(),
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
    });

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

    let error = scheduling.await.unwrap().unwrap_err();
    assert_eq!(
        error.code,
        agentic_compact::error::ErrorCode::ActiveSubagents
    );
    assert!(server.try_next_request().is_none());
    let journal = JournalStore::open().unwrap().load("root").unwrap().unwrap();
    assert_eq!(journal.state, TransitionState::Cancelled);
    assert_eq!(journal.reason_code.as_deref(), Some("active_subagents"));
}

async fn scheduling_deadline_fails_closed() {
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
                    thread_id: "deadline".to_owned(),
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
    });
    server.initialize_connection().await;
    let resume = request(&mut server, "thread/resume").await;
    assert_eq!(resume["params"]["threadId"], "deadline");
    pause();
    advance(Duration::from_secs(6)).await;
    tokio::task::yield_now().await;

    let error = scheduling.await.unwrap().unwrap_err();
    assert_eq!(error.code, agentic_compact::error::ErrorCode::Timeout);
    let journal = JournalStore::open()
        .unwrap()
        .load("deadline")
        .unwrap()
        .unwrap();
    assert_eq!(journal.state, TransitionState::FailedSafe);
    assert_eq!(journal.reason_code.as_deref(), Some("timeout"));
}
