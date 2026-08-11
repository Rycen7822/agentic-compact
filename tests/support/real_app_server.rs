use agentic_compact::app_server::AppServerClient;
use agentic_compact::doctor::CapabilityRecord;
use agentic_compact::journal::{TransitionJournal, TransitionState};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;
use tokio::time::timeout;
use tokio_tungstenite::client_async_with_config;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_WEBSOCKET_BYTES: usize = 128 * 1024 * 1024;

pub(crate) struct FrozenCodexServer {
    pub(crate) child: Child,
    pub(crate) home: TempDir,
}

pub(crate) async fn start_frozen_codex(
    authenticated: bool,
    configure: impl FnOnce(&Path) -> String,
) -> (FrozenCodexServer, PathBuf) {
    let home = tempfile::tempdir().unwrap();
    if authenticated {
        copy_auth(home.path());
    }
    let config = format!(
        "model = \"gpt-5.6-luna\"\nmodel_reasoning_effort = \"high\"\nservice_tier = \"priority\"\n\n[features]\ncode_mode = false\ncode_mode_only = false\n{}",
        configure(home.path())
    );
    std::fs::write(home.path().join("config.toml"), config).unwrap();
    let socket = home
        .path()
        .join("app-server-control")
        .join("app-server-control.sock");
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let codex = std::env::var_os("AGENTIC_COMPACT_CODEX_BIN").unwrap_or_else(|| "codex".into());
    let version = Command::new(&codex)
        .arg("--version")
        .output()
        .await
        .unwrap();
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8(version.stdout).unwrap().trim(),
        "codex-cli 0.147.0"
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
        .expect("start the frozen Codex app-server");

    for _ in 0..100 {
        if socket.exists() {
            return (FrozenCodexServer { child, home }, socket);
        }
        if child.try_wait().unwrap().is_some() {
            panic!("Codex app-server exited before creating its socket");
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("Codex app-server did not create its socket within five seconds");
}

pub(crate) fn production_mcp_config(home: &Path) -> String {
    let state_root = home.join("state");
    let binary = Path::new(env!("CARGO_BIN_EXE_agentic-compact"));
    format!(
        "\n[mcp_servers.agentic-compact]\ncommand = {}\nargs = [\"mcp\"]\nstartup_timeout_sec = 10\ntool_timeout_sec = 30\ndefault_tools_approval_mode = \"approve\"\n\n[mcp_servers.agentic-compact.env]\nCODEX_HOME = {}\nAGENTIC_COMPACT_STATE_DIR = {}\n",
        toml_string(binary),
        toml_string(home),
        toml_string(&state_root),
    )
}

pub(crate) fn write_ready_capability(home: &Path, client: &AppServerClient) {
    let directory = home.join("agentic-compact");
    std::fs::create_dir_all(&directory).unwrap();
    let record = CapabilityRecord {
        schema_version: 1,
        plugin_version: env!("CARGO_PKG_VERSION").to_owned(),
        codex_user_agent: client.initialize_result.user_agent.clone(),
        platform_family: client.initialize_result.platform_family.clone(),
        platform_os: client.initialize_result.platform_os.clone(),
        empty_continuation: true,
        reentrant_attach_acknowledged: true,
        hidden_checkpoint_acknowledged: true,
        checked_at_ms: 1,
    };
    std::fs::write(
        directory.join("capabilities.json"),
        serde_json::to_vec_pretty(&record).unwrap(),
    )
    .unwrap();
}

pub(crate) async fn wait_for_terminal_journal(state_root: &Path) -> TransitionJournal {
    timeout(Duration::from_secs(240), async {
        loop {
            let directory = state_root.join("journals");
            let mut paths = std::fs::read_dir(&directory)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
                .collect::<Vec<_>>();
            paths.sort();
            assert!(paths.len() <= 1, "unexpected journals: {paths:?}");
            if let Some(path) = paths.first() {
                let journal: TransitionJournal =
                    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
                if matches!(
                    journal.state,
                    TransitionState::Cooldown
                        | TransitionState::Cancelled
                        | TransitionState::FailedSafe
                ) {
                    return journal;
                }
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("production transition did not reach a terminal journal state")
}

pub(crate) async fn wait_for_mcp_pid(app_server_pid: u32) -> u32 {
    timeout(Duration::from_secs(30), async {
        loop {
            let pids = mcp_descendants(app_server_pid).await;
            if pids.len() == 1 {
                return pids[0];
            }
            assert!(
                pids.is_empty(),
                "multiple production MCP processes: {pids:?}"
            );
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("production MCP process did not appear")
}

pub(crate) async fn assert_process_alive(pid: u32) {
    let status = Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .await
        .unwrap();
    assert!(status.success(), "MCP process {pid} is not alive");
}

async fn mcp_descendants(app_server_pid: u32) -> Vec<u32> {
    let output = Command::new("ps")
        .args(["-eo", "pid=,ppid=,comm="])
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let processes = String::from_utf8(output.stdout).unwrap();
    let rows = processes
        .lines()
        .filter_map(parse_process_row)
        .collect::<Vec<_>>();
    let mut descendants = vec![app_server_pid];
    let mut changed = true;
    while changed {
        changed = false;
        for (pid, parent, _) in &rows {
            if descendants.contains(parent) && !descendants.contains(pid) {
                descendants.push(*pid);
                changed = true;
            }
        }
    }
    rows.into_iter()
        .filter(|(pid, _, command)| {
            descendants.contains(pid)
                && Path::new(command)
                    .file_name()
                    .is_some_and(|binary| binary == "agentic-compact")
        })
        .map(|(pid, _, _)| pid)
        .collect()
}

fn parse_process_row(line: &str) -> Option<(u32, u32, &str)> {
    let line = line.trim_start();
    let pid_end = line.find(char::is_whitespace)?;
    let pid = line[..pid_end].parse().ok()?;
    let rest = line[pid_end..].trim_start();
    let parent_end = rest.find(char::is_whitespace)?;
    let parent = rest[..parent_end].parse().ok()?;
    Some((pid, parent, rest[parent_end..].trim_start()))
}

pub(crate) fn toml_string(path: &Path) -> String {
    serde_json::to_string(&path.display().to_string()).unwrap()
}

fn copy_auth(target: &Path) {
    let source = std::env::var_os("AGENTIC_COMPACT_REAL_CODEX_HOME")
        .or_else(|| std::env::var_os("CODEX_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .expect("set AGENTIC_COMPACT_REAL_CODEX_HOME to an authenticated Codex home");
    let auth = source.join("auth.json");
    assert!(
        auth.is_file(),
        "real Codex test requires auth.json in the selected Codex home"
    );
    std::fs::copy(auth, target.join("auth.json")).unwrap();
}

pub(crate) struct RawAppServerClient {
    socket: tokio_tungstenite::WebSocketStream<UnixStream>,
    next_id: u64,
}

impl RawAppServerClient {
    pub(crate) async fn connect(path: &Path) -> Self {
        let stream = timeout(REQUEST_TIMEOUT, UnixStream::connect(path))
            .await
            .expect("app-server socket connection timed out")
            .expect("connect to app-server socket");
        let request = "ws://localhost/rpc".into_client_request().unwrap();
        let config = WebSocketConfig::default()
            .max_frame_size(Some(MAX_WEBSOCKET_BYTES))
            .max_message_size(Some(MAX_WEBSOCKET_BYTES));
        let socket = timeout(
            REQUEST_TIMEOUT,
            client_async_with_config(request, stream, Some(config)),
        )
        .await
        .expect("app-server WebSocket handshake timed out")
        .expect("upgrade app-server WebSocket")
        .0;
        let mut client = Self { socket, next_id: 1 };
        client
            .request(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "agentic_compact_phase8_test",
                        "title": "Agentic Compact Phase 8 Test",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "capabilities": {
                        "experimentalApi": false,
                        "requestAttestation": false
                    }
                }),
            )
            .await;
        client.notify("initialized", json!({})).await;
        client
    }

    pub(crate) async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"id": id, "method": method, "params": params}))
            .await;
        timeout(REQUEST_TIMEOUT, async {
            loop {
                match self.socket.next().await {
                    Some(Ok(Message::Text(text))) => {
                        let message: Value = serde_json::from_str(text.as_ref()).unwrap();
                        if message["id"] != id {
                            assert!(
                                message.get("method").is_some(),
                                "unexpected app-server response: {message}"
                            );
                            assert!(
                                message.get("id").is_none(),
                                "unexpected app-server request: {message}"
                            );
                            continue;
                        }
                        assert!(
                            message.get("error").is_none(),
                            "app-server request {method} failed: {message}"
                        );
                        return message.get("result").cloned().unwrap_or(Value::Null);
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        self.socket.send(Message::Pong(payload)).await.unwrap();
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(frame))) => {
                        panic!("app-server closed the WebSocket: {frame:?}")
                    }
                    Some(Ok(other)) => panic!("unexpected app-server frame: {other:?}"),
                    Some(Err(error)) => panic!("app-server WebSocket failed: {error}"),
                    None => panic!("app-server WebSocket ended"),
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("app-server request {method} timed out"))
    }

    async fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"method": method, "params": params})).await;
    }

    async fn send(&mut self, value: Value) {
        self.socket
            .send(Message::Text(value.to_string().into()))
            .await
            .unwrap();
    }
}
