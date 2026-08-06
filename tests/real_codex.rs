use agentic_compact::app_server::AppServerClient;
use agentic_compact::error::ErrorCode;
use agentic_compact::journal::TransitionState;
use agentic_compact::protocol::{AppEvent, ResumeSnapshot};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;
use uuid::Uuid;

#[path = "support/real_app_server.rs"]
mod real_app_server;

use real_app_server::{
    RawAppServerClient, assert_process_alive, production_mcp_config, start_frozen_codex,
    toml_string, wait_for_mcp_pid, wait_for_terminal_journal, write_ready_capability,
};

#[tokio::test]
#[ignore = "requires frozen Codex and kills an isolated unauthenticated app-server"]
async fn killed_real_app_server_fails_closed_without_a_retry() {
    let (mut server, socket) = start_frozen_codex(false, |_| String::new()).await;
    let client = AppServerClient::connect(&socket).await.unwrap();
    let mut events = client.subscribe();
    let pid = server.child.id().unwrap().to_string();
    let status = Command::new("kill")
        .args(["-TERM", &pid])
        .status()
        .await
        .unwrap();
    assert!(status.success());
    server.child.wait().await.unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                events.recv().await.unwrap(),
                AppEvent::ConnectionClosed { .. }
            ) {
                return;
            }
        }
    })
    .await
    .expect("client did not observe app-server termination");

    let error = client
        .thread_read("00000000-0000-0000-0000-000000000000", false)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::SharedAppServerUnavailable, "{error}");
}

#[tokio::test]
#[ignore = "requires authenticated frozen Codex and starts a real empty turn"]
async fn resumes_same_thread_100_times_without_mutating_settings() {
    let (server, socket) = start_frozen_codex(true, |_| String::new()).await;
    let owner = AppServerClient::connect(&socket).await.unwrap();
    let started = owner.start_thread(false).await.unwrap();
    assert_eq!(started.model.as_deref(), Some("gpt-5.6-luna"));
    assert_eq!(started.reasoning_effort.as_deref(), Some("high"));
    let mut events = owner.subscribe();
    let turn_id = owner.start_empty_turn(&started.thread.id).await.unwrap();
    wait_for_turn(&mut events, &started.thread.id, &turn_id).await;
    let baseline = owner.thread_resume(&started.thread.id).await.unwrap();

    for attempt in 1..=100 {
        let client = timeout(Duration::from_secs(10), AppServerClient::connect(&socket))
            .await
            .unwrap_or_else(|_| panic!("attach {attempt} timed out"))
            .unwrap();
        let resumed = timeout(
            Duration::from_secs(10),
            client.thread_resume(&baseline.thread.id),
        )
        .await
        .unwrap_or_else(|_| panic!("resume {attempt} timed out"))
        .unwrap();

        assert_snapshot_unchanged(&baseline, &resumed, attempt);
        client.unsubscribe(&baseline.thread.id).await.unwrap();
        client.close().await;
    }

    owner.unsubscribe(&baseline.thread.id).await.unwrap();
    owner.delete_thread(&baseline.thread.id).await.unwrap();
    owner.close().await;
    drop(server);
}

#[tokio::test]
#[ignore = "requires frozen Codex and exercises its direct MCP bridge"]
async fn direct_mcp_metadata_contract_for_frozen_codex() {
    let receipt = format!("rcpt_{}", Uuid::new_v4().simple());
    let expected_receipt = receipt.clone();
    let (server, socket) = start_frozen_codex(false, move |home| {
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/support/phase0a_mcp_probe.py");
        let log = home.join("phase8-direct-mcp.jsonl");
        format!(
            "\n[mcp_servers.phase0a_probe]\ncommand = \"python3\"\nargs = [{}]\nstartup_timeout_sec = 10\ntool_timeout_sec = 30\ndefault_tools_approval_mode = \"approve\"\n\n[mcp_servers.phase0a_probe.env]\nPHASE0A_PROBE_LOG = {}\nPHASE0A_RECEIPT = {}\n",
            toml_string(&script),
            toml_string(&log),
            serde_json::to_string(&expected_receipt).unwrap(),
        )
    })
    .await;
    let owner = AppServerClient::connect(&socket).await.unwrap();
    let thread = owner.start_thread(false).await.unwrap().thread.id;
    let mut raw = RawAppServerClient::connect(&socket).await;
    let result = raw
        .request(
            "mcpServer/tool/call",
            json!({
                "threadId": thread,
                "server": "phase0a_probe",
                "tool": "request_compaction",
                "arguments": {"preserve": [], "next_action": "continue the direct probe"}
            }),
        )
        .await;

    assert_eq!(result["content"], json!([]));
    assert_eq!(
        result["structuredContent"],
        json!({"status": "scheduled_after_turn"})
    );
    assert_eq!(result["_meta"]["agenticCompact"]["receiptId"], receipt);
    owner.delete_thread(&thread).await.unwrap();
    owner.close().await;
    drop(server);
}

