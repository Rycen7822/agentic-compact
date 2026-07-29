use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn mcp_stdout_contains_only_json_rpc_and_fails_closed_without_app_server() {
    let codex_home = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_agentic-compact"))
        .arg("mcp")
        .env("CODEX_HOME", codex_home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let requests = [
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2025-06-18"}
        }),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "status",
                "arguments": {},
                "_meta": {
                    "threadId": "thr_1",
                    "x-codex-turn-metadata": {
                        "thread_id": "thr_1",
                        "turn_id": "turn_1"
                    }
                }
            }
        }),
    ];
    {
        let stdin = child.stdin.as_mut().unwrap();
        for request in requests {
            writeln!(stdin, "{request}").unwrap();
        }
    }
    drop(child.stdin.take());

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let responses = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();

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
    assert!(
        responses[1]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| !tool["outputSchema"].is_null())
    );
    assert_eq!(
        responses[2]["result"]["structuredContent"]["reasonCode"],
        "shared_app_server_unavailable"
    );
}
