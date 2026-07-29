use crate::error::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;

const MAX_SNAPSHOT_SETTING_BYTES: usize = 16 * 1024;
const MAX_PROJECTED_OBJECTIVE_SCALARS: usize = 512;
const MAX_PROJECTED_CHANGED_FILES: usize = 64;
const MAX_PROJECTED_PATH_SCALARS: usize = 256;
const MAX_PROJECTED_ITEM_ID_SCALARS: usize = 128;
const MAX_PROJECTED_STATUS_SCALARS: usize = 32;
const MAX_RECEIPT_IDS_PER_ITEM: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub user_agent: String,
    pub codex_home: String,
    pub platform_family: String,
    pub platform_os: String,
}

#[derive(Debug, Clone)]
pub struct ResumeSnapshot {
    pub thread: ThreadRef,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub cwd: Option<String>,
    pub approval_policy: Option<Value>,
    pub sandbox: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadRef {
    pub id: String,
    pub parent_thread_id: Option<String>,
    pub status: String,
    pub turns: Vec<TurnRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRef {
    pub id: String,
    pub status: String,
    pub items: Vec<ItemRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemRef {
    pub id: String,
    pub item_type: String,
    pub status: Option<String>,
    pub server: Option<String>,
    pub tool: Option<String>,
    pub receipt_ids: Vec<String>,
    pub has_error: bool,
    #[serde(default)]
    pub safe_evidence: Vec<Value>,
}

#[derive(Debug, Clone)]
pub enum AppEvent {
    TurnStarted {
        thread_id: String,
        turn: TurnRef,
    },
    TurnCompleted {
        thread_id: String,
        turn: TurnRef,
    },
    ItemStarted {
        thread_id: String,
        turn_id: String,
        item: ItemRef,
    },
    ItemCompleted {
        thread_id: String,
        turn_id: String,
        item: ItemRef,
    },
    ThreadStatusChanged {
        thread_id: String,
        status: String,
    },
    TokenUsageUpdated {
        thread_id: String,
        turn_id: String,
        active_context_tokens: Option<i64>,
    },
    ServerRequest {
        method: String,
        id: Value,
    },
    UnknownNotification {
        method: String,
    },
    ConnectionClosed {
        reason: String,
    },
}

impl ThreadRef {
    pub fn from_response(response: &Value) -> Result<Self> {
        let thread = response.get("thread").unwrap_or(response);
        let object = thread
            .as_object()
            .ok_or_else(|| Error::protocol("thread response must contain an object"))?;
        let id = required_string(object.get("id"), "thread.id")?;
        let parent_thread_id = optional_string(object.get("parentThreadId"));
        let status = parse_thread_status(object.get("status"))?;
        let turns = object
            .get("turns")
            .and_then(Value::as_array)
            .map(|values| values.iter().map(TurnRef::from_value).collect())
            .transpose()?
            .unwrap_or_default();
        Ok(Self {
            id,
            parent_thread_id,
            status,
            turns,
        })
    }

    pub fn is_active(&self) -> bool {
        self.status == "active"
    }

    pub fn is_idle(&self) -> bool {
        self.status == "idle"
    }

    pub fn find_turn(&self, turn_id: &str) -> Option<&TurnRef> {
        self.turns.iter().find(|turn| turn.id == turn_id)
    }
}

impl TurnRef {
    pub fn from_value(value: &Value) -> Result<Self> {
        let object = value
            .as_object()
            .ok_or_else(|| Error::protocol("turn must be an object"))?;
        let id = required_string(object.get("id"), "turn.id")?;
        let status = required_string(object.get("status"), "turn.status")?;
        let items = object
            .get("items")
            .and_then(Value::as_array)
            .map(|values| values.iter().map(ItemRef::from_value).collect())
            .transpose()?
            .unwrap_or_default();
        Ok(Self { id, status, items })
    }

    pub fn is_compaction(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.item_type == "contextCompaction")
    }

    pub fn is_completed_regular(&self) -> bool {
        self.status == "completed" && !self.is_compaction()
    }
}

impl ItemRef {
    pub fn from_value(value: &Value) -> Result<Self> {
        let object = value
            .as_object()
            .ok_or_else(|| Error::protocol("thread item must be an object"))?;
        let id = required_string(object.get("id"), "item.id")?;
        let item_type = required_string(object.get("type"), "item.type")?;
        let status = optional_string(object.get("status"));
        let server = optional_string(object.get("server"));
        let tool = optional_string(object.get("tool"));
        let receipt_ids = project_receipt_ids(object.get("result"));
        let has_error = object.get("error").is_some_and(|value| !value.is_null());
        let safe_evidence = project_safe_evidence(&item_type, object);
        Ok(Self {
            id,
            item_type,
            status,
            server,
            tool,
            receipt_ids,
            has_error,
            safe_evidence,
        })
    }

    pub fn is_allowed_in_compaction_turn(&self) -> bool {
        self.item_type == "contextCompaction"
    }

    pub fn completed_successfully(&self) -> bool {
        self.status
            .as_deref()
            .is_none_or(|status| status == "completed")
            && !self.has_error
    }

    pub fn contains_receipt(&self, receipt_id: &str) -> bool {
        self.receipt_ids.iter().any(|value| value == receipt_id)
    }
}

pub fn parse_resume_snapshot(response: &Value) -> Result<ResumeSnapshot> {
    Ok(ResumeSnapshot {
        thread: ThreadRef::from_response(response)?,
        model: optional_snapshot_string(response, "model")?,
        reasoning_effort: optional_snapshot_string(response, "reasoningEffort")?,
        cwd: optional_snapshot_string(response, "cwd")?,
        approval_policy: optional_snapshot_value(response, "approvalPolicy")?,
        sandbox: optional_snapshot_value(response, "sandbox")?,
    })
}

fn optional_snapshot_string(response: &Value, field: &'static str) -> Result<Option<String>> {
    match response.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.len() <= MAX_SNAPSHOT_SETTING_BYTES => {
            Ok(Some(value.clone()))
        }
        Some(Value::String(_)) => Err(Error::new(
            ErrorCode::ThreadSnapshotTooLarge,
            "thread setting exceeds the snapshot field limit",
        )
        .component("protocol")),
        Some(_) => Err(Error::protocol("thread setting has an invalid type")),
    }
}

fn optional_snapshot_value(response: &Value, field: &'static str) -> Result<Option<Value>> {
    let Some(value) = response.get(field) else {
        return Ok(None);
    };
    if serde_json::to_vec(value)?.len() > MAX_SNAPSHOT_SETTING_BYTES {
        return Err(Error::new(
            ErrorCode::ThreadSnapshotTooLarge,
            "thread setting exceeds the snapshot field limit",
        )
        .component("protocol"));
    }
    Ok(Some(value.clone()))
}

pub fn turn_from_response(response: &Value) -> Result<TurnRef> {
    let turn = response
        .get("turn")
        .ok_or_else(|| Error::protocol("turn response is missing turn"))?;
    TurnRef::from_value(turn)
}

pub fn loaded_thread_page(response: &Value) -> Result<(Vec<String>, Option<String>)> {
    let data = response
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::protocol("thread/loaded/list response is missing data"))?;
    let mut seen = HashSet::new();
    let mut ids = Vec::with_capacity(data.len());
    for value in data {
        let id = value
            .as_str()
            .ok_or_else(|| Error::protocol("thread/loaded/list data must contain strings"))?;
        if seen.insert(id.to_owned()) {
            ids.push(id.to_owned());
        }
    }
    let next_cursor = response
        .get("nextCursor")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok((ids, next_cursor))
}