#[tokio::test]
#[ignore = "requires authenticated frozen Codex and performs a production transition"]
async fn completes_v02_transition_with_original_mcp_process() {
    let (server, socket) = start_frozen_codex(true, production_mcp_config).await;
    let state_root = server.home.path().join("state");
    let app_server_pid = server.child.id().unwrap();
    let owner = AppServerClient::connect(&socket).await.unwrap();
    let started = owner.start_thread(false).await.unwrap();
    assert_eq!(started.model.as_deref(), Some("gpt-5.6-luna"));
    assert_eq!(started.reasoning_effort.as_deref(), Some("high"));
    write_ready_capability(server.home.path(), &owner);
    let thread_id = started.thread.id;
    let mut events = owner.subscribe();
    let mut raw = RawAppServerClient::connect(&socket).await;
    let mcp_status = raw
        .request(
            "mcpServerStatus/list",
            json!({"threadId": thread_id, "detail": "full"}),
        )
        .await;
    assert_production_mcp_loaded(&mcp_status);
    let original_pid = wait_for_mcp_pid(app_server_pid).await;
    assert_process_alive(original_pid).await;
    let source = raw
        .request(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [{
                    "type": "text",
                    "text": "This is an explicit Agentic Compact acceptance test at a settled phase boundary. Your only action in this turn is to call the request_compaction MCP tool exactly once with preserve set to [\"Phase 8 production transition\"] and next_action set to \"Reply exactly PHASE8_CONTINUED without calling a tool.\" Do not answer in prose. After the tool reports scheduled, end this turn immediately."
                }]
            }),
        )
        .await;
    let source_turn_id = source["turn"]["id"].as_str().unwrap().to_owned();
    wait_for_turn(&mut events, &thread_id, &source_turn_id).await;
    let source_snapshot = raw
        .request(
            "thread/read",
            json!({"threadId": thread_id, "includeTurns": true}),
        )
        .await;
    assert_source_requested_compaction(&source_snapshot, &source_turn_id);
    assert_process_alive(original_pid).await;

    let journal = wait_for_terminal_journal(&state_root).await;
    assert_eq!(journal.state, TransitionState::Cooldown, "{journal:?}");
    assert_eq!(journal.thread_id, thread_id);
    assert_eq!(journal.source_turn_id, source_turn_id);
    let compact_turn_id = journal.compact_turn_id.as_deref().unwrap();
    let continuation_turn_id = journal.continuation_turn_id.as_deref().unwrap();
    wait_for_turn(&mut events, &thread_id, continuation_turn_id).await;
    assert_eq!(wait_for_mcp_pid(app_server_pid).await, original_pid);
    assert_process_alive(original_pid).await;

    let snapshot = raw
        .request(
            "thread/read",
            json!({"threadId": thread_id, "includeTurns": true}),
        )
        .await;
    assert_real_transition_snapshot(
        &snapshot,
        &source_turn_id,
        compact_turn_id,
        continuation_turn_id,
        &journal.receipt_id,
    );
    owner.unsubscribe(&thread_id).await.unwrap();
    owner.delete_thread(&thread_id).await.unwrap();
    owner.close().await;
    drop(server);
}

fn assert_production_mcp_loaded(response: &Value) {
    let servers = response["data"].as_array().unwrap();
    let server = servers
        .iter()
        .find(|server| server["name"] == "agentic-compact")
        .unwrap_or_else(|| panic!("production MCP is absent from status: {response}"));
    assert!(
        server["tools"].get("request_compaction").is_some(),
        "production tool is absent from status: {server}"
    );
}

