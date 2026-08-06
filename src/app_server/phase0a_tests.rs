use super::*;
use crate::protocol::{AppEvent, TurnRef};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tempfile::TempDir;
use tokio::process::{Child, Command};
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};
use uuid::Uuid;

struct RealServer {
    _child: Child,
    _home: TempDir,
}

#[tokio::test]
#[ignore = "requires authenticated frozen Codex and performs real model turns"]
async fn phase0a_direct_mcp_contract_and_native_compaction_process_survival() {
    let receipt = format!("rcpt_{}", Uuid::new_v4().simple());
    let (server, socket, log_path) = start_app_server(&receipt).await;
    let client = AppServerClient::connect(&socket).await.unwrap();
    let mut events = client.subscribe();
    let started = client.start_thread(false).await.unwrap();
    assert_eq!(started.model.as_deref(), Some("gpt-5.6-luna"));
    assert_eq!(started.reasoning_effort.as_deref(), Some("high"));
    let thread_id = started.thread.id;
    wait_for_probe_initialize(&log_path).await;

    let rejected = call_probe(
        &client,
        &thread_id,
        json!({"preserve": ["force rejection"], "next_action": "continue normally"}),
    )
    .await;
    assert_eq!(
        rejected["structuredContent"],
        json!({
            "status": "rejected",
            "reasonCode": "shared_app_server_unavailable",
            "message": "The shared Codex app-server is unavailable; continue without compaction.",
            "retryable": false
        })
    );
    let probe_pid = single_probe_pid(&log_path, 1);

    let scheduled = call_probe(
        &client,
        &thread_id,
        json!({"preserve": [], "next_action": "continue the characterization"}),
    )
    .await;
    assert_eq!(scheduled["content"], json!([]));
    assert_eq!(
        scheduled["structuredContent"],
        json!({"status": "scheduled_after_turn"})
    );
    assert_eq!(scheduled["_meta"]["agenticCompact"]["receiptId"], receipt);

    let seed_turn = client.start_empty_turn(&thread_id).await.unwrap();
    wait_for_turn(&mut events, &seed_turn).await;
    let read = raw_read(&client, &thread_id).await;
    let resume = client
        .read_request("thread/resume", json!({"threadId": thread_id}), true)
        .await
        .unwrap();
    assert_snapshot_arrays(&read);
    assert_snapshot_arrays(&resume);
    assert_eq!(turn_signature(&read), turn_signature(&resume));
    assert_chronological(&read, &[&seed_turn]);

    assert_eq!(single_probe_pid(&log_path, 2), probe_pid);
    assert_process_alive(probe_pid).await;

    client.compact_start(&thread_id).await.unwrap();
    let compact_turn = wait_for_next_completed_turn(&mut events, &thread_id).await;
    assert!(compact_turn.items.is_empty());
    let after_compact = raw_read(&client, &thread_id).await;
    let compact = unique_turn(&after_compact, &compact_turn.id);
    let compact_items = compact["items"].as_array().unwrap();
    assert_eq!(compact_items.len(), 1);
    assert_eq!(compact_items[0]["type"], "contextCompaction");
    assert!(compact_items[0].get("status").is_none());
    assert_eq!(single_probe_pid(&log_path, 2), probe_pid);
    assert_process_alive(probe_pid).await;

    let post_compact = call_probe(
        &client,
        &thread_id,
        json!({"preserve": ["force rejection"], "next_action": "finish characterization"}),
    )
    .await;
    assert_eq!(post_compact["structuredContent"]["status"], "rejected");
    let final_snapshot = raw_read(&client, &thread_id).await;
    assert_snapshot_arrays(&final_snapshot);
    assert_chronological(&final_snapshot, &[&seed_turn, &compact_turn.id]);
    assert_eq!(single_probe_pid(&log_path, 3), probe_pid);
    assert_process_alive(probe_pid).await;
    client.unsubscribe(&thread_id).await.unwrap();
    client.delete_thread(&thread_id).await.unwrap();
    client.close().await;
    drop(server);
}

async fn call_probe(client: &AppServerClient, thread_id: &str, arguments: Value) -> Value {
    client
        .read_request(
            "mcpServer/tool/call",
            json!({
                "threadId": thread_id,
                "server": "phase0a_probe",
                "tool": "request_compaction",
                "arguments": arguments
            }),
            false,
        )
        .await
        .unwrap()
}

async fn raw_read(client: &AppServerClient, thread_id: &str) -> Value {
    client
        .read_request(
            "thread/read",
            json!({"threadId": thread_id, "includeTurns": true}),
            true,
        )
        .await
        .unwrap()
}

async fn wait_for_turn(events: &mut broadcast::Receiver<AppEvent>, turn_id: &str) -> TurnRef {
    timeout(Duration::from_secs(180), async {
        loop {
            if let AppEvent::TurnCompleted { turn, .. } = events.recv().await.unwrap() {
                if turn.id == turn_id {
                    assert_eq!(turn.status, "completed");
                    return turn;
                }
            }
        }
    })
    .await
    .expect("real Codex turn did not complete")
}

async fn wait_for_next_completed_turn(
    events: &mut broadcast::Receiver<AppEvent>,
    thread_id: &str,
) -> TurnRef {
    timeout(Duration::from_secs(180), async {
        loop {
            if let AppEvent::TurnCompleted {
                thread_id: completed_thread,
                turn,
            } = events.recv().await.unwrap()
            {
                if completed_thread == thread_id {
                    assert_eq!(turn.status, "completed");
                    return turn;
                }
            }
        }
    })
    .await
    .expect("Codex compaction turn did not complete")
}

