use agentic_compact::app_server::AppServerClient;
use agentic_compact::error::ErrorCode;
use agentic_compact::protocol::{AppEvent, ResumeSnapshot};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tempfile::TempDir;
use tokio::process::{Child, Command};
use tokio::time::{sleep, timeout};

struct AppServer {
    _child: Child,
    _home: TempDir,
}

#[tokio::test]
#[ignore = "requires frozen Codex and kills an isolated unauthenticated app-server"]
async fn killed_real_app_server_fails_closed_without_a_retry() {
    let (mut server, socket) = start_app_server(false).await;
    let client = AppServerClient::connect(&socket).await.unwrap();
    let mut events = client.subscribe();
    let pid = server._child.id().unwrap().to_string();
    let status = Command::new("kill")
        .args(["-TERM", &pid])
        .status()
        .await
        .unwrap();
    assert!(status.success());
    server._child.wait().await.unwrap();
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
    let (server, socket) = start_app_server(true).await;
    let owner = AppServerClient::connect(&socket).await.unwrap();
    let started = owner.start_thread(false).await.unwrap();
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

async fn start_app_server(authenticated: bool) -> (AppServer, PathBuf) {
    let home = tempfile::tempdir().unwrap();
    if authenticated {
        seed_codex_home(home.path());
    }
    let socket = home.path().join("agentic-compact-real.sock");
    let codex = std::env::var_os("AGENTIC_COMPACT_CODEX_BIN").unwrap_or_else(|| "codex".into());
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
            return (
                AppServer {
                    _child: child,
                    _home: home,
                },
                socket,
            );
        }
        if child.try_wait().unwrap().is_some() {
            panic!("Codex app-server exited before creating its socket");
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("Codex app-server did not create its socket within five seconds");
}

fn seed_codex_home(target: &Path) {
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
    let config = source.join("config.toml");
    if config.is_file() {
        std::fs::copy(config, target.join("config.toml")).unwrap();
    }
}