fn assert_source_requested_compaction(response: &Value, source_id: &str) {
    let thread = response.get("thread").unwrap_or(response);
    let source = thread["turns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|turn| turn["id"] == source_id)
        .unwrap();
    let items = source["items"].as_array().unwrap();
    let calls = items
        .iter()
        .filter(|item| {
            item["type"] == "mcpToolCall"
                && item["server"] == "agentic-compact"
                && item["tool"] == "request_compaction"
        })
        .collect::<Vec<_>>();
    let item_types = items
        .iter()
        .map(|item| item["type"].as_str().unwrap_or("unknown"))
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1, "source item types: {item_types:?}");
    assert_eq!(
        calls[0]["result"]["structuredContent"]["status"], "scheduled_after_turn",
        "production MCP did not schedule: {}",
        calls[0]
    );
}

fn assert_snapshot_unchanged(baseline: &ResumeSnapshot, resumed: &ResumeSnapshot, attempt: usize) {
    assert_eq!(resumed.thread.id, baseline.thread.id, "attach {attempt}");
    assert_eq!(resumed.model, baseline.model, "model at attach {attempt}");
    assert_eq!(
        resumed.reasoning_effort, baseline.reasoning_effort,
        "reasoning effort at attach {attempt}"
    );
    assert_eq!(resumed.cwd, baseline.cwd, "cwd at attach {attempt}");
    assert_eq!(
        resumed.approval_policy, baseline.approval_policy,
        "approval policy at attach {attempt}"
    );
    assert_eq!(
        resumed.sandbox, baseline.sandbox,
        "sandbox at attach {attempt}"
    );
    assert_eq!(
        turn_signature(resumed),
        turn_signature(baseline),
        "turn snapshot at attach {attempt}"
    );
}

fn turn_signature(snapshot: &ResumeSnapshot) -> Vec<(&str, &str)> {
    snapshot
        .thread
        .turns
        .iter()
        .map(|turn| (turn.id.as_str(), turn.status.as_str()))
        .collect()
}

async fn wait_for_turn(
    events: &mut tokio::sync::broadcast::Receiver<AppEvent>,
    thread_id: &str,
    turn_id: &str,
) {
    timeout(Duration::from_secs(60), async {
        loop {
            if let AppEvent::TurnCompleted {
                thread_id: completed_thread,
                turn,
            } = events.recv().await.unwrap()
            {
                if completed_thread == thread_id && turn.id == turn_id {
                    assert_eq!(turn.status, "completed");
                    return;
                }
            }
        }
    })
    .await
    .expect("empty persistence turn did not complete");
}

fn assert_real_transition_snapshot(
    response: &Value,
    source_id: &str,
    compact_id: &str,
    continuation_id: &str,
    receipt_id: &str,
) {
    let thread = response.get("thread").unwrap_or(response);
    let turns = thread["turns"].as_array().unwrap();
    assert_eq!(
        turns
            .iter()
            .map(|turn| turn["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [source_id, compact_id, continuation_id]
    );
    assert!(turns.iter().all(|turn| turn["status"] == "completed"));

    let source = &turns[0];
    let calls = source["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| {
            item["type"] == "mcpToolCall"
                && item["server"] == "agentic-compact"
                && item["tool"] == "request_compaction"
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["result"]["content"], json!([]));
    assert_eq!(
        calls[0]["result"]["structuredContent"],
        json!({"status": "scheduled_after_turn"})
    );
    assert_eq!(
        calls[0]["result"]["_meta"]["agenticCompact"]["receiptId"],
        receipt_id
    );
    assert!(
        calls[0]["result"].get("checkpointId").is_none(),
        "checkpoint ID leaked into the tool result"
    );

    let compact_items = turns[1]["items"].as_array().unwrap();
    assert_eq!(compact_items.len(), 1);
    assert_eq!(compact_items[0]["type"], "contextCompaction");
    let all_items = turns
        .iter()
        .flat_map(|turn| turn["items"].as_array().unwrap());
    assert_eq!(
        all_items
            .clone()
            .filter(|item| item["type"] == "contextCompaction")
            .count(),
        1
    );
    assert_eq!(
        all_items
            .clone()
            .filter(|item| item["type"] == "userMessage")
            .count(),
        1
    );
    let continuation_text = turns[2]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "agentMessage")
        .and_then(|item| item["text"].as_str())
        .unwrap();
    assert_eq!(continuation_text.trim(), "PHASE8_CONTINUED");
    assert!(
        !response
            .to_string()
            .contains("A host-controlled agentic-compact transition has completed."),
        "injected continuity wrapper leaked into the stable thread snapshot"
    );
}
