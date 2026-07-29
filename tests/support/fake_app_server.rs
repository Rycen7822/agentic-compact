use agentic_compact::app_server::AppServerClient;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::TempDir;
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

enum ServerCommand {
    Json(Value),
    Text(String),
    Close,
}

pub(crate) struct FakeServer {
    _directory: TempDir,
    codex_home: PathBuf,
    socket_path: PathBuf,
    requests: mpsc::Receiver<Value>,
    commands: mpsc::Sender<ServerCommand>,
    task: JoinHandle<()>,
}

#[allow(dead_code)]
impl FakeServer {
    pub(crate) async fn start() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let codex_home = directory.path().join("codex-home");
        let socket_path = codex_home
            .join("app-server-control")
            .join("app-server-control.sock");
        std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&socket_path).unwrap();
        let (request_tx, requests) = mpsc::channel(32);
        let (commands, mut command_rx) = mpsc::channel(32);
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket = accept_async(stream).await.unwrap();
            loop {
                tokio::select! {
                    command = command_rx.recv() => {
                        match command {
                            Some(ServerCommand::Json(value)) => {
                                websocket
                                    .send(Message::Text(value.to_string().into()))
                                    .await
                                    .unwrap();
                            }
                            Some(ServerCommand::Text(text)) => {
                                websocket.send(Message::Text(text.into())).await.unwrap();
                            }
                            Some(ServerCommand::Close) => {
                                websocket.send(Message::Close(None)).await.unwrap();
                                break;
                            }
                            None => break,
                        }
                    }
                    message = websocket.next() => {
                        match message {
                            Some(Ok(Message::Text(text))) => {
                                let value = serde_json::from_str(text.as_ref()).unwrap();
                                if request_tx.send(value).await.is_err() {
                                    break;
                                }
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                websocket.send(Message::Pong(payload)).await.unwrap();
                            }
                            Some(Ok(Message::Close(_))) | None => break,
                            Some(Ok(_)) => {}
                            Some(Err(_)) => break,
                        }
                    }
                }
            }
        });
        Self {
            _directory: directory,
            codex_home,
            socket_path,
            requests,
            commands,
            task,
        }
    }

    pub(crate) async fn connect(&mut self) -> AppServerClient {
        let socket_path = self.socket_path.clone();
        let connecting = tokio::spawn(async move { AppServerClient::connect(socket_path).await });
        self.initialize_connection().await;
        connecting.await.unwrap().unwrap()
    }

    pub(crate) async fn initialize_connection(&mut self) {
        let initialize = self.next_request().await;
        assert_eq!(initialize["method"], "initialize");
        assert_eq!(
            initialize["params"]["capabilities"]["experimentalApi"],
            false
        );
        self.send(json!({
            "id": initialize["id"],
            "result": {
                "userAgent": "codex-test/0.1",
                "codexHome": self.codex_home.display().to_string(),
                "platformFamily": "unix",
                "platformOs": "linux"
            }
        }))
        .await;
        let initialized = self.next_request().await;
        assert_eq!(initialized["method"], "initialized");
        assert!(initialized.get("id").is_none());
    }

    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub(crate) fn codex_home(&self) -> &Path {
        &self.codex_home
    }

    pub(crate) async fn next_request(&mut self) -> Value {
        timeout(Duration::from_secs(2), self.requests.recv())
            .await
            .expect("fake app-server did not receive a request")
            .expect("fake app-server request channel closed")
    }

    pub(crate) fn try_next_request(&mut self) -> Option<Value> {
        self.requests.try_recv().ok()
    }

    pub(crate) async fn send(&self, value: Value) {
        self.commands
            .send(ServerCommand::Json(value))
            .await
            .unwrap();
    }

    pub(crate) async fn send_text(&self, text: impl Into<String>) {
        self.commands
            .send(ServerCommand::Text(text.into()))
            .await
            .unwrap();
    }

    pub(crate) async fn close(&self) {
        self.commands.send(ServerCommand::Close).await.unwrap();
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
