use super::State;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::net::UnixStream;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

pub(super) async fn serve(
    stream: UnixStream,
    state: Arc<Mutex<State>>,
    notifications: broadcast::Sender<Value>,
    cancellation: CancellationToken,
    codex_home: PathBuf,
) {
    let mut websocket = tokio_tungstenite::accept_async(stream).await.unwrap();
    let mut notification_rx = notifications.subscribe();
    let _guard = ConnectionGuard::new(Arc::clone(&state));
    let mut bound_thread = None;
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => break,
            notification = notification_rx.recv() => {
                match notification {
                    Ok(notification) => {
                        if !try_send(&mut websocket, notification).await {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => panic!("transition server notification lagged"),
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            message = websocket.next() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let request: Value = serde_json::from_str(text.as_ref()).unwrap();
                        if request.get("id").is_none() {
                            continue;
                        }
                        handle_request(
                            &mut websocket,
                            &state,
                            &notifications,
                            &mut bound_thread,
                            &codex_home,
                            request,
                        ).await;
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
}

async fn handle_request(
    websocket: &mut tokio_tungstenite::WebSocketStream<UnixStream>,
    state: &Arc<Mutex<State>>,
    notifications: &broadcast::Sender<Value>,
    bound_thread: &mut Option<String>,
    codex_home: &Path,
    request: Value,
) {
    let id = request["id"].clone();
    let method = request["method"].as_str().unwrap();
    let params = &request["params"];
    match method {
        "initialize" => {
            respond(
                websocket,
                id,
                json!({
                    "userAgent": "codex-test/0.1",
                    "codexHome": codex_home.display().to_string(),
                    "platformFamily": "unix",
                    "platformOs": "linux"
                }),
            )
            .await;
        }
        "thread/resume" => {
            let thread_id = params["threadId"].as_str().unwrap();
            check_affinity(state, bound_thread.as_deref(), thread_id);
            *bound_thread = Some(thread_id.to_owned());
            let snapshot = snapshot(state, thread_id);
            respond(websocket, id, json!({"thread": snapshot})).await;
        }
        "thread/loaded/list" => {
            let data = bound_thread.iter().cloned().collect::<Vec<_>>();
            respond(websocket, id, json!({"data": data, "nextCursor": null})).await;
        }
        "thread/read" => {
            let thread_id = params["threadId"].as_str().unwrap();
            check_affinity(state, bound_thread.as_deref(), thread_id);
            respond(websocket, id, snapshot(state, thread_id)).await;
        }
        "thread/compact/start" => {
            let thread_id = params["threadId"].as_str().unwrap();
            check_affinity(state, bound_thread.as_deref(), thread_id);
            let compact_notifications = begin_compaction(state, thread_id);
            respond(websocket, id, json!({})).await;
            for notification in compact_notifications {
                notifications.send(notification).unwrap();
            }
        }
        "thread/inject_items" => {
            let thread_id = params["threadId"].as_str().unwrap();
            check_affinity(state, bound_thread.as_deref(), thread_id);
            record_injection(state, thread_id, &params["items"]);
            respond(websocket, id, json!({})).await;
            state
                .lock()
                .unwrap()
                .threads
                .get_mut(thread_id)
                .unwrap()
                .records
                .last_mut()
                .unwrap()
                .injection_acknowledged_at = Some(Instant::now());
        }
        "turn/start" => {
            let thread_id = params["threadId"].as_str().unwrap();
            check_affinity(state, bound_thread.as_deref(), thread_id);
            let (turn, notification) = begin_continuation(state, thread_id, &params["input"]);
            respond(websocket, id, json!({"turn": turn})).await;
            notifications.send(notification).unwrap();
        }
        "thread/unsubscribe" => {
            let thread_id = params["threadId"].as_str().unwrap();
            check_affinity(state, bound_thread.as_deref(), thread_id);
            state
                .lock()
                .unwrap()
                .threads
                .get_mut(thread_id)
                .unwrap()
                .records
                .last_mut()
                .unwrap()
                .unsubscribed = true;
            respond(websocket, id, json!({})).await;
        }
        _ => panic!("unexpected transition-server request: {method}"),
    }
}

fn begin_compaction(state: &Arc<Mutex<State>>, thread_id: &str) -> Vec<Value> {
    let mut state = state.lock().unwrap();
    let thread = state.threads.get_mut(thread_id).unwrap();
    let compact_id = {
        let record = thread.records.last_mut().unwrap();
        record.compact_requests += 1;
        record.compact_requested_at = Some(Instant::now());
        record.compact_id.clone()
    };
    thread.turns.push(json!({
        "id": compact_id,
        "status": "completed",
        "items": [{
            "id": format!("{compact_id}-item"),
            "type": "contextCompaction",
            "status": "completed"
        }]
    }));
    thread.status = "idle";
    thread.records.last_mut().unwrap().compact_completed_at = Some(Instant::now());
    vec![
        json!({
            "method": "turn/started",
            "params": {
                "threadId": thread_id,
                "turn": {"id": compact_id, "status": "inProgress", "items": []}
            }
        }),
        json!({
            "method": "item/started",
            "params": {
                "threadId": thread_id,
                "turnId": compact_id,
                "startedAtMs": 1,
                "item": {"id": format!("{compact_id}-item"), "type": "contextCompaction"}
            }
        }),
        json!({
            "method": "item/completed",
            "params": {
                "threadId": thread_id,
                "turnId": compact_id,
                "completedAtMs": 2,
                "item": {
                    "id": format!("{compact_id}-item"),
                    "type": "contextCompaction",
                    "status": "completed"
                }
            }
        }),
        json!({
            "method": "turn/completed",
            "params": {
                "threadId": thread_id,
                "turn": {"id": compact_id, "status": "completed", "items": []}
            }
        }),
    ]
}

fn record_injection(state: &Arc<Mutex<State>>, thread_id: &str, items: &Value) {
    let mut state = state.lock().unwrap();
    let record = state
        .threads
        .get_mut(thread_id)
        .unwrap()
        .records
        .last_mut()
        .unwrap();
    record.injections += 1;
    record.injection_requested_at = Some(Instant::now());
    let serialized = items.to_string();
    record.checkpoint_present = serialized.contains(&record.expected_receipt)
        && serialized.contains(&record.expected_checkpoint);
    record.synthetic_user_messages += items
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["role"] == "user" || item["type"] == "userMessage")
        .count();
}

fn begin_continuation(state: &Arc<Mutex<State>>, thread_id: &str, input: &Value) -> (Value, Value) {
    let mut state = state.lock().unwrap();
    let thread = state.threads.get_mut(thread_id).unwrap();
    let record = thread.records.last_mut().unwrap();
    record.continuation_requested_at = Some(Instant::now());
    record.synthetic_user_messages += usize::from(input != &json!([]));
    let continuation_id = record.continuation_id.clone();
    let turn = json!({
        "id": continuation_id,
        "status": "inProgress",
        "items": []
    });
    thread.turns.push(turn.clone());
    thread.status = "active";
    let notification = json!({
        "method": "turn/started",
        "params": {"threadId": thread_id, "turn": turn}
    });
    (turn, notification)
}

fn snapshot(state: &Arc<Mutex<State>>, thread_id: &str) -> Value {
    let state = state.lock().unwrap();
    let thread = state.threads.get(thread_id).unwrap();
    json!({
        "id": thread_id,
        "parentThreadId": null,
        "status": {"type": thread.status},
        "turns": thread.turns
    })
}

fn check_affinity(state: &Arc<Mutex<State>>, bound_thread: Option<&str>, requested: &str) {
    if bound_thread.is_some_and(|bound| bound != requested) {
        state.lock().unwrap().cross_thread_actions += 1;
    }
}

async fn respond(
    websocket: &mut tokio_tungstenite::WebSocketStream<UnixStream>,
    id: Value,
    result: Value,
) {
    send(websocket, json!({"id": id, "result": result})).await;
}

async fn send(websocket: &mut tokio_tungstenite::WebSocketStream<UnixStream>, value: Value) {
    assert!(
        try_send(websocket, value).await,
        "transition server response write failed"
    );
}

async fn try_send(
    websocket: &mut tokio_tungstenite::WebSocketStream<UnixStream>,
    value: Value,
) -> bool {
    websocket
        .send(Message::Text(value.to_string().into()))
        .await
        .is_ok()
}

struct ConnectionGuard(Arc<Mutex<State>>);

impl ConnectionGuard {
    fn new(state: Arc<Mutex<State>>) -> Self {
        state.lock().unwrap().connections += 1;
        Self(state)
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.lock().unwrap().connections -= 1;
    }
}
