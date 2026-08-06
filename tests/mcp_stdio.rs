use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};

#[cfg(unix)]
mod support;

#[cfg(unix)]
use support::{FakeServer, write_ready_capability};

fn invocation_meta() -> Value {
    json!({
        "threadId": "thr_1",
        "x-codex-turn-metadata": {
            "thread_id": "thr_1",
            "turn_id": "turn_1"
        }
    })
}

fn run_mcp(input: &[u8]) -> Vec<Value> {
    let codex_home = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_agentic-compact"))
        .arg("mcp")
        .env("CODEX_HOME", codex_home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    child.stdin.as_mut().unwrap().write_all(input).unwrap();
    drop(child.stdin.take());

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect()
}

fn encode_requests(requests: &[Value]) -> Vec<u8> {
    let mut input = Vec::new();
    for request in requests {
        writeln!(input, "{request}").unwrap();
    }
    input
}

#[test]
fn mcp_advertises_one_tool_and_fails_closed_without_app_server() {
    let responses = run_mcp(&encode_requests(&[
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2025-06-18"}
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "request_compaction",
                "arguments": {"next_action": "continue the task"},
                "_meta": invocation_meta()
            }
        }),
    ]));

    assert_eq!(responses.len(), 3);
    assert!(
        responses
            .iter()
            .all(|response| response["jsonrpc"] == "2.0")
    );
    assert_eq!(
        responses[0]["result"]["serverInfo"]["name"],
        "agentic-compact"
    );
    assert_eq!(
        responses[0]["result"]["instructions"],
        "Call request_compaction only after a phase has conclusively ended, substantial work remains, and no command, test, approval, file change, or subagent is active. If scheduled, end the turn immediately; if rejected, continue without retrying in that turn."
    );

    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "request_compaction");
    assert_eq!(tools[0]["inputSchema"]["required"], json!(["next_action"]));

    let rejection = &responses[2]["result"];
    assert_eq!(
        rejection["structuredContent"]["reasonCode"],
        "shared_app_server_unavailable"
    );
    assert_eq!(rejection["structuredContent"]["retryable"], false);
    assert_eq!(rejection["isError"], false);
    assert!(rejection.get("_meta").is_none());
}

#[test]
fn mcp_front_door_errors_are_static_and_redacted() {
    let responses = run_mcp(&encode_requests(&[
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "unsupported",
            "params": {"privatePath": "/tmp/secret"}
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {}
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "private-tool-name",
                "arguments": {},
                "_meta": invocation_meta()
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "request_compaction",
                "arguments": {"preserve": []},
                "_meta": invocation_meta()
            }
        }),
    ]));

    assert_eq!(responses[0]["error"]["code"], -32602);
    assert_eq!(responses[0]["error"]["message"], "Unsupported MCP method.");
    assert_eq!(responses[1]["error"]["code"], -32602);
    assert_eq!(
        responses[1]["error"]["message"],
        "Invalid JSON-RPC request."
    );

    assert_eq!(
        responses[2]["result"]["structuredContent"]["message"],
        "Unknown MCP tool."
    );
    assert_eq!(responses[2]["result"]["isError"], true);
    assert_eq!(
        responses[3]["result"]["structuredContent"]["message"],
        "The compaction request is invalid."
    );
    assert_eq!(responses[3]["result"]["isError"], true);

    let output = serde_json::to_string(&responses).unwrap();
    assert!(!output.contains("/tmp/secret"));
    assert!(!output.contains("private-tool-name"));
    assert!(!output.contains("component"));
}

#[test]
fn malformed_and_oversized_messages_use_fixed_json_rpc_errors() {
    let malformed = run_mcp(b"{not-json\n");
    assert_eq!(malformed[0]["error"]["code"], -32700);
    assert_eq!(
        malformed[0]["error"]["message"],
        "Invalid JSON-RPC request."
    );

    let mut oversized = vec![b'x'; 1024 * 1024 + 1];
    oversized.push(b'\n');
    let oversized = run_mcp(&oversized);
    assert_eq!(oversized[0]["error"]["code"], -32600);
    assert_eq!(
        oversized[0]["error"]["message"],
        "MCP message exceeds the configured size limit."
    );
}

#[cfg(unix)]
#[tokio::test]
async fn scheduled_call_returns_once_with_empty_content_and_meta_receipt() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::time::{Duration, timeout};

    let mut server = FakeServer::start().await;
    let state_root = server.codex_home().parent().unwrap().join("state");
    write_ready_capability(server.codex_home());

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_agentic-compact"))
        .arg("mcp")
        .env("CODEX_HOME", server.codex_home())
        .env("AGENTIC_COMPACT_STATE_DIR", &state_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let requests = encode_requests(&[
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2025-06-18"}
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "request_compaction",
                "arguments": {"next_action": "continue the task"},
                "_meta": invocation_meta()
            }
        }),
    ]);
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(&requests).await.unwrap();

    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let initialized: Value =
        serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(initialized["id"], 1);

    server.initialize_connection().await;
    let resume = server.next_request().await;
    assert_eq!(resume["method"], "thread/resume");
    server
        .send(json!({
            "id": resume["id"],
            "result": {
                "thread": {
                    "id": "thr_1",
                    "status": {"type": "active"},
                    "turns": [{"id": "turn_1", "status": "inProgress", "items": []}]
                }
            }
        }))
        .await;
    let loaded = server.next_request().await;
    assert_eq!(loaded["method"], "thread/loaded/list");
    server
        .send(json!({
            "id": loaded["id"],
            "result": {"data": ["thr_1"], "nextCursor": null}
        }))
        .await;

    let scheduled: Value =
        serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    let result = &scheduled["result"];
    assert_eq!(result["content"], json!([]));
    assert_eq!(
        result["structuredContent"],
        json!({"status": "scheduled_after_turn"})
    );
    assert_eq!(result["isError"], false);
    assert!(
        result["_meta"]["agenticCompact"]["receiptId"]
            .as_str()
            .unwrap()
            .starts_with("rcpt_")
    );
    assert!(!result.to_string().contains("checkpointId"));

    drop(stdin);
    let extra = timeout(Duration::from_secs(2), lines.next_line())
        .await
        .unwrap()
        .unwrap();
    assert!(extra.is_none());
    assert!(child.wait().await.unwrap().success());
}
