use crate::checkpoint::CompactionIntent;
use crate::error::{Error, ErrorCode, Result};
use crate::metadata::BoundInvocation;
use crate::orchestrator::Orchestrator;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{info, warn};

const MAX_STDIN_MESSAGE_BYTES: usize = 1024 * 1024;
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Debug, Deserialize)]
struct RpcRequest {
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Deserialize)]
struct ToolCallParams {
    name: String,
    #[serde(default)]
    arguments: Value,
    #[serde(default, rename = "_meta")]
    meta: Value,
}

pub async fn serve() -> Result<()> {
    let orchestrator = Arc::new(Orchestrator::new()?);
    let recovered = orchestrator.recover_nonterminal_journals().await?;
    if recovered > 0 {
        warn!(
            recovered,
            "recovered stale agentic-compact transitions conservatively"
        );
    }

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut stdout = tokio::io::stdout();
    let mut line = String::new();

    loop {
        line.clear();
        let bytes = reader.read_line(&mut line).await?;
        if bytes == 0 {
            break;
        }
        if bytes > MAX_STDIN_MESSAGE_BYTES {
            write_response(
                &mut stdout,
                json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": {"code": -32600, "message": "MCP message exceeds 1 MiB"}
                }),
            )
            .await?;
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: RpcRequest = match serde_json::from_str(trimmed) {
            Ok(request) => request,
            Err(error) => {
                write_response(
                    &mut stdout,
                    json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": {"code": -32700, "message": format!("invalid JSON: {error}")}
                    }),
                )
                .await?;
                continue;
            }
        };

        let Some(id) = request.id.clone() else {
            handle_notification(&request.method);
            continue;
        };
        let response = handle_request(Arc::clone(&orchestrator), request).await;
        let envelope = match response {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(error) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": rpc_error_code(error.code),
                    "message": error.to_string(),
                    "data": {
                        "reasonCode": error.code.as_str(),
                        "component": error.component,
                        "retryable": error.retryable
                    }
                }
            }),
        };
        write_response(&mut stdout, envelope).await?;
    }

    info!("agentic-compact MCP server stopped");
    Ok(())
}

async fn handle_request(orchestrator: Arc<Orchestrator>, request: RpcRequest) -> Result<Value> {
    match request.method.as_str() {
        "initialize" => {
            let protocol_version = request
                .params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_PROTOCOL_VERSION);
            Ok(json!({
                "protocolVersion": protocol_version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {
                    "name": "agentic-compact",
                    "title": "Agentic Compact",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "instructions": "Use request_compaction only at a stable semantic boundary. After calling it, finish the current turn without starting new tools or subagents."
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tool_list()),
        "tools/call" => handle_tool_call(orchestrator, request.params).await,
        _ => Err(Error::new(
            ErrorCode::InvalidRequest,
            format!("unsupported MCP method: {}", request.method),
        )
        .component("mcp")),
    }
}

async fn handle_tool_call(orchestrator: Arc<Orchestrator>, params: Value) -> Result<Value> {
    let params: ToolCallParams = serde_json::from_value(params).map_err(|error| {
        Error::new(
            ErrorCode::InvalidRequest,
            format!("invalid tools/call parameters: {error}"),
        )
        .component("mcp")
    })?;
    let bound = match BoundInvocation::from_meta(&params.meta) {
        Ok(bound) => bound,
        Err(error) => return Ok(tool_error(error)),
    };

    match params.name.as_str() {
        "status" | "agentic_compact.status" => {
            if !params.arguments.is_null()
                && params
                    .arguments
                    .as_object()
                    .is_none_or(|object| !object.is_empty())
            {
                return Ok(tool_error(
                    Error::invalid("status does not accept arguments").component("mcp"),
                ));
            }
            match orchestrator.status(&bound).await {
                Ok(status) => tool_success(serde_json::to_value(status)?),
                Err(error) => Ok(tool_error(error)),
            }
        }
        "request_compaction" | "agentic_compact.request_compaction" => {
            let intent: CompactionIntent = match serde_json::from_value(params.arguments) {
                Ok(intent) => intent,
                Err(error) => {
                    return Ok(tool_error(
                        Error::new(
                            ErrorCode::InvalidRequest,
                            format!("invalid request_compaction arguments: {error}"),
                        )
                        .component("mcp"),
                    ));
                }
            };
            let intent = match intent.validate() {
                Ok(intent) => intent,
                Err(error) => return Ok(tool_error(error)),
            };
            match orchestrator.schedule(bound, intent).await {
                Ok(scheduled) => tool_success(serde_json::to_value(scheduled)?),
                Err(error) => Ok(tool_error(error)),
            }
        }
        _ => Ok(tool_error(
            Error::new(
                ErrorCode::InvalidRequest,
                format!("unknown tool: {}", params.name),
            )
            .component("mcp"),
        )),
    }
}

fn tool_list() -> Value {
    json!({
        "tools": [
            {
                "name": "status",
                "title": "Agentic Compact Status",
                "description": "Read whether same-thread agentic compaction is currently safe and available. This tool has no side effects.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                },
                "outputSchema": status_output_schema()
            },
            {
                "name": "request_compaction",
                "title": "Request Agentic Context Compaction",
                "description": "Schedule Codex-native context compaction after the current turn completes, inject a bounded checkpoint, then continue in the same thread. Call only after active tests, commands, and subagents have finished. End the current turn immediately after this call.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "preserve": {
                            "type": "array",
                            "description": "At most four short invariants that must survive compaction.",
                            "items": {"type": "string", "minLength": 1, "maxLength": 96},
                            "maxItems": 4
                        },
                        "next_action": {
                            "type": "string",
                            "description": "One directly executable next action after the checkpoint is restored.",
                            "minLength": 1,
                            "maxLength": 180
                        }
                    },
                    "required": ["preserve", "next_action"],
                    "additionalProperties": false
                },
                "outputSchema": request_output_schema()
            }
        ]
    })
}

