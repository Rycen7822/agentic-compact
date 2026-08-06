#![allow(dead_code)]

mod connection;

use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::net::UnixListener;
use tokio::sync::broadcast;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug)]
pub(crate) struct TransitionSample {
    pub(crate) source_to_compact: Duration,
    pub(crate) compact_to_injection: Duration,
    pub(crate) injection_to_continuation: Duration,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Integrity {
    pub(crate) transitions: usize,
    pub(crate) duplicate_compacts: usize,
    pub(crate) lost_checkpoints: usize,
    pub(crate) synthetic_user_messages: usize,
    pub(crate) cross_thread_actions: usize,
}

#[derive(Default)]
pub(super) struct State {
    pub(super) threads: HashMap<String, ThreadState>,
    pub(super) connections: usize,
    pub(super) cross_thread_actions: usize,
}

pub(super) struct ThreadState {
    status: &'static str,
    turns: Vec<Value>,
    records: Vec<TransitionRecord>,
}

#[derive(Default)]
struct TransitionRecord {
    source_id: String,
    compact_id: String,
    continuation_id: String,
    expected_receipt: String,
    expected_checkpoint: String,
    source_completed_at: Option<Instant>,
    compact_requested_at: Option<Instant>,
    compact_completed_at: Option<Instant>,
    injection_requested_at: Option<Instant>,
    injection_acknowledged_at: Option<Instant>,
    continuation_requested_at: Option<Instant>,
    compact_requests: usize,
    injections: usize,
    synthetic_user_messages: usize,
    checkpoint_present: bool,
    unsubscribed: bool,
}

pub(crate) struct TransitionServer {
    _directory: TempDir,
    codex_home: PathBuf,
    state: Arc<Mutex<State>>,
    notifications: broadcast::Sender<Value>,
    cancellation: CancellationToken,
    task: Option<JoinHandle<()>>,
}

impl TransitionServer {
    pub(crate) async fn start() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let codex_home = directory.path().join("codex-home");
        let socket = codex_home
            .join("app-server-control")
            .join("app-server-control.sock");
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&socket).unwrap();
        let state = Arc::new(Mutex::new(State::default()));
        let (notifications, _) = broadcast::channel(2048);
        let cancellation = CancellationToken::new();
        let task_state = Arc::clone(&state);
        let task_notifications = notifications.clone();
        let task_cancellation = cancellation.clone();
        let task_codex_home = codex_home.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    _ = task_cancellation.cancelled() => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break };
                        connections.spawn(connection::serve(
                            stream,
                            Arc::clone(&task_state),
                            task_notifications.clone(),
                            task_cancellation.clone(),
                            task_codex_home.clone(),
                        ));
                    }
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        Self {
            _directory: directory,
            codex_home,
            state,
            notifications,
            cancellation,
            task: Some(task),
        }
    }

    pub(crate) fn codex_home(&self) -> &Path {
        &self.codex_home
    }

    pub(crate) fn prepare_transition(&self, thread_id: &str, sequence: usize) -> String {
        let mut state = self.state.lock().unwrap();
        let thread = state
            .threads
            .entry(thread_id.to_owned())
            .or_insert_with(|| ThreadState {
                status: "idle",
                turns: Vec::new(),
                records: Vec::new(),
            });
        assert_eq!(thread.records.len(), sequence);
        if let Some(continuation_id) = thread
            .records
            .last()
            .map(|previous| previous.continuation_id.clone())
        {
            set_turn_status(&mut thread.turns, &continuation_id, "completed");
            for regular in 0..3 {
                thread.turns.push(json!({
                    "id": format!("{thread_id}-regular-{sequence}-{regular}"),
                    "status": "completed",
                    "items": []
                }));
            }
        }
        let source_id = format!("{thread_id}-source-{sequence}");
        thread.turns.push(json!({
            "id": source_id,
            "status": "inProgress",
            "items": []
        }));
        thread.status = "active";
        thread.records.push(TransitionRecord {
            source_id: source_id.clone(),
            compact_id: format!("{thread_id}-compact-{sequence}"),
            continuation_id: format!("{thread_id}-continuation-{sequence}"),
            ..TransitionRecord::default()
        });
        source_id
    }

    pub(crate) fn complete_source(
        &self,
        thread_id: &str,
        sequence: usize,
        receipt_id: &str,
        checkpoint_id: &str,
    ) {
        let (source_id, completed_at) = {
            let mut state = self.state.lock().unwrap();
            let thread = state.threads.get_mut(thread_id).unwrap();
            let (source_id, completed_at) = {
                let record = thread.records.get_mut(sequence).unwrap();
                record.expected_receipt = receipt_id.to_owned();
                record.expected_checkpoint = checkpoint_id.to_owned();
                let completed_at = Instant::now();
                record.source_completed_at = Some(completed_at);
                (record.source_id.clone(), completed_at)
            };
            set_turn_status(&mut thread.turns, &source_id, "completed");
            thread.status = "idle";
            (source_id, completed_at)
        };
        let completed_at_ms = completed_at.elapsed().as_millis() as i64;
        self.publish(json!({
            "method": "item/completed",
            "params": {
                "threadId": thread_id,
                "turnId": source_id,
                "completedAtMs": completed_at_ms,
                "item": {
                    "id": format!("{thread_id}-request-{sequence}"),
                    "type": "mcpToolCall",
                    "status": "completed",
                    "server": "agentic-compact",
                    "tool": "request_compaction",
                    "result": {
                        "_meta": {"agenticCompact": {"receiptId": receipt_id}}
                    }
                }
            }
        }));
        self.publish(json!({
            "method": "turn/completed",
            "params": {
                "threadId": thread_id,
                "turn": {"id": source_id, "status": "completed", "items": []}
            }
        }));
    }

    pub(crate) async fn wait_for_transition(
        &self,
        thread_id: &str,
        sequence: usize,
    ) -> TransitionSample {
        timeout(Duration::from_secs(10), async {
            loop {
                if let Some(sample) = self.sample_if_complete(thread_id, sequence) {
                    return sample;
                }
                sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{thread_id} transition {sequence} did not finish"))
    }

    pub(crate) async fn wait_for_no_connections(&self) {
        timeout(Duration::from_secs(10), async {
            loop {
                if self.state.lock().unwrap().connections == 0 {
                    return;
                }
                sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("transition connections leaked");
    }

    pub(crate) fn integrity(&self) -> Integrity {
        let state = self.state.lock().unwrap();
        let mut integrity = Integrity {
            cross_thread_actions: state.cross_thread_actions,
            ..Integrity::default()
        };
        for thread in state.threads.values() {
            for record in &thread.records {
                integrity.transitions += 1;
                integrity.duplicate_compacts += record.compact_requests.saturating_sub(1);
                integrity.lost_checkpoints += usize::from(!record.checkpoint_present);
                integrity.synthetic_user_messages += record.synthetic_user_messages;
            }
        }
        integrity
    }

    pub(crate) async fn shutdown(mut self) {
        self.cancellation.cancel();
        if let Some(task) = self.task.take() {
            task.await.unwrap();
        }
    }

    fn sample_if_complete(&self, thread_id: &str, sequence: usize) -> Option<TransitionSample> {
        let state = self.state.lock().unwrap();
        let record = state.threads.get(thread_id)?.records.get(sequence)?;
        if !record.unsubscribed {
            return None;
        }
        Some(TransitionSample {
            source_to_compact: record
                .compact_requested_at?
                .duration_since(record.source_completed_at?),
            compact_to_injection: record
                .injection_requested_at?
                .duration_since(record.compact_completed_at?),
            injection_to_continuation: record
                .continuation_requested_at?
                .duration_since(record.injection_acknowledged_at?),
        })
    }

    fn publish(&self, notification: Value) {
        self.notifications
            .send(notification)
            .expect("transition notification has no subscriber");
    }
}

impl Drop for TransitionServer {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

fn set_turn_status(turns: &mut [Value], turn_id: &str, status: &str) {
    let turn = turns
        .iter_mut()
        .find(|turn| turn["id"] == turn_id)
        .unwrap_or_else(|| panic!("turn {turn_id} is missing"));
    turn["status"] = Value::String(status.to_owned());
}
