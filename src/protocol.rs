use crate::checkpoint::{Evidence, contains_sensitive_text, verification_spec};
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
const MCP_SERVER_ALIASES: &[&str] = &["agentic-compact", "agentic_compact"];
const REQUEST_COMPACTION_TOOL_ALIASES: &[&str] =
    &["request_compaction", "agentic_compact.request_compaction"];
const RECEIPT_PROJECTION_COMPONENT: &str = "receipt_projection";
const ACTIVE_WORK_ITEM_TYPES: &[&str] = &[
    "commandExecution",
    "fileChange",
    "mcpToolCall",
    "dynamicToolCall",
    "collabAgentToolCall",
    "imageGeneration",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnItemsMode {
    Full,
    Lifecycle,
}

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
    pub receipt_id: Option<String>,
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
    RequestCompactionResultInvalid {
        thread_id: String,
        turn_id: String,
    },
    ThreadStatusChanged {
        thread_id: String,
        status: String,
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
    pub fn from_response(response: &Value, include_turns: bool) -> Result<Self> {
        let thread = response.get("thread").unwrap_or(response);
        let object = thread
            .as_object()
            .ok_or_else(|| Error::protocol("thread response must contain an object"))?;
        let id = required_string(object.get("id"), "thread.id")?;
        let parent_thread_id = optional_string(object.get("parentThreadId"));
        let status = parse_thread_status(object.get("status"))?;
        let turns = if include_turns {
            object
                .get("turns")
                .and_then(Value::as_array)
                .ok_or_else(|| Error::protocol("full thread snapshot requires a turns array"))?
                .iter()
                .map(|value| TurnRef::from_value(value, TurnItemsMode::Full))
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
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

    pub fn unique_turn_index(&self, turn_id: &str) -> Result<usize> {
        let mut matches = self
            .turns
            .iter()
            .enumerate()
            .filter(|(_, turn)| turn.id == turn_id)
            .map(|(index, _)| index);
        let index = matches.next().ok_or_else(|| {
            Error::new(
                ErrorCode::RecoveryAmbiguous,
                "turn anchor is missing from the full snapshot",
            )
            .component("protocol")
        })?;
        if matches.next().is_some() {
            return Err(Error::new(
                ErrorCode::RecoveryAmbiguous,
                "turn anchor is duplicated in the full snapshot",
            )
            .component("protocol"));
        }
        Ok(index)
    }

    pub fn unique_turn(&self, turn_id: &str) -> Result<&TurnRef> {
        Ok(&self.turns[self.unique_turn_index(turn_id)?])
    }

    pub fn ensure_exact_last_turn(&self, turn_id: &str) -> Result<&TurnRef> {
        let index = self.unique_turn_index(turn_id)?;
        if index + 1 != self.turns.len() {
            return Err(Error::new(
                ErrorCode::RecoveryAmbiguous,
                "turn anchor is not the exact last turn in the full snapshot",
            )
            .component("protocol"));
        }
        Ok(&self.turns[index])
    }

    pub fn ordered_items_through_turn(
        &self,
        turn_id: &str,
    ) -> Result<impl Iterator<Item = (usize, usize, &ItemRef)>> {
        let last_turn = self.unique_turn_index(turn_id)?;
        Ok(self.turns[..=last_turn]
            .iter()
            .enumerate()
            .flat_map(|(turn_index, turn)| {
                turn.items
                    .iter()
                    .enumerate()
                    .map(move |(item_index, item)| (turn_index, item_index, item))
            }))
    }
}

impl TurnRef {
    fn from_value(value: &Value, mode: TurnItemsMode) -> Result<Self> {
        let object = value
            .as_object()
            .ok_or_else(|| Error::protocol("turn must be an object"))?;
        let id = required_string(object.get("id"), "turn.id")?;
        let status = required_string(object.get("status"), "turn.status")?;
        match (mode, object.get("itemsView")) {
            (TurnItemsMode::Full, None) => {}
            (TurnItemsMode::Full, Some(Value::String(view))) if view == "full" => {}
            (TurnItemsMode::Lifecycle, None) => {}
            (TurnItemsMode::Lifecycle, Some(Value::String(view)))
                if matches!(view.as_str(), "notLoaded" | "summary" | "full") => {}
            (TurnItemsMode::Full, _) => {
                return Err(Error::protocol("turn.itemsView must be full"));
            }
            (TurnItemsMode::Lifecycle, _) => {
                return Err(Error::protocol("turn.itemsView is invalid"));
            }
        }
        let items = object
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::protocol("full turn snapshot requires an items array"))?
            .iter()
            .map(ItemRef::from_value)
            .collect::<Result<Vec<_>>>()?;
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

    pub fn is_completed_pure_compaction(&self) -> bool {
        self.status == "completed"
            && self.items.len() == 1
            && self.items[0].is_allowed_in_compaction_turn()
            && !self.items[0].has_error
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
        let receipt_id =
            if is_request_compaction_call(&item_type, server.as_deref(), tool.as_deref()) {
                project_owned_receipt(object.get("result"))?
            } else {
                None
            };
        let has_error = object.get("error").is_some_and(|value| !value.is_null());
        let safe_evidence = project_safe_evidence(&item_type, object);
        Ok(Self {
            id,
            item_type,
            status,
            server,
            tool,
            receipt_id,
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

    pub fn is_request_compaction_call(&self) -> bool {
        is_request_compaction_call(
            &self.item_type,
            self.server.as_deref(),
            self.tool.as_deref(),
        )
    }
}

pub fn parse_resume_snapshot(response: &Value) -> Result<ResumeSnapshot> {
    Ok(ResumeSnapshot {
        thread: ThreadRef::from_response(response, true)?,
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
    TurnRef::from_value(turn, TurnItemsMode::Lifecycle)
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
                TurnItemsMode::Lifecycle,
            )?,
        }),
        "turn/completed" => {
            let thread_id = required_path_string(params, "/threadId", "threadId")?;
            let value = params
                .get("turn")
                .ok_or_else(|| Error::protocol("turn/completed is missing turn"))?;
            let turn_id = required_path_string(value, "/id", "turn.id")?;
            match TurnRef::from_value(value, TurnItemsMode::Lifecycle) {
                Ok(turn) => Ok(AppEvent::TurnCompleted { thread_id, turn }),
                Err(error) if error.component == RECEIPT_PROJECTION_COMPONENT => {
                    Ok(AppEvent::RequestCompactionResultInvalid { thread_id, turn_id })
                }
                Err(error) => Err(error),
            }
        }
        "item/started" => Ok(AppEvent::ItemStarted {
            thread_id: required_path_string(params, "/threadId", "threadId")?,
            turn_id: required_path_string(params, "/turnId", "turnId")?,
            item: ItemRef::from_value(
                params
                    .get("item")
                    .ok_or_else(|| Error::protocol("item/started is missing item"))?,
            )?,
        }),
        "item/completed" => {
            let thread_id = required_path_string(params, "/threadId", "threadId")?;
            let turn_id = required_path_string(params, "/turnId", "turnId")?;
            let value = params
                .get("item")
                .ok_or_else(|| Error::protocol("item/completed is missing item"))?;
            match ItemRef::from_value(value) {
                Ok(item) => Ok(AppEvent::ItemCompleted {
                    thread_id,
                    turn_id,
                    item,
                }),
                Err(error) if error.component == RECEIPT_PROJECTION_COMPONENT => {
                    Ok(AppEvent::RequestCompactionResultInvalid { thread_id, turn_id })
                }
                Err(error) => Err(error),
            }
        }
        "thread/status/changed" => Ok(AppEvent::ThreadStatusChanged {
            thread_id: required_path_string(params, "/threadId", "threadId")?,
            status: parse_thread_status(params.get("status"))?,
        }),
        _ => Ok(AppEvent::UnknownNotification {
            method: method.to_owned(),
        }),
    }
}

pub fn completed_regular_turns_after(thread: &ThreadRef, turn_id: &str) -> Result<usize> {
    let index = thread.unique_turn_index(turn_id)?;
    Ok(thread.turns[index + 1..]
        .iter()
        .filter(|turn| turn.is_completed_regular())
        .count())
}

pub fn completed_regular_turns_since_latest_compaction(
    thread: &ThreadRef,
    source_turn_id: &str,
) -> Result<Option<usize>> {
    let source_index = thread.unique_turn_index(source_turn_id)?;
    let Some(compact_index) = thread.turns[..source_index]
        .iter()
        .rposition(|turn| turn.status == "completed" && turn.is_compaction())
    else {
        return Ok(None);
    };
    Ok(Some(
        thread.turns[compact_index + 1..source_index]
            .iter()
            .filter(|turn| turn.is_completed_regular())
            .count(),
    ))
}

pub fn has_active_work_through_turn(thread: &ThreadRef, source_turn_id: &str) -> Result<bool> {
    Ok(thread
        .ordered_items_through_turn(source_turn_id)?
        .any(|(_, _, item)| {
            ACTIVE_WORK_ITEM_TYPES.contains(&item.item_type.as_str())
                && item.status.as_deref() == Some("inProgress")
        }))
}

pub fn project_current_window_evidence(
    thread: &ThreadRef,
    source_turn_id: &str,
) -> Result<Evidence> {
    let mut evidence = Evidence::default();
    for (turn_index, _, item) in thread.ordered_items_through_turn(source_turn_id)? {
        if thread.turns[turn_index].status == "completed" && item.item_type == "contextCompaction" {
            evidence.reset_window();
            continue;
        }
        for value in &item.safe_evidence {
            evidence.observe_item(value);
        }
    }
    evidence.normalize();
    Ok(evidence)
}

fn project_safe_evidence(item_type: &str, object: &serde_json::Map<String, Value>) -> Vec<Value> {
    match item_type {
        "userMessage" => {
            let text = bounded_user_text(object.get("content"));
            let sensitive = object
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|entry| entry.get("text").and_then(Value::as_str))
                .any(contains_sensitive_text);
            vec![json!({
                "kind": "user_objective",
                "text": (!sensitive && !text.trim().is_empty()).then_some(text)
            })]
        }
        "fileChange" if object.get("status").and_then(Value::as_str) == Some("completed") => {
            let mut paths = object
                .get("changes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .rev()
                .filter_map(|change| change.get("path").and_then(Value::as_str))
                .filter(|path| !contains_sensitive_text(path))
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
            let item_id = object.get("id").and_then(Value::as_str);
            match (command.and_then(verification_spec), status, item_id) {
                (Some((kind, label)), Some(status @ ("completed" | "failed")), Some(item_id))
                    if !item_id.is_empty() =>
                {
                    vec![json!({
                        "kind": "verification",
                        "itemId": truncate_scalars(item_id, MAX_PROJECTED_ITEM_ID_SCALARS),
                        "verificationKind": kind,
                        "label": label,
                        "status": truncate_scalars(status, MAX_PROJECTED_STATUS_SCALARS),
                        "exitCode": object.get("exitCode").and_then(Value::as_i64)
                    })]
                }
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

fn is_request_compaction_call(item_type: &str, server: Option<&str>, tool: Option<&str>) -> bool {
    item_type == "mcpToolCall"
        && server.is_some_and(|value| MCP_SERVER_ALIASES.contains(&value))
        && tool.is_some_and(|value| REQUEST_COMPACTION_TOOL_ALIASES.contains(&value))
}

fn project_owned_receipt(result: Option<&Value>) -> Result<Option<String>> {
    let Some(result) = result.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let result = result
        .as_object()
        .ok_or_else(|| receipt_projection_error("request_compaction result must be an object"))?;
    let Some(metadata) = result.get("_meta").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let metadata = metadata.as_object().ok_or_else(|| {
        receipt_projection_error("request_compaction result _meta must be an object")
    })?;
    let Some(namespace) = metadata.get("agenticCompact") else {
        return Ok(None);
    };
    let namespace = namespace.as_object().ok_or_else(|| {
        receipt_projection_error("request_compaction agenticCompact metadata must be an object")
    })?;
    if namespace.len() != 1 || !namespace.contains_key("receiptId") {
        return Err(receipt_projection_error(
            "request_compaction agenticCompact metadata has invalid fields",
        ));
    }
    let receipt_id = namespace["receiptId"]
        .as_str()
        .filter(|value| valid_receipt_id(value))
        .ok_or_else(|| receipt_projection_error("request_compaction receiptId is invalid"))?;
    Ok(Some(receipt_id.to_owned()))
}

fn receipt_projection_error(message: &'static str) -> Error {
    Error::protocol(message).component(RECEIPT_PROJECTION_COMPONENT)
}

fn valid_receipt_id(value: &str) -> bool {
    value.len() == 37
        && value.starts_with("rcpt_")
        && value[5..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(turns: Vec<Value>, source_turn_id: &str) -> Evidence {
        let thread = ThreadRef::from_response(
            &json!({"thread": {"id": "thread", "status": "idle", "turns": turns}}),
            true,
        )
        .unwrap();
        project_current_window_evidence(&thread, source_turn_id).unwrap()
    }

    fn changed_file(id: &str, path: &str) -> Value {
        json!({
            "id": id,
            "type": "fileChange",
            "status": "completed",
            "changes": [{"path": path}]
        })
    }

    fn command(id: &str, command: &str, status: &str, exit_code: i64) -> Value {
        json!({
            "id": id,
            "type": "commandExecution",
            "command": command,
            "status": status,
            "exitCode": exit_code,
            "aggregatedOutput": "raw output must not survive"
        })
    }

    fn user_message(id: &str, content: Value) -> Value {
        json!({"id": id, "type": "userMessage", "content": content})
    }

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
    fn full_snapshot_rejects_partial_turn_and_item_shapes() {
        let invalid = [
            json!({"thread": {"id": "thread", "status": "idle"}}),
            json!({"thread": {"id": "thread", "status": "idle", "turns": null}}),
            json!({
                "thread": {
                    "id": "thread",
                    "status": "idle",
                    "turns": [{"id": "turn", "status": "completed"}]
                }
            }),
            json!({
                "thread": {
                    "id": "thread",
                    "status": "idle",
                    "turns": [{"id": "turn", "status": "completed", "items": null}]
                }
            }),
            json!({
                "thread": {
                    "id": "thread",
                    "status": "idle",
                    "turns": [{
                        "id": "turn",
                        "status": "completed",
                        "items": [],
                        "itemsView": null
                    }]
                }
            }),
            json!({
                "thread": {
                    "id": "thread",
                    "status": "idle",
                    "turns": [{
                        "id": "turn",
                        "status": "completed",
                        "items": [],
                        "itemsView": "summary"
                    }]
                }
            }),
            json!({
                "thread": {
                    "id": "thread",
                    "status": "idle",
                    "turns": [{
                        "id": "turn",
                        "status": "completed",
                        "items": [],
                        "itemsView": "notLoaded"
                    }]
                }
            }),
        ];
        for snapshot in invalid {
            assert_eq!(
                ThreadRef::from_response(&snapshot, true).unwrap_err().code,
                ErrorCode::Protocol
            );
        }

        for items_view in [None, Some("full")] {
            let mut turn = json!({"id": "turn", "status": "completed", "items": []});
            if let Some(items_view) = items_view {
                turn["itemsView"] = json!(items_view);
            }
            let snapshot = json!({
                "thread": {"id": "thread", "status": "idle", "turns": [turn]}
            });
            assert!(ThreadRef::from_response(&snapshot, true).is_ok());
        }

        let shallow = ThreadRef::from_response(
            &json!({
                "thread": {
                    "id": "thread",
                    "status": "active",
                    "turns": [{"itemsView": "summary"}]
                }
            }),
            false,
        )
        .unwrap();
        assert!(shallow.turns.is_empty());
    }

    #[test]
    fn lifecycle_turn_views_are_bounded_but_not_full_decisions() {
        for view in ["notLoaded", "summary", "full"] {
            let turn = json!({
                "id": "turn",
                "status": "completed",
                "items": [],
                "itemsView": view
            });
            assert!(TurnRef::from_value(&turn, TurnItemsMode::Lifecycle).is_ok());
            if view == "full" {
                assert!(TurnRef::from_value(&turn, TurnItemsMode::Full).is_ok());
            } else {
                assert!(TurnRef::from_value(&turn, TurnItemsMode::Full).is_err());
            }
        }

        for invalid in [
            json!({"id": "turn", "status": "completed", "itemsView": "summary"}),
            json!({
                "id": "turn",
                "status": "completed",
                "items": [],
                "itemsView": null
            }),
            json!({
                "id": "turn",
                "status": "completed",
                "items": [],
                "itemsView": "unknown"
            }),
        ] {
            assert_eq!(
                TurnRef::from_value(&invalid, TurnItemsMode::Lifecycle)
                    .unwrap_err()
                    .code,
                ErrorCode::Protocol
            );
        }
    }

    #[test]
    fn native_compaction_cooldown_counts_only_regular_turns_before_source() {
        let thread = ThreadRef::from_response(
            &json!({"thread": {
                "id": "thread",
                "status": "active",
                "turns": [
                    {"id": "old-compact", "status": "completed", "items": [
                        {"id": "old", "type": "contextCompaction", "status": "completed"}
                    ]},
                    {"id": "ignored", "status": "failed", "items": []},
                    {"id": "latest-compact", "status": "completed", "items": [
                        {"id": "latest", "type": "contextCompaction", "status": "completed"}
                    ]},
                    {"id": "one", "status": "completed", "items": []},
                    {"id": "two", "status": "completed", "items": []},
                    {"id": "source", "status": "inProgress", "items": []}
                ]
            }}),
            true,
        )
        .unwrap();
        assert_eq!(
            completed_regular_turns_since_latest_compaction(&thread, "source").unwrap(),
            Some(2)
        );

        let no_compaction = ThreadRef {
            turns: thread.turns[3..].to_vec(),
            ..thread
        };
        assert_eq!(
            completed_regular_turns_since_latest_compaction(&no_compaction, "source").unwrap(),
            None
        );
    }

    #[test]
    fn active_work_matches_only_the_six_owned_in_progress_types() {
        for item_type in ACTIVE_WORK_ITEM_TYPES {
            let thread = ThreadRef::from_response(
                &json!({"thread": {
                    "id": "thread",
                    "status": "idle",
                    "turns": [{"id": "source", "status": "completed", "items": [
                        {"id": "work", "type": item_type, "status": "inProgress"}
                    ]}]
                }}),
                true,
            )
            .unwrap();
            assert!(has_active_work_through_turn(&thread, "source").unwrap());
        }

        for (item_type, status) in [
            ("commandExecution", "completed"),
            ("fileChange", "failed"),
            ("futureWorkType", "inProgress"),
            ("mcpToolCall", "unknown"),
        ] {
            let thread = ThreadRef::from_response(
                &json!({"thread": {
                    "id": "thread",
                    "status": "idle",
                    "turns": [{"id": "source", "status": "completed", "items": [
                        {"id": "work", "type": item_type, "status": status}
                    ]}]
                }}),
                true,
            )
            .unwrap();
            assert!(!has_active_work_through_turn(&thread, "source").unwrap());
        }
    }

    #[test]
    fn evidence_uses_the_last_completed_compaction_item_position() {
        let projected = evidence(
            vec![
                json!({
                    "id": "history",
                    "status": "completed",
                    "items": [
                        user_message("objective", json!([{"type": "text", "text": "ship v0.2"}])),
                        changed_file("pre", "src/pre.rs"),
                        command("verify", "cargo test --workspace", "completed", 0),
                        json!({"id": "compact-one", "type": "contextCompaction"}),
                        changed_file("middle", "src/middle.rs"),
                        command("verify", "cargo check", "completed", 0),
                        json!({"id": "compact-two", "type": "contextCompaction"}),
                        changed_file("after", "src/after.rs"),
                        command("verify", "cargo check", "completed", 0)
                    ]
                }),
                json!({
                    "id": "source",
                    "status": "completed",
                    "items": [
                        changed_file("source-file", "src/source.rs"),
                        command("verify", "cargo clippy --all-targets", "failed", 1)
                    ]
                }),
            ],
            "source",
        );

        assert_eq!(projected.last_user_objective.as_deref(), Some("ship v0.2"));
        assert_eq!(
            projected.window_changed_files,
            vec!["src/after.rs", "src/source.rs"]
        );
        assert_eq!(projected.verification.len(), 1);
        assert_eq!(projected.verification[0].item_id, "verify");
        assert_eq!(projected.verification[0].kind, "lint");
        assert_eq!(projected.verification[0].status, "failed");
        assert_eq!(projected.verification[0].exit_code, Some(1));
    }

    #[test]
    fn every_later_user_message_invalidates_the_previous_objective() {
        let stale = user_message(
            "old",
            json!([{"type": "text", "text": "keep the old objective"}]),
        );
        for latest in [
            user_message("empty", json!([])),
            user_message(
                "sensitive",
                json!([{
                    "type": "text",
                    "text": "Authorization: Bearer abcdefghijklmnopqrstuvwxyz"
                }]),
            ),
            user_message("image", json!([{"type": "image", "url": "ignored"}])),
            user_message("nontext", json!([{"type": "text", "text": 7}])),
        ] {
            let projected = evidence(
                vec![json!({
                    "id": "source",
                    "status": "completed",
                    "items": [stale.clone(), latest]
                })],
                "source",
            );
            assert!(projected.last_user_objective.is_none());
        }

        let projected = evidence(
            vec![json!({
                "id": "source",
                "status": "completed",
                "items": [
                    stale,
                    user_message("new", json!([{"type": "text", "text": "new objective"}]))
                ]
            })],
            "source",
        );
        assert_eq!(
            projected.last_user_objective.as_deref(),
            Some("new objective")
        );
    }

    #[test]
    fn verification_allowlist_has_fixed_kinds_and_prefix_boundaries() {
        for (command, expected) in [
            ("cargo test --workspace", ("test", "cargo test")),
            ("cargo check", ("check", "cargo check")),
            ("cargo clippy --all-targets", ("lint", "cargo clippy")),
            ("pytest -q", ("test", "pytest")),
            ("python -m pytest tests", ("test", "python -m pytest")),
            ("npm test", ("test", "npm test")),
            ("npm run test -- --run", ("test", "npm run test")),
            ("pnpm test", ("test", "pnpm test")),
            ("yarn test", ("test", "yarn test")),
            ("go test ./...", ("test", "go test")),
            ("make test", ("test", "make test")),
            ("cmake --build build", ("build", "cmake --build")),
        ] {
            assert_eq!(verification_spec(command), Some(expected));
        }
        for command in [
            "cargo testing",
            "cargo testx",
            "pytester",
            "npm test-run",
            "cmake --builder",
            "echo cargo test",
        ] {
            assert_eq!(verification_spec(command), None);
        }
    }

    #[test]
    fn window_evidence_is_terminal_deduplicated_bounded_and_redacted() {
        let mut items = (0..70)
            .map(|index| changed_file(&format!("file-{index}"), &format!("src/{index}.rs")))
            .collect::<Vec<_>>();
        items.extend((0..20).map(|index| {
            command(
                &format!("verification-{index}"),
                "cargo test -- --token=must-not-survive",
                "completed",
                0,
            )
        }));
        items.extend([
            command("verification-19", "cargo check", "failed", 1),
            command("running", "cargo test", "inProgress", 0),
            command("declined", "cargo test", "declined", 0),
            command("cancelled", "cargo test", "cancelled", 0),
            command("unknown-status", "cargo test", "unknown", 0),
            command("unknown-command", "cargo testing", "completed", 0),
        ]);
        let projected = evidence(
            vec![json!({
                "id": "source",
                "status": "completed",
                "items": items
            })],
            "source",
        );

        assert_eq!(projected.window_changed_files.len(), 64);
        assert_eq!(projected.window_changed_files[0], "src/6.rs");
        assert_eq!(projected.window_changed_files[63], "src/69.rs");
        assert_eq!(projected.verification.len(), 16);
        assert_eq!(projected.verification[0].item_id, "verification-4");
        let latest = projected.verification.last().unwrap();
        assert_eq!(latest.item_id, "verification-19");
        assert_eq!(latest.kind, "check");
        assert_eq!(latest.status, "failed");
        let encoded = serde_json::to_string(&projected).unwrap();
        assert!(!encoded.contains("must-not-survive"));
        assert!(!encoded.contains("raw output"));
    }

    #[test]
    fn token_usage_notifications_have_no_runtime_projection() {
        let event = parse_notification(&json!({
            "method": "thread/tokenUsage/updated",
            "params": {
                "threadId": "thread",
                "turnId": "turn",
                "tokenUsage": {"last": {"inputTokens": 123}}
            }
        }))
        .unwrap();
        assert!(
            matches!(event, AppEvent::UnknownNotification { method } if method == "thread/tokenUsage/updated")
        );
    }

    #[test]
    fn turn_anchors_are_unique_exact_and_ordered() {
        let mut thread = ThreadRef::from_response(
            &json!({"thread": {
                "id": "thread",
                "status": "idle",
                "turns": [
                    {
                        "id": "first",
                        "status": "completed",
                        "items": [
                            {"id": "first-1", "type": "reasoning"},
                            {"id": "first-2", "type": "agentMessage"}
                        ]
                    },
                    {
                        "id": "last",
                        "status": "completed",
                        "items": [{"id": "last-1", "type": "agentMessage"}]
                    }
                ]
            }}),
            true,
        )
        .unwrap();
        assert_eq!(thread.unique_turn_index("last").unwrap(), 1);
        assert!(thread.ensure_exact_last_turn("last").is_ok());
        assert!(thread.ensure_exact_last_turn("first").is_err());
        assert!(thread.unique_turn("missing").is_err());
        let ordered = thread
            .ordered_items_through_turn("last")
            .unwrap()
            .map(|(turn, item, value)| (turn, item, value.id.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            ordered,
            vec![(0, 0, "first-1"), (0, 1, "first-2"), (1, 0, "last-1")]
        );

        thread.turns.push(thread.turns[1].clone());
        assert!(thread.unique_turn("last").is_err());
        assert!(thread.ensure_exact_last_turn("last").is_err());
        assert!(thread.ordered_items_through_turn("last").is_err());
    }

    #[test]
    fn pure_compaction_requires_one_error_free_item_without_item_status() {
        let pure = TurnRef::from_value(
            &json!({
                "id": "compact",
                "status": "completed",
                "items": [{"id": "item", "type": "contextCompaction"}]
            }),
            TurnItemsMode::Full,
        )
        .unwrap();
        assert!(pure.is_completed_pure_compaction());

        for invalid in [
            json!({
                "id": "compact",
                "status": "inProgress",
                "items": [{"id": "item", "type": "contextCompaction"}]
            }),
            json!({
                "id": "compact",
                "status": "completed",
                "items": [{"id": "item", "type": "contextCompaction", "error": {}}]
            }),
            json!({
                "id": "compact",
                "status": "completed",
                "items": [
                    {"id": "item", "type": "contextCompaction"},
                    {"id": "extra", "type": "agentMessage"}
                ]
            }),
        ] {
            assert!(
                !TurnRef::from_value(&invalid, TurnItemsMode::Full)
                    .unwrap()
                    .is_completed_pure_compaction()
            );
        }
    }

    #[test]
    fn item_projection_keeps_only_bounded_safe_fields() {
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

    fn request_item(server: &str, tool: &str, result: Option<Value>) -> Result<ItemRef> {
        let mut value = json!({
            "type": "mcpToolCall",
            "id": "item_1",
            "status": "completed",
            "server": server,
            "tool": tool
        });
        if let Some(result) = result {
            value["result"] = result;
        }
        ItemRef::from_value(&value)
    }

    #[test]
    fn receipt_projection_accepts_only_owned_metadata_for_both_aliases() {
        let receipt_id = format!("rcpt_{}", "a".repeat(32));
        let result = json!({
            "other": true,
            "_meta": {
                "other": {"ignored": true},
                "agenticCompact": {"receiptId": receipt_id}
            }
        });
        for server in MCP_SERVER_ALIASES {
            for tool in REQUEST_COMPACTION_TOOL_ALIASES {
                let item = request_item(server, tool, Some(result.clone())).unwrap();
                assert!(item.is_request_compaction_call());
                assert_eq!(item.receipt_id.as_deref(), Some(receipt_id.as_str()));
            }
        }

        for unbound in [
            None,
            Some(Value::Null),
            Some(json!({})),
            Some(json!({"_meta": null})),
            Some(json!({"_meta": {"other": true}})),
        ] {
            let item = request_item("agentic-compact", "request_compaction", unbound).unwrap();
            assert!(item.receipt_id.is_none());
        }
    }

    #[test]
    fn receipt_projection_rejects_malformed_owned_metadata() {
        let uppercase = format!("rcpt_{}A", "a".repeat(31));
        let short = format!("rcpt_{}", "a".repeat(31));
        let long = format!("rcpt_{}", "a".repeat(33));
        let invalid_results = [
            json!("not-an-object"),
            json!({"_meta": []}),
            json!({"_meta": {"agenticCompact": null}}),
            json!({"_meta": {"agenticCompact": []}}),
            json!({"_meta": {"agenticCompact": {}}}),
            json!({"_meta": {"agenticCompact": {"receiptId": null}}}),
            json!({"_meta": {"agenticCompact": {"receiptId": uppercase}}}),
            json!({"_meta": {"agenticCompact": {"receiptId": short}}}),
            json!({"_meta": {"agenticCompact": {"receiptId": long}}}),
            json!({
                "_meta": {
                    "agenticCompact": {
                        "receiptId": format!("rcpt_{}", "a".repeat(32)),
                        "unknown": true
                    }
                }
            }),
        ];
        for result in invalid_results {
            let error =
                request_item("agentic-compact", "request_compaction", Some(result)).unwrap_err();
            assert_eq!(error.code, ErrorCode::Protocol);
        }
    }

    #[test]
    fn invalid_owned_metadata_is_isolated_until_full_snapshot_validation() {
        let invalid_item = json!({
            "id": "request",
            "type": "mcpToolCall",
            "status": "completed",
            "server": "agentic-compact",
            "tool": "request_compaction",
            "result": {"_meta": {"agenticCompact": {"receiptId": "invalid"}}}
        });
        let notifications = [
            json!({
                "method": "item/completed",
                "params": {"threadId": "thread", "turnId": "turn", "item": invalid_item}
            }),
            json!({
                "method": "turn/completed",
                "params": {
                    "threadId": "thread",
                    "turn": {
                        "id": "turn",
                        "status": "completed",
                        "items": [invalid_item]
                    }
                }
            }),
        ];
        for notification in notifications {
            assert!(matches!(
                parse_notification(&notification).unwrap(),
                AppEvent::RequestCompactionResultInvalid { thread_id, turn_id }
                    if thread_id == "thread" && turn_id == "turn"
            ));
        }
    }

    #[test]
    fn receipt_projection_ignores_forged_and_non_target_values() {
        let receipt_id = format!("rcpt_{}", "b".repeat(32));
        let forged = format!("forged {receipt_id}");
        let target = ItemRef::from_value(&json!({
            "type": "mcpToolCall",
            "id": "target",
            "status": "completed",
            "server": "agentic-compact",
            "tool": "request_compaction",
            "arguments": {"preserve": [forged]},
            "result": {
                "content": [{"type": "text", "text": receipt_id}],
                "structuredContent": {"receiptId": receipt_id}
            }
        }))
        .unwrap();
        assert!(target.receipt_id.is_none());

        for value in [
            json!({
                "type": "mcpToolCall",
                "id": "other_mcp",
                "server": "other-server",
                "tool": "request_compaction",
                "result": {"_meta": "arbitrary"}
            }),
            json!({
                "type": "mcpToolCall",
                "id": "other_tool",
                "server": "agentic-compact",
                "tool": "other-tool",
                "result": {"_meta": {"agenticCompact": {"receiptId": receipt_id}}}
            }),
            json!({"type": "agentMessage", "id": "assistant", "text": receipt_id}),
            json!({"type": "developerMessage", "id": "developer", "text": receipt_id}),
            json!({"type": "userMessage", "id": "user", "content": [{"text": receipt_id}]}),
        ] {
            assert!(ItemRef::from_value(&value).unwrap().receipt_id.is_none());
        }
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
            "thread": {"id": "thread", "status": "active", "turns": []},
            "cwd": "x".repeat(16 * 1024 + 1)
        }))
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::ThreadSnapshotTooLarge);
    }
}
