use crate::checkpoint::Checkpoint;
use crate::error::{Error, ErrorCode, Result};
use crate::protocol::{
    AppEvent, InitializeResult, ResumeSnapshot, ThreadRef, loaded_thread_page, parse_notification,
    parse_resume_snapshot, turn_from_response,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, Semaphore, broadcast, mpsc, oneshot};
use tokio::time::{sleep, timeout};
use tokio_tungstenite::client_async_with_config;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

const HANDSHAKE_URL: &str = "ws://localhost/rpc";
const MAX_WEBSOCKET_BYTES: usize = 128 * 1024 * 1024;
const MAX_STANDARD_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_SNAPSHOT_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const MAX_INJECT_BYTES: usize = 64 * 1024;
const MAX_LOADED_THREADS: usize = 65_536;
const MAX_LOADED_PAGES: usize = 256;
const LOADED_PAGE_SIZE: usize = 256;
const REQUEST_LIMIT: usize = 64;
const EVENT_BUFFER: usize = 512;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MUTATING_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug)]
struct PendingRequest {
    sender: oneshot::Sender<Result<Value>>,
    max_response_bytes: usize,
    overflow_code: ErrorCode,
}

#[derive(Debug)]
enum WriterCommand {
    Text(String),
    Pong(Vec<u8>),
    Close,
}

struct Inner {
    writer: mpsc::Sender<WriterCommand>,
    pending: Mutex<HashMap<String, PendingRequest>>,
    events: broadcast::Sender<AppEvent>,
    next_id: AtomicU64,
    in_flight: Arc<Semaphore>,
    cancellation: CancellationToken,
    owners: AtomicUsize,
}

pub struct AppServerClient {
    inner: Arc<Inner>,
    pub initialize_result: Arc<InitializeResult>,
    endpoint: Arc<String>,
}

impl Clone for AppServerClient {
    fn clone(&self) -> Self {
        self.inner.owners.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: Arc::clone(&self.inner),
            initialize_result: Arc::clone(&self.initialize_result),
            endpoint: Arc::clone(&self.endpoint),
        }
    }
}

impl Drop for AppServerClient {
    fn drop(&mut self) {
        if self.inner.owners.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.inner.cancellation.cancel();
            let _ = self.inner.writer.try_send(WriterCommand::Close);
        }
    }
}

impl AppServerClient {
    pub async fn connect_default() -> Result<Self> {
        Self::connect(default_socket_path()?).await
    }