async fn assert_process_alive(pid: u32) {
    assert!(
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .await
            .unwrap()
            .success()
    );
}

async fn wait_for_probe_initialize(path: &Path) {
    timeout(Duration::from_secs(30), async {
        loop {
            if std::fs::read_to_string(path)
                .is_ok_and(|records| records.contains("\"event\": \"initialize\""))
            {
                return;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("Codex did not initialize the direct MCP probe");
}

fn snapshot_turns(snapshot: &Value) -> &[Value] {
    snapshot["thread"]["turns"]
        .as_array()
        .expect("snapshot must contain a turns array")
}

fn unique_turn<'a>(snapshot: &'a Value, turn_id: &str) -> &'a Value {
    let mut matches = snapshot_turns(snapshot)
        .iter()
        .filter(|turn| turn["id"] == turn_id);
    let turn = matches
        .next()
        .expect("snapshot is missing an expected turn");
    assert!(
        matches.next().is_none(),
        "snapshot contains a duplicate turn ID"
    );
    turn
}

fn assert_snapshot_arrays(snapshot: &Value) {
    for turn in snapshot_turns(snapshot) {
        assert!(turn["items"].is_array(), "turn.items must be an array");
        assert_eq!(turn["itemsView"], "full");
    }
}

fn turn_signature(snapshot: &Value) -> Vec<(String, Vec<String>)> {
    snapshot_turns(snapshot)
        .iter()
        .map(|turn| {
            let items = turn["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["type"].as_str().unwrap().to_owned())
                .collect();
            (turn["id"].as_str().unwrap().to_owned(), items)
        })
        .collect()
}

fn assert_chronological(snapshot: &Value, turn_ids: &[&str]) {
    let turns = snapshot_turns(snapshot);
    let positions = turn_ids
        .iter()
        .map(|turn_id| {
            let matches = turns
                .iter()
                .enumerate()
                .filter(|(_, turn)| turn["id"] == *turn_id)
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            assert_eq!(matches.len(), 1, "turn ID must be unique");
            matches[0]
        })
        .collect::<Vec<_>>();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
}

fn single_probe_pid(path: &Path, expected_calls: usize) -> u32 {
    let records = std::fs::read_to_string(path).unwrap();
    let values = records
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        values
            .iter()
            .filter(|value| value["event"] == "call")
            .count(),
        expected_calls
    );
    let mut pids = values
        .iter()
        .map(|value| value["pid"].as_u64().unwrap() as u32)
        .collect::<Vec<_>>();
    pids.sort_unstable();
    pids.dedup();
    assert_eq!(pids.len(), 1, "probe process changed: {records}");
    pids[0]
}

async fn start_app_server(receipt: &str) -> (RealServer, PathBuf, PathBuf) {
    let home = tempfile::Builder::new()
        .prefix("phase0a-codex-home-")
        .tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("target"))
        .unwrap();
    copy_auth(home.path());
    let log_path = home.path().join("phase0a-probe.jsonl");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/phase0a_mcp_probe.py");
    let config = format!(
        "model = \"gpt-5.6-luna\"\nmodel_reasoning_effort = \"high\"\n\n[features]\ncode_mode = false\ncode_mode_only = false\n\n[mcp_servers.phase0a_probe]\ncommand = \"python3\"\nargs = [{}]\nstartup_timeout_sec = 10\ntool_timeout_sec = 30\ndefault_tools_approval_mode = \"approve\"\n\n[mcp_servers.phase0a_probe.env]\nPHASE0A_PROBE_LOG = {}\nPHASE0A_RECEIPT = {}\n",
        serde_json::to_string(&script.display().to_string()).unwrap(),
        serde_json::to_string(&log_path.display().to_string()).unwrap(),
        serde_json::to_string(receipt).unwrap(),
    );
    std::fs::write(home.path().join("config.toml"), config).unwrap();

    let socket = home.path().join("phase0a-real.sock");
    let codex = std::env::var_os("AGENTIC_COMPACT_CODEX_BIN").unwrap_or_else(|| "codex".into());
    let version = Command::new(&codex)
        .arg("--version")
        .output()
        .await
        .unwrap();
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8(version.stdout).unwrap().trim(),
        "codex-cli 0.146.0"
    );
    let mut child = Command::new(codex)
        .args([
            "app-server",
            "--listen",
            &format!("unix://{}", socket.display()),
        ])
        .env("CODEX_HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if socket.exists() {
            return (
                RealServer {
                    _child: child,
                    _home: home,
                },
                socket,
                log_path,
            );
        }
        assert!(child.try_wait().unwrap().is_none());
        sleep(Duration::from_millis(50)).await;
    }
    panic!("Codex app-server did not create its socket within five seconds");
}

fn copy_auth(target: &Path) {
    let source = std::env::var_os("AGENTIC_COMPACT_REAL_CODEX_HOME")
        .or_else(|| std::env::var_os("CODEX_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .expect("set AGENTIC_COMPACT_REAL_CODEX_HOME to an authenticated Codex home");
    let auth = source.join("auth.json");
    assert!(auth.is_file(), "selected Codex home has no auth.json");
    std::fs::copy(auth, target.join("auth.json")).unwrap();
}
