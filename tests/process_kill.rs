#![cfg(unix)]

use agentic_compact::journal::{JournalStore, TransitionState};
use agentic_compact::lease::ThreadLease;
use serde_json::{Value, json};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdout, Command};
use tokio::time::timeout;

mod support;

use support::{EnvironmentGuard, TransitionServer, write_ready_capability};

#[tokio::test]
async fn killed_mcp_restarts_without_replaying_a_pre_compaction_transition() {
    let server = TransitionServer::start().await;
    let state_root = server.codex_home().parent().unwrap().join("state");
    let _codex_home = EnvironmentGuard::set("CODEX_HOME", server.codex_home());
    let _state_root = EnvironmentGuard::set("AGENTIC_COMPACT_STATE_DIR", &state_root);
    write_ready_capability(server.codex_home());
    let source_id = server.prepare_transition("mcp-kill", 0);

    let mut child = command(server.codex_home(), &state_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2025-06-18"}
        }),
    )
    .await;
    assert_eq!(
        response(&mut stdout, 1).await["result"]["serverInfo"]["name"],
        "agentic-compact"
    );
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "request_compaction",
                "arguments": {
                    "preserve": ["do not replay an ambiguous mutation"],
                    "next_action": "resume only from a proven state"
                },
                "_meta": {
                    "threadId": "mcp-kill",
                    "x-codex-turn-metadata": {
                        "thread_id": "mcp-kill",
                        "turn_id": source_id
                    }
                }
            }
        }),
    )
    .await;
    let scheduled = response(&mut stdout, 2).await;
    assert_eq!(
        scheduled["result"]["structuredContent"]["status"], "scheduled_after_turn",
        "{scheduled}"
    );

    child.kill().await.unwrap();
    child.wait().await.unwrap();
    drop(stdin);
    server.wait_for_no_connections().await;

    let restarted = command(server.codex_home(), &state_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .output()
        .await
        .unwrap();
    assert!(restarted.status.success());
    assert!(restarted.stdout.is_empty());
    let journal = JournalStore::open()
        .unwrap()
        .load("mcp-kill")
        .unwrap()
        .unwrap();
    assert_eq!(journal.state, TransitionState::Cancelled);
    assert_eq!(
        journal.reason_code.as_deref(),
        Some("process_restarted_before_compaction")
    );
    drop(ThreadLease::acquire("mcp-kill").unwrap());
    server.shutdown().await;
}

fn command(codex_home: &std::path::Path, state_root: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agentic-compact"));
    command
        .arg("mcp")
        .env("CODEX_HOME", codex_home)
        .env("AGENTIC_COMPACT_STATE_DIR", state_root)
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}

async fn send(stdin: &mut tokio::process::ChildStdin, value: Value) {
    stdin.write_all(value.to_string().as_bytes()).await.unwrap();
    stdin.write_all(b"\n").await.unwrap();
    stdin.flush().await.unwrap();
}

async fn response(stdout: &mut Lines<BufReader<ChildStdout>>, id: i64) -> Value {
    timeout(Duration::from_secs(5), async {
        loop {
            let line = stdout.next_line().await.unwrap().unwrap();
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["id"] == id {
                return value;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("MCP response {id} timed out"))
}