    #[cfg(unix)]
    pub async fn connect(socket_path: impl AsRef<Path>) -> Result<Self> {
        use tokio::net::UnixStream;

        let socket_path = socket_path.as_ref().to_path_buf();
        let endpoint = format!("unix://{}", socket_path.display());
        let stream = timeout(CONNECT_TIMEOUT, UnixStream::connect(&socket_path))
            .await
            .map_err(|_| {
                Error::new(
                    ErrorCode::SharedAppServerUnavailable,
                    "timed out connecting to the Codex app-server socket",
                )
                .component("app_server")
            })?
            .map_err(|error| {
                Error::new(
                    ErrorCode::SharedAppServerUnavailable,
                    format!("failed to connect to {}: {error}", socket_path.display()),
                )
                .component("app_server")
            })?;

        let request = HANDSHAKE_URL
            .into_client_request()
            .map_err(|error| Error::protocol(format!("invalid UDS handshake request: {error}")))?;
        let config = WebSocketConfig::default()
            .max_frame_size(Some(MAX_WEBSOCKET_BYTES))
            .max_message_size(Some(MAX_WEBSOCKET_BYTES));
        let websocket = timeout(
            CONNECT_TIMEOUT,
            client_async_with_config(request, stream, Some(config)),
        )
        .await
        .map_err(|_| {
            Error::new(
                ErrorCode::SharedAppServerUnavailable,
                "timed out upgrading the Codex app-server WebSocket",
            )
            .component("app_server")
        })?
        .map_err(|error| {
            Error::new(
                ErrorCode::SharedAppServerUnavailable,
                format!("failed to upgrade the Codex app-server WebSocket: {error}"),
            )
            .component("app_server")
        })?
        .0;

        let (mut sink, mut stream) = websocket.split();
        let (writer_tx, mut writer_rx) = mpsc::channel::<WriterCommand>(128);
        let (event_tx, _) = broadcast::channel(EVENT_BUFFER);
        let cancellation = CancellationToken::new();
        let inner = Arc::new(Inner {
            writer: writer_tx,
            pending: Mutex::new(HashMap::new()),
            events: event_tx,
            next_id: AtomicU64::new(1),
            in_flight: Arc::new(Semaphore::new(REQUEST_LIMIT)),
            cancellation: cancellation.clone(),
            owners: AtomicUsize::new(1),
        });

        let writer_cancel = cancellation.clone();
        tokio::spawn(async move {
            loop {
                let command = tokio::select! {
                    _ = writer_cancel.cancelled() => break,
                    command = writer_rx.recv() => command,
                };
                let Some(command) = command else {
                    break;
                };
                let result = match command {
                    WriterCommand::Text(text) => sink.send(Message::Text(text.into())).await,
                    WriterCommand::Pong(payload) => sink.send(Message::Pong(payload.into())).await,
                    WriterCommand::Close => {
                        let _ = sink.send(Message::Close(None)).await;
                        break;
                    }
                };
                if let Err(error) = result {
                    debug!(%error, "app-server writer stopped");
                    break;
                }
            }
            writer_cancel.cancel();
        });

        let reader_inner = Arc::clone(&inner);
        tokio::spawn(async move {
            let close_reason = loop {
                tokio::select! {
                    _ = reader_inner.cancellation.cancelled() => {
                        break "connection cancelled".to_owned();
                    }
                    next = stream.next() => {
                        match next {
                            Some(Ok(Message::Text(text))) => {
                                if text.len() > MAX_WEBSOCKET_BYTES {
                                    break "app-server message exceeded 128 MiB".to_owned();
                                }
                                if let Err(error) = handle_incoming(&reader_inner, text.as_ref()).await {
                                    warn!(
                                        code = error.code.as_str(),
                                        component = error.component,
                                        "invalid app-server message"
                                    );
                                }
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                let _ = reader_inner
                                    .writer
                                    .send(WriterCommand::Pong(payload.to_vec()))
                                    .await;
                            }
                            Some(Ok(Message::Pong(_))) => {}
                            Some(Ok(Message::Close(frame))) => {
                                break frame
                                    .map(|frame| frame.reason.to_string())
                                    .unwrap_or_else(|| "connection closed".to_owned());
                            }
                            Some(Ok(Message::Binary(_))) | Some(Ok(Message::Frame(_))) => {
                                break "unexpected non-text app-server frame".to_owned();
                            }
                            Some(Err(error)) => break format!("websocket read failed: {error}"),
                            None => break "app-server stream ended".to_owned(),
                        }
                    }
                }
            };
            reader_inner.cancellation.cancel();
            fail_pending(&reader_inner, &close_reason).await;
            let _ = reader_inner.events.send(AppEvent::ConnectionClosed {
                reason: close_reason,
            });
        });

        let mut provisional = Self {
            inner,
            initialize_result: Arc::new(InitializeResult {
                user_agent: String::new(),
                codex_home: String::new(),
                platform_family: String::new(),
                platform_os: String::new(),
            }),
            endpoint: Arc::new(endpoint),
        };
        let initialize = provisional
            .request_with_limit(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "agentic_compact",
                        "title": "Agentic Compact",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "capabilities": {
                        "experimentalApi": false,
                        "requestAttestation": false,
                        "optOutNotificationMethods": [
                            "item/agentMessage/delta",
                            "item/reasoning/summaryTextDelta",
                            "item/reasoning/textDelta",
                            "item/commandExecution/outputDelta"
                        ]
                    }
                }),
                MAX_STANDARD_RESPONSE_BYTES,
                ErrorCode::Protocol,
                REQUEST_TIMEOUT,
            )
            .await?;
        provisional.notify("initialized", json!({})).await?;
        let initialize_result = InitializeResult {
            user_agent: initialize
                .get("userAgent")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            codex_home: initialize
                .get("codexHome")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::protocol("initialize response is missing codexHome"))?
                .to_owned(),
            platform_family: initialize
                .get("platformFamily")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            platform_os: initialize
                .get("platformOs")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        };
        provisional.initialize_result = Arc::new(initialize_result);
        Ok(provisional)
    }

    #[cfg(not(unix))]
    pub async fn connect(_socket_path: impl AsRef<Path>) -> Result<Self> {
        Err(Error::new(
            ErrorCode::UnsupportedCodex,
            "agentic-compact 0.1.0 requires a Unix-domain app-server socket",
        )
        .component("app_server"))
    }

    pub fn endpoint(&self) -> &str {
        self.endpoint.as_str()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.inner.events.subscribe()
    }

    pub fn is_closed(&self) -> bool {
        self.inner.cancellation.is_cancelled()
    }

    pub async fn close(&self) {
        self.inner.cancellation.cancel();
        let _ = self.inner.writer.send(WriterCommand::Close).await;
    }

    pub async fn loaded_threads(&self) -> Result<Vec<String>> {
        let mut cursor: Option<String> = None;
        let mut output = Vec::new();
        let mut seen = HashSet::new();
        for _ in 0..MAX_LOADED_PAGES {
            let response = self
                .read_request(
                    "thread/loaded/list",
                    json!({"cursor": cursor, "limit": LOADED_PAGE_SIZE}),
                    false,
                )
                .await?;
            let (ids, next) = loaded_thread_page(&response)?;
            for id in ids {
                if seen.insert(id.clone()) {
                    output.push(id);
                }
                if output.len() > MAX_LOADED_THREADS {
                    return Err(Error::protocol(
                        "thread/loaded/list exceeded the 65,536 thread safety bound",
                    ));
                }
            }
            match next {
                Some(next_cursor) if cursor.as_deref() != Some(next_cursor.as_str()) => {
                    cursor = Some(next_cursor);
                }
                Some(_) => {
                    return Err(Error::protocol(
                        "thread/loaded/list returned a repeated pagination cursor",
                    ));
                }
                None => return Ok(output),
            }
        }
        Err(Error::protocol(
            "thread/loaded/list exceeded the 256 page safety bound",
        ))
    }

    pub async fn thread_read(&self, thread_id: &str, include_turns: bool) -> Result<ThreadRef> {
        let response = self
            .read_request(
                "thread/read",
                json!({"threadId": thread_id, "includeTurns": include_turns}),
                include_turns,
            )
            .await?;
        ThreadRef::from_response(&response)
    }

    pub async fn thread_resume(&self, thread_id: &str) -> Result<ResumeSnapshot> {
        let response = self
            .read_request("thread/resume", json!({"threadId": thread_id}), true)
            .await?;
        parse_resume_snapshot(&response)
    }

    pub async fn compact_start(&self, thread_id: &str) -> Result<()> {
        self.mutating_request("thread/compact/start", json!({"threadId": thread_id}))
            .await
            .map(|_| ())
    }

    pub async fn inject_checkpoint(&self, thread_id: &str, checkpoint: &Checkpoint) -> Result<()> {
        self.inject_items(thread_id, crate::checkpoint::injection_items(checkpoint)?)
            .await
    }

    pub async fn inject_items(&self, thread_id: &str, items: Vec<Value>) -> Result<()> {
        if items.is_empty() || items.len() > 16 {
            return Err(Error::invalid("thread/inject_items requires 1..=16 items"));
        }
        if serde_json::to_vec(&items)?.len() > MAX_INJECT_BYTES {
            return Err(Error::new(
                ErrorCode::CheckpointTooLarge,
                "thread/inject_items payload exceeds 64 KiB",
            )
            .component("app_server"));
        }
        self.mutating_request(
            "thread/inject_items",
            json!({"threadId": thread_id, "items": items}),
        )
        .await
        .map(|_| ())
    }

    pub async fn start_empty_turn(&self, thread_id: &str) -> Result<String> {
        let response = self
            .mutating_request("turn/start", json!({"threadId": thread_id, "input": []}))
            .await?;
        Ok(turn_from_response(&response)?.id)
    }

    pub async fn start_thread(&self, ephemeral: bool) -> Result<ResumeSnapshot> {
        let response = self
            .mutating_request("thread/start", json!({"ephemeral": ephemeral}))
            .await?;
        parse_resume_snapshot(&response)
    }

    pub async fn start_ephemeral_thread(&self) -> Result<ResumeSnapshot> {
        self.start_thread(true).await
    }

    pub async fn delete_thread(&self, thread_id: &str) -> Result<()> {
        self.mutating_request("thread/delete", json!({"threadId": thread_id}))
            .await
            .map(|_| ())
    }

    pub async fn unsubscribe(&self, thread_id: &str) -> Result<()> {
        self.request_with_limit(
            "thread/unsubscribe",
            json!({"threadId": thread_id}),
            MAX_STANDARD_RESPONSE_BYTES,
            ErrorCode::Protocol,
            REQUEST_TIMEOUT,
        )
        .await
        .map(|_| ())
    }

    async fn read_request(&self, method: &str, params: Value, snapshot: bool) -> Result<Value> {
        let max_bytes = if snapshot {
            MAX_SNAPSHOT_RESPONSE_BYTES
        } else {
            MAX_STANDARD_RESPONSE_BYTES
        };
        let overflow_code = if snapshot {
            ErrorCode::ThreadSnapshotTooLarge
        } else {
            ErrorCode::Protocol
        };
        let mut delay = Duration::from_millis(50);
        let mut last_error = None;
        for attempt in 0..4 {
            match self
                .request_with_limit(
                    method,
                    params.clone(),
                    max_bytes,
                    overflow_code,
                    REQUEST_TIMEOUT,
                )
                .await
            {
                Ok(value) => return Ok(value),
                Err(error) if error.code == ErrorCode::AppServerOverloaded && attempt < 3 => {
                    last_error = Some(error);
                    sleep(delay + deterministic_jitter()).await;
                    delay = (delay * 2).min(Duration::from_millis(800));
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.unwrap_or_else(|| Error::protocol("read request failed")))
    }

    async fn mutating_request(&self, method: &str, params: Value) -> Result<Value> {
        self.request_with_limit(
            method,
            params,
            MAX_STANDARD_RESPONSE_BYTES,
            ErrorCode::Protocol,
            MUTATING_TIMEOUT,
        )
        .await
    }

    async fn request_with_limit(
        &self,
        method: &str,
        params: Value,
        max_response_bytes: usize,
        overflow_code: ErrorCode,
        wait: Duration,
    ) -> Result<Value> {
        if self.is_closed() {
            return Err(Error::new(
                ErrorCode::SharedAppServerUnavailable,
                "app-server connection is closed",
            )
            .component("app_server"));
        }

        let permit = timeout(wait, Arc::clone(&self.inner.in_flight).acquire_owned())
            .await
            .map_err(|_| Error::timeout("app_server", "request limiter timed out"))?
            .map_err(|_| Error::protocol("request limiter closed"))?;
        let id_number = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let id = id_number.to_string();
        let (sender, receiver) = oneshot::channel();
        self.inner.pending.lock().await.insert(
            id.clone(),
            PendingRequest {
                sender,
                max_response_bytes,
                overflow_code,
            },
        );
        let message = json!({"id": id_number, "method": method, "params": params});
        let send_result = timeout(
            wait,
            self.inner
                .writer
                .send(WriterCommand::Text(message.to_string())),
        )
        .await;
        match send_result {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                self.inner.pending.lock().await.remove(&id);
                return Err(Error::new(
                    ErrorCode::SharedAppServerUnavailable,
                    "app-server writer is closed",
                )
                .component("app_server"));
            }
            Err(_) => {
                self.inner.pending.lock().await.remove(&id);
                return Err(Error::timeout(
                    "app_server",
                    format!("sending app-server request {method} timed out"),
                ));
            }
        }

        let result = timeout(wait, receiver).await;
        drop(permit);
        match result {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(Error::new(
                ErrorCode::SharedAppServerUnavailable,
                "app-server response channel closed",
            )
            .component("app_server")),
            Err(_) => {
                self.inner.pending.lock().await.remove(&id);
                Err(Error::timeout(
                    "app_server",
                    format!("app-server request {method} timed out"),
                ))
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let message = json!({"method": method, "params": params}).to_string();
        timeout(
            REQUEST_TIMEOUT,
            self.inner.writer.send(WriterCommand::Text(message)),
        )
        .await
        .map_err(|_| Error::timeout("app_server", "app-server notification timed out"))?
        .map_err(|_| {
            Error::new(
                ErrorCode::SharedAppServerUnavailable,
                "app-server writer is closed",
            )
            .component("app_server")
        })
    }
}

async fn handle_incoming(inner: &Arc<Inner>, text: &str) -> Result<()> {
    let message: Value = serde_json::from_str(text)?;
    if let Some(id) = message.get("id") {
        if message.get("method").is_some() {
            let event = parse_notification(&message)?;
            let _ = inner.events.send(event);
            return Ok(());
        }
        let key = request_id_key(id)?;
        let Some(pending) = inner.pending.lock().await.remove(&key) else {
            debug!(request_id = %key, "ignored late or unknown app-server response");
            return Ok(());
        };
        if text.len() > pending.max_response_bytes {
            let _ = pending.sender.send(Err(Error::new(
                pending.overflow_code,
                "app-server response exceeded the per-request size limit",
            )
            .component("app_server")));
            return Ok(());
        }
        let result = if let Some(error) = message.get("error") {
            let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown app-server error");
            Err(Error::rpc(code, format!("rpc error {code}: {message}")))
        } else {
            Ok(message.get("result").cloned().unwrap_or(Value::Null))
        };
        let _ = pending.sender.send(result);
        return Ok(());
    }
    if message.get("method").is_some() {
        let event = parse_notification(&message)?;
        let _ = inner.events.send(event);
    }
    Ok(())
}

async fn fail_pending(inner: &Arc<Inner>, reason: &str) {
    let pending = std::mem::take(&mut *inner.pending.lock().await);
    for (_, request) in pending {
        let _ = request.sender.send(Err(Error::new(
            ErrorCode::SharedAppServerUnavailable,
            format!("app-server connection closed: {reason}"),
        )
        .component("app_server")));
    }
}

fn request_id_key(value: &Value) -> Result<String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        _ => Err(Error::protocol("app-server response id is invalid")),
    }
}

fn deterministic_jitter() -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    Duration::from_millis((nanos % 31) as u64)
}

pub fn codex_home() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CODEX_HOME") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| Error::new(ErrorCode::Io, "HOME and CODEX_HOME are both unset"))?;
    Ok(home.join(".codex"))
}

pub fn default_socket_path() -> Result<PathBuf> {
    Ok(codex_home()?
        .join("app-server-control")
        .join("app-server-control.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_socket_is_under_codex_home() {
        let path = default_socket_path().unwrap();
        assert!(path.ends_with("app-server-control/app-server-control.sock"));
    }

    #[test]
    fn overload_error_is_structured() {
        let error = Error::rpc(-32001, "Server overloaded; retry later.");
        assert_eq!(error.code, ErrorCode::AppServerOverloaded);
        assert!(error.retryable);
        assert_eq!(error.rpc_code, Some(-32001));
    }
}