pub fn parse_notification(message: &Value) -> Result<AppEvent> {
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::protocol("app-server notification is missing method"))?;
    if let Some(id) = message.get("id") {
        return Ok(AppEvent::ServerRequest {
            method: method.to_owned(),
            id: id.clone(),
        });
    }
    let params = message.get("params").unwrap_or(&Value::Null);
    match method {
        "turn/started" => Ok(AppEvent::TurnStarted {
            thread_id: required_path_string(params, "/threadId", "threadId")?,
            turn: TurnRef::from_value(
                params
                    .get("turn")
                    .ok_or_else(|| Error::protocol("turn/started is missing turn"))?,
            )?,
        }),
        "turn/completed" => Ok(AppEvent::TurnCompleted {
            thread_id: required_path_string(params, "/threadId", "threadId")?,
            turn: TurnRef::from_value(
                params
                    .get("turn")
                    .ok_or_else(|| Error::protocol("turn/completed is missing turn"))?,
            )?,
        }),
        "item/started" => Ok(AppEvent::ItemStarted {
            thread_id: required_path_string(params, "/threadId", "threadId")?,
            turn_id: required_path_string(params, "/turnId", "turnId")?,
            item: ItemRef::from_value(
                params
                    .get("item")
                    .ok_or_else(|| Error::protocol("item/started is missing item"))?,
            )?,
        }),
        "item/completed" => Ok(AppEvent::ItemCompleted {
            thread_id: required_path_string(params, "/threadId", "threadId")?,
            turn_id: required_path_string(params, "/turnId", "turnId")?,
            item: ItemRef::from_value(
                params
                    .get("item")
                    .ok_or_else(|| Error::protocol("item/completed is missing item"))?,
            )?,
        }),
        "thread/status/changed" => Ok(AppEvent::ThreadStatusChanged {
            thread_id: required_path_string(params, "/threadId", "threadId")?,
            status: parse_thread_status(params.get("status"))?,
        }),
        "thread/tokenUsage/updated" => Ok(AppEvent::TokenUsageUpdated {
            thread_id: required_path_string(params, "/threadId", "threadId")?,
            turn_id: required_path_string(params, "/turnId", "turnId")?,
            active_context_tokens: token_usage_total(params.get("tokenUsage")),
        }),
        _ => Ok(AppEvent::UnknownNotification {
            method: method.to_owned(),
        }),
    }
}

