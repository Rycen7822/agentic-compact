#![cfg(unix)]

use agentic_compact::checkpoint::{Checkpoint, CompactionIntent, Evidence};
use agentic_compact::journal::{JournalStore, TransitionJournal, TransitionState};
use agentic_compact::orchestrator::Orchestrator;
use serde_json::{Value, json};

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

fn journal() -> TransitionJournal {
    TransitionJournal::new(
        "thread".to_owned(),
        "source".to_owned(),
        format!("rcpt_{}", "a".repeat(32)),
        format!("cp_{}", "b".repeat(32)),
        CompactionIntent {
            preserve: vec!["keep invariant".to_owned()],
            next_action: "continue recovery".to_owned(),
        },
    )
    .unwrap()
}

fn persisted_checkpoint_journal() -> TransitionJournal {
    let mut journal = journal();
    journal
        .transition(TransitionState::AwaitSourceTurnCompleted, "attached")
        .unwrap();
    journal
        .transition(TransitionState::ReadyToCompact, "source completed")
        .unwrap();
    journal
        .transition(TransitionState::CompactRequestSent, "compact sent")
        .unwrap();
    journal.set_compact_turn("compact".to_owned()).unwrap();
    journal
        .transition(TransitionState::AwaitCompactionItem, "compact bound")
        .unwrap();
    journal
        .transition(
            TransitionState::AwaitCompactionTurnCompleted,
            "compact completed",
        )
        .unwrap();
    journal
        .set_checkpoint(
            Checkpoint::build(
                journal.checkpoint_id.clone(),
                journal.receipt_id.clone(),
                journal.thread_id.clone(),
                journal.source_turn_id.clone(),
                "compact".to_owned(),
                journal.intent.clone(),
                Evidence::default(),
            )
            .unwrap(),
        )
        .unwrap();
    journal
}

#[tokio::test]
async fn recovers_only_uniquely_provable_mutation_boundaries() {
    recover_persisted_checkpoint().await;
    confirm_exact_continuation().await;
    user_turn_blocks_checkpoint_recovery().await;
}

async fn recover_persisted_checkpoint() {
    let mut server = FakeServer::start().await;
    let state_root = server.codex_home().parent().unwrap().join("state");
    let _codex_home = EnvironmentGuard::set("CODEX_HOME", server.codex_home());
    let _state_root = EnvironmentGuard::set("AGENTIC_COMPACT_STATE_DIR", &state_root);
    write_ready_capability(server.codex_home());

    let store = JournalStore::open().unwrap();
    store.save(&persisted_checkpoint_journal()).unwrap();

    let orchestrator = Orchestrator::new().unwrap();
    let recovering = tokio::spawn(async move { orchestrator.recover_nonterminal_journals().await });
    server.initialize_connection().await;

    let resume = request(&mut server, "thread/resume").await;
    respond(
        &server,
        &resume,
        json!({"thread":{
            "id":"thread",
            "status":{"type":"idle"},
            "turns":[
                {"id":"source","status":"completed","items":[]},
                {
                    "id":"compact",
                    "status":"completed",
                    "items":[{
                        "id":"compact-item",
                        "type":"contextCompaction"
                    }]
                }
            ]
        }}),
    )
    .await;
    let loaded = request(&mut server, "thread/loaded/list").await;
    respond(
        &server,
        &loaded,
        json!({"data":["thread"],"nextCursor":null}),
    )
    .await;

    let injection = request(&mut server, "thread/inject_items").await;
    let items = injection["params"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    let assistant_text = items[1]["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(assistant_text).unwrap(),
        json!({"preserve":["keep invariant"],"nextAction":"continue recovery"})
    );
    assert!(
        items[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("non-authoritative continuity state")
    );
    respond(&server, &injection, json!({})).await;
    let continuation = request(&mut server, "turn/start").await;
    assert_eq!(continuation["params"]["input"], json!([]));
    respond(
        &server,
        &continuation,
        json!({"turn":{"id":"continuation","status":"inProgress","items":[]}}),
    )
    .await;
    server
        .send(json!({
            "method":"turn/started",
            "params":{
                "threadId":"thread",
                "turn":{"id":"continuation","status":"inProgress","items":[]}
            }
        }))
        .await;
    let unsubscribe = request(&mut server, "thread/unsubscribe").await;
    respond(&server, &unsubscribe, json!({})).await;

    assert_eq!(recovering.await.unwrap().unwrap(), 1);
    let recovered = store.load("thread").unwrap().unwrap();
    assert_eq!(recovered.state, TransitionState::Cooldown);
    assert_eq!(
        recovered.continuation_turn_id.as_deref(),
        Some("continuation")
    );
}

async fn confirm_exact_continuation() {
    let mut server = FakeServer::start().await;
    let state_root = server.codex_home().parent().unwrap().join("state");
    let _codex_home = EnvironmentGuard::set("CODEX_HOME", server.codex_home());
    let _state_root = EnvironmentGuard::set("AGENTIC_COMPACT_STATE_DIR", &state_root);

    let store = JournalStore::open().unwrap();
    let mut journal = journal();
    journal.state = TransitionState::AwaitContinuationStarted;
    journal.continuation_turn_id = Some("continuation".to_owned());
    store.save(&journal).unwrap();

    let orchestrator = Orchestrator::new().unwrap();
    let recovering = tokio::spawn(async move { orchestrator.recover_nonterminal_journals().await });
    server.initialize_connection().await;
    let read = request(&mut server, "thread/read").await;
    respond(
        &server,
        &read,
        json!({"thread":{
            "id":"thread",
            "status":{"type":"idle"},
            "turns":[{"id":"continuation","status":"completed","items":[]}]
        }}),
    )
    .await;

    assert_eq!(recovering.await.unwrap().unwrap(), 1);
    assert_eq!(
        store.load("thread").unwrap().unwrap().state,
        TransitionState::Cooldown
    );
}

async fn user_turn_blocks_checkpoint_recovery() {
    let mut server = FakeServer::start().await;
    let state_root = server.codex_home().parent().unwrap().join("state");
    let _codex_home = EnvironmentGuard::set("CODEX_HOME", server.codex_home());
    let _state_root = EnvironmentGuard::set("AGENTIC_COMPACT_STATE_DIR", &state_root);
    write_ready_capability(server.codex_home());

    let store = JournalStore::open().unwrap();
    store.save(&persisted_checkpoint_journal()).unwrap();
    let orchestrator = Orchestrator::new().unwrap();
    let recovering = tokio::spawn(async move { orchestrator.recover_nonterminal_journals().await });
    server.initialize_connection().await;
    let resume = request(&mut server, "thread/resume").await;
    respond(
        &server,
        &resume,
        json!({"thread":{
            "id":"thread",
            "status":{"type":"idle"},
            "turns":[
                {"id":"source","status":"completed","items":[]},
                {
                    "id":"compact",
                    "status":"completed",
                    "items":[{
                        "id":"compact-item",
                        "type":"contextCompaction"
                    }]
                },
                {"id":"user","status":"completed","items":[]}
            ]
        }}),
    )
    .await;
    let unsubscribe = request(&mut server, "thread/unsubscribe").await;
    respond(&server, &unsubscribe, json!({})).await;

    assert_eq!(recovering.await.unwrap().unwrap(), 1);
    assert!(server.try_next_request().is_none());
    let recovered = store.load("thread").unwrap().unwrap();
    assert_eq!(recovered.state, TransitionState::Cancelled);
    assert_eq!(recovered.reason_code.as_deref(), Some("race_lost"));
}