fn status_output_schema() -> Value {
    json!({
        "type": "object",
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "version": {"type": "string"},
                    "mode": {"type": "string"},
                    "threadIdBound": {"type": "boolean"},
                    "transitionPending": {"type": "boolean"},
                    "cooldownTurnsRemaining": {"type": "integer", "minimum": 0},
                    "activeContextTokens": {"type": ["integer", "null"]},
                    "autoCompactLimit": {"type": ["integer", "null"]},
                    "lastTransition": {
                        "oneOf": [
                            {"type": "null"},
                            {
                                "type": "object",
                                "properties": {
                                    "checkpointId": {"type": "string"},
                                    "completedRegularTurnsAgo": {"type": "integer", "minimum": 0}
                                },
                                "required": ["checkpointId", "completedRegularTurnsAgo"],
                                "additionalProperties": false
                            }
                        ]
                    },
                    "guards": {
                        "type": "object",
                        "properties": {
                            "rootThread": {"type": "boolean"},
                            "noActiveDescendants": {"type": "boolean"},
                            "sharedAppServer": {"type": "boolean"},
                            "emptyContinuation": {"type": "boolean"}
                        },
                        "required": [
                            "rootThread",
                            "noActiveDescendants",
                            "sharedAppServer",
                            "emptyContinuation"
                        ],
                        "additionalProperties": false
                    },
                    "reasonCode": {"type": ["string", "null"]}
                },
                "required": [
                    "version",
                    "mode",
                    "threadIdBound",
                    "transitionPending",
                    "cooldownTurnsRemaining",
                    "activeContextTokens",
                    "autoCompactLimit",
                    "lastTransition",
                    "guards",
                    "reasonCode"
                ],
                "additionalProperties": false
            },
            error_output_schema()
        ]
    })
}

fn request_output_schema() -> Value {
    json!({
        "type": "object",
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "status": {"const": "scheduled_after_turn"},
                    "receiptId": {"type": "string"},
                    "checkpointId": {"type": "string"}
                },
                "required": ["status", "receiptId", "checkpointId"],
                "additionalProperties": false
            },
            error_output_schema()
        ]
    })
}

fn error_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "status": {"const": "rejected"},
            "reasonCode": {"type": "string"},
            "message": {"type": "string"},
            "retryable": {"type": "boolean"}
        },
        "required": ["status", "reasonCode", "message", "retryable"],
        "additionalProperties": false
    })
}

fn tool_success(value: Value) -> Result<Value> {
    Ok(json!({
        "content": [{"type": "text", "text": serde_json::to_string(&value)?}],
        "structuredContent": value,
        "isError": false
    }))
}

fn tool_error(error: Error) -> Value {
    let value = json!({
        "status": "rejected",
        "reasonCode": error.code.as_str(),
        "message": error.to_string(),
        "retryable": error.retryable
    });
    json!({
        "content": [{"type": "text", "text": value.to_string()}],
        "structuredContent": value,
        "isError": true
    })
}

fn handle_notification(method: &str) {
    match method {
        "notifications/initialized" | "notifications/cancelled" => {}
        _ => tracing::debug!(method, "ignored MCP notification"),
    }
}

async fn write_response(stdout: &mut tokio::io::Stdout, value: Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(&value)?;
    bytes.push(b'\n');
    stdout.write_all(&bytes).await?;
    stdout.flush().await?;
    Ok(())
}

fn rpc_error_code(code: ErrorCode) -> i64 {
    match code {
        ErrorCode::InvalidRequest | ErrorCode::InvalidMetadata | ErrorCode::MetadataMismatch => {
            -32602
        }
        _ => -32000,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_list_is_bounded() {
        let value = tool_list();
        assert_eq!(value["tools"].as_array().unwrap().len(), 2);
        assert!(value.to_string().len() < 8_192);
        for tool in value["tools"].as_array().unwrap() {
            assert_eq!(tool["outputSchema"]["oneOf"][1], error_output_schema());
        }
    }
}