pub fn completed_regular_turns_after(thread: &ThreadRef, turn_id: &str) -> Option<usize> {
    let index = thread.turns.iter().position(|turn| turn.id == turn_id)?;
    Some(
        thread.turns[index + 1..]
            .iter()
            .filter(|turn| turn.is_completed_regular())
            .count(),
    )
}

fn project_safe_evidence(item_type: &str, object: &serde_json::Map<String, Value>) -> Vec<Value> {
    match item_type {
        "userMessage" => {
            let text = bounded_user_text(object.get("content"));
            (!text.trim().is_empty())
                .then(|| json!({"kind": "user_objective", "text": text}))
                .into_iter()
                .collect()
        }
        "fileChange" if object.get("status").and_then(Value::as_str) == Some("completed") => {
            let mut paths = object
                .get("changes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .rev()
                .filter_map(|change| change.get("path").and_then(Value::as_str))
                .take(MAX_PROJECTED_CHANGED_FILES)
                .map(|path| {
                    json!({
                        "kind": "changed_file",
                        "path": truncate_scalars(path, MAX_PROJECTED_PATH_SCALARS)
                    })
                })
                .collect::<Vec<_>>();
            paths.reverse();
            paths
        }
        "commandExecution" => {
            let command = object.get("command").and_then(Value::as_str);
            let status = object.get("status").and_then(Value::as_str);
            match (command.and_then(verification_label), status) {
                (Some(label), Some(status)) => vec![json!({
                    "kind": "verification",
                    "itemId": truncate_scalars(
                        object.get("id").and_then(Value::as_str).unwrap_or(""),
                        MAX_PROJECTED_ITEM_ID_SCALARS,
                    ),
                    "verificationKind": "test",
                    "label": label,
                    "status": truncate_scalars(status, MAX_PROJECTED_STATUS_SCALARS),
                    "exitCode": object.get("exitCode").and_then(Value::as_i64)
                })],
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

fn bounded_user_text(content: Option<&Value>) -> String {
    let mut output = String::new();
    let mut remaining = MAX_PROJECTED_OBJECTIVE_SCALARS;
    for text in content
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|entry| entry.get("text").and_then(Value::as_str))
    {
        if remaining == 0 {
            break;
        }
        if !output.is_empty() {
            output.push('\n');
            remaining -= 1;
        }
        let projected = truncate_scalars(text, remaining);
        remaining -= projected.chars().count();
        output.push_str(&projected);
    }
    output
}

fn verification_label(command: &str) -> Option<&'static str> {
    let command = command.trim_start();
    [
        ("cargo test", "cargo test"),
        ("cargo check", "cargo check"),
        ("cargo clippy", "cargo clippy"),
        ("pytest", "pytest"),
        ("python -m pytest", "python -m pytest"),
        ("npm test", "npm test"),
        ("npm run test", "npm run test"),
        ("pnpm test", "pnpm test"),
        ("yarn test", "yarn test"),
        ("go test", "go test"),
        ("make test", "make test"),
        ("cmake --build", "cmake --build"),
    ]
    .iter()
    .find_map(|(prefix, label)| {
        let prefix_end = command.get(..prefix.len())?;
        let remainder = command.get(prefix.len()..)?;
        (prefix_end.eq_ignore_ascii_case(prefix)
            && remainder.chars().next().is_none_or(char::is_whitespace))
        .then_some(*label)
    })
}

fn project_receipt_ids(value: Option<&Value>) -> Vec<String> {
    let mut receipt_ids = Vec::new();
    if let Some(value) = value {
        collect_receipt_ids(value, &mut receipt_ids);
    }
    receipt_ids
}

fn collect_receipt_ids(value: &Value, receipt_ids: &mut Vec<String>) {
    if receipt_ids.len() >= MAX_RECEIPT_IDS_PER_ITEM {
        return;
    }
    match value {
        Value::String(text) => {
            for (start, _) in text.match_indices("rcpt_") {
                let Some(candidate) = text.get(start..start + 37) else {
                    continue;
                };
                if candidate[5..].bytes().all(|byte| byte.is_ascii_hexdigit())
                    && !receipt_ids.iter().any(|value| value == candidate)
                {
                    receipt_ids.push(candidate.to_owned());
                    if receipt_ids.len() == MAX_RECEIPT_IDS_PER_ITEM {
                        break;
                    }
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_receipt_ids(value, receipt_ids);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                collect_receipt_ids(value, receipt_ids);
            }
        }
        _ => {}
    }
}

fn truncate_scalars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn required_string(value: Option<&Value>, label: &str) -> Result<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| Error::protocol(format!("{label} is missing or invalid")))
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

fn required_path_string(value: &Value, pointer: &str, label: &str) -> Result<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| Error::protocol(format!("{label} is missing or invalid")))
}

fn parse_thread_status(value: Option<&Value>) -> Result<String> {
    match value {
        Some(Value::String(status)) => Ok(status.clone()),
        Some(Value::Object(object)) => required_string(object.get("type"), "thread.status.type"),
        _ => Err(Error::protocol("thread status is missing or invalid")),
    }
}

fn token_usage_total(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    [
        "/total/totalTokens",
        "/totalTokens",
        "/last/totalTokens",
        "/activeContextTokens",
    ]
    .iter()
    .find_map(|pointer| value.pointer(pointer).and_then(Value::as_i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_loaded_thread_string_page() {
        let (ids, cursor) = loaded_thread_page(&json!({
            "data": ["a", "b", "a"],
            "nextCursor": "c"
        }))
        .unwrap();
        assert_eq!(ids, vec!["a", "b"]);
        assert_eq!(cursor.as_deref(), Some("c"));
    }

    #[test]
    fn parses_context_compaction_item() {
        let item = ItemRef::from_value(&json!({
            "type": "contextCompaction",
            "id": "item_1"
        }))
        .unwrap();
        assert!(item.is_allowed_in_compaction_turn());
    }

    #[test]
    fn item_projection_keeps_only_bounded_safe_fields() {
        let receipt_id = format!("rcpt_{}", "a".repeat(32));
        let item = ItemRef::from_value(&json!({
            "type": "mcpToolCall",
            "id": "item_1",
            "arguments": {"raw": "x".repeat(64 * 1024)},
            "result": {"content": [{"text": format!("{{\"receiptId\":\"{receipt_id}\"}}")}]}
        }))
        .unwrap();
        assert!(item.contains_receipt(&receipt_id));
        assert_eq!(item.receipt_ids, vec![receipt_id]);

        let verification = ItemRef::from_value(&json!({
            "type": "commandExecution",
            "id": "command_1",
            "command": "cargo test -- --token=must-not-appear",
            "status": "completed",
            "exitCode": 0
        }))
        .unwrap();
        assert_eq!(verification.safe_evidence[0]["label"], "cargo test");
        assert!(
            !serde_json::to_string(&verification.safe_evidence)
                .unwrap()
                .contains("must-not-appear")
        );

        let unrelated = ItemRef::from_value(&json!({
            "type": "commandExecution",
            "id": "command_2",
            "command": "cargo testing",
            "status": "completed"
        }))
        .unwrap();
        assert!(unrelated.safe_evidence.is_empty());
    }

    #[test]
    fn user_objective_projection_is_scalar_bounded() {
        let item = ItemRef::from_value(&json!({
            "type": "userMessage",
            "id": "item_1",
            "content": [{"type": "text", "text": "界".repeat(4096)}]
        }))
        .unwrap();
        let text = item.safe_evidence[0]["text"].as_str().unwrap();
        assert_eq!(text.chars().count(), MAX_PROJECTED_OBJECTIVE_SCALARS);
    }

    #[test]
    fn resume_snapshot_keeps_active_settings() {
        let snapshot = parse_resume_snapshot(&json!({
            "thread": {"id": "thread", "status": "active", "turns": []},
            "model": "gpt-5",
            "reasoningEffort": "high",
            "cwd": "/workspace",
            "approvalPolicy": "on-request",
            "sandbox": {"type": "workspaceWrite", "writableRoots": ["/workspace"]}
        }))
        .unwrap();

        assert_eq!(snapshot.model.as_deref(), Some("gpt-5"));
        assert_eq!(snapshot.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(snapshot.cwd.as_deref(), Some("/workspace"));
        assert_eq!(snapshot.approval_policy, Some(json!("on-request")));
        assert_eq!(
            snapshot.sandbox,
            Some(json!({"type": "workspaceWrite", "writableRoots": ["/workspace"]}))
        );
    }

    #[test]
    fn resume_snapshot_rejects_oversized_settings() {
        let error = parse_resume_snapshot(&json!({
            "thread": {"id": "thread", "status": "active"},
            "cwd": "x".repeat(16 * 1024 + 1)
        }))
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::ThreadSnapshotTooLarge);
    }
}
