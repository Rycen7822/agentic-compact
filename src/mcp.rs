use crate::checkpoint::CompactionIntent;
use crate::error::{ErrorCode, Result};
use crate::metadata::BoundInvocation;
use crate::orchestrator::Orchestrator;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{info, warn};

const MAX_STDIN_MESSAGE_BYTES: usize = 1024 * 1024;
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";
const INITIALIZE_INSTRUCTIONS: &str = "Call request_compaction only after a phase has conclusively ended, substantial work remains, and no command, test, approval, file change, or subagent is active. If scheduled, end the turn immediately; if rejected, continue without retrying in that turn.";
const TOOL_DESCRIPTION: &str = "At a settled phase boundary, schedule Codex-native same-thread compaction, inject bounded continuity state, and continue automatically. Do not call during investigation, editing, testing, uncertainty, or soon after another compaction. If scheduled, end this turn immediately.";
const PRESERVE_DESCRIPTION: &str = "Zero to four short facts the host cannot infer, ordered as: decisive conclusion or root cause; ruled-out route or invariant; interface or behavior constraint; unresolved risk or verification obligation.";
const NEXT_ACTION_DESCRIPTION: &str =
    "One concrete next step that remains valid after checking the current workspace.";
const INVALID_JSON_RPC_MESSAGE: &str = "Invalid JSON-RPC request.";
const MESSAGE_TOO_LARGE: &str = "MCP message exceeds the configured size limit.";
const UNSUPPORTED_METHOD_MESSAGE: &str = "Unsupported MCP method.";
const UNKNOWN_TOOL_MESSAGE: &str = "Unknown MCP tool.";

struct RpcFailure {
    code: i64,
    message: &'static str,
}

type RpcResult = std::result::Result<Value, RpcFailure>;

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
                rpc_error_envelope(Value::Null, -32600, MESSAGE_TOO_LARGE),
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
            Err(_) => {
                write_response(
                    &mut stdout,
                    rpc_error_envelope(Value::Null, -32700, INVALID_JSON_RPC_MESSAGE),
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
            Err(error) => rpc_error_envelope(id, error.code, error.message),
        };
        write_response(&mut stdout, envelope).await?;
    }

    info!("agentic-compact MCP server stopped");
    Ok(())
}

async fn handle_request(orchestrator: Arc<Orchestrator>, request: RpcRequest) -> RpcResult {
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
                "instructions": INITIALIZE_INSTRUCTIONS
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tool_list()),
        "tools/call" => handle_tool_call(orchestrator, request.params).await,
        _ => Err(RpcFailure {
            code: -32602,
            message: UNSUPPORTED_METHOD_MESSAGE,
        }),
    }
}

async fn handle_tool_call(orchestrator: Arc<Orchestrator>, params: Value) -> RpcResult {
    let params: ToolCallParams = serde_json::from_value(params).map_err(|_| RpcFailure {
        code: -32602,
        message: INVALID_JSON_RPC_MESSAGE,
    })?;
    let bound = match BoundInvocation::from_meta(&params.meta) {
        Ok(bound) => bound,
        Err(error) => return Ok(tool_rejected(error.code, error.code.model_message())),
    };

    match params.name.as_str() {
        "request_compaction" | "agentic_compact.request_compaction" => {
            let intent: CompactionIntent = match serde_json::from_value(params.arguments) {
                Ok(intent) => intent,
                Err(_) => {
                    return Ok(tool_rejected(
                        ErrorCode::InvalidRequest,
                        ErrorCode::InvalidRequest.model_message(),
                    ));
                }
            };
            let intent = match intent.validate() {
                Ok(intent) => intent,
                Err(error) => return Ok(tool_rejected(error.code, error.code.model_message())),
            };
            match orchestrator.schedule(bound, intent).await {
                Ok(scheduled) => Ok(tool_scheduled(scheduled.receipt_id)),
                Err(error) => Ok(tool_rejected(error.code, error.code.model_message())),
            }
        }
        _ => Ok(tool_rejected(
            ErrorCode::InvalidRequest,
            UNKNOWN_TOOL_MESSAGE,
        )),
    }
}

fn tool_list() -> Value {
    json!({
        "tools": [
            {
                "name": "request_compaction",
                "title": "Request Agentic Context Compaction",
                "description": TOOL_DESCRIPTION,
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "preserve": {
                            "type": "array",
                            "description": PRESERVE_DESCRIPTION,
                            "items": {"type": "string", "minLength": 1, "maxLength": 96},
                            "maxItems": 4
                        },
                        "next_action": {
                            "type": "string",
                            "description": NEXT_ACTION_DESCRIPTION,
                            "minLength": 1,
                            "maxLength": 180
                        }
                    },
                    "required": ["next_action"],
                    "additionalProperties": false
                },
                "outputSchema": request_output_schema()
            }
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
                    "status": {"const": "scheduled_after_turn"}
                },
                "required": ["status"],
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
            "retryable": {"const": false}
        },
        "required": ["status", "reasonCode", "message", "retryable"],
        "additionalProperties": false
    })
}

fn tool_scheduled(receipt_id: String) -> Value {
    json!({
        "content": [],
        "structuredContent": {"status": "scheduled_after_turn"},
        "_meta": {"agenticCompact": {"receiptId": receipt_id}},
        "isError": false
    })
}

fn tool_rejected(code: ErrorCode, message: &'static str) -> Value {
    let value = json!({
        "status": "rejected",
        "reasonCode": code.as_str(),
        "message": message,
        "retryable": false
    });
    json!({
        "content": [{"type": "text", "text": value.to_string()}],
        "structuredContent": value,
        "isError": !code.is_expected_rejection()
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

fn rpc_error_envelope(id: Value, code: i64, message: &'static str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
            "data": {
                "reasonCode": "invalid_request",
                "retryable": false
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_list_matches_the_single_tool_contract() {
        let value = tool_list();
        let tools = value["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert!(serde_json::to_vec(tools).unwrap().len() <= 4_096);
        assert_eq!(tools[0]["name"], "request_compaction");
        assert_eq!(tools[0]["title"], "Request Agentic Context Compaction");
        assert_eq!(tools[0]["description"], TOOL_DESCRIPTION);
        assert_eq!(tools[0]["inputSchema"]["required"], json!(["next_action"]));
        assert_eq!(tools[0]["outputSchema"]["oneOf"][1], error_output_schema());
    }

    #[test]
    fn scheduled_result_hides_internal_ids_from_model_content() {
        let result = tool_scheduled("rcpt_0123456789abcdef0123456789abcdef".to_owned());
        assert_eq!(result["content"], json!([]));
        assert_eq!(
            result["structuredContent"],
            json!({"status": "scheduled_after_turn"})
        );
        assert_eq!(
            result["_meta"],
            json!({
                "agenticCompact": {
                    "receiptId": "rcpt_0123456789abcdef0123456789abcdef"
                }
            })
        );
        assert_eq!(result["isError"], false);
        assert!(!result.to_string().contains("checkpointId"));
    }

    #[test]
    fn rejection_serializer_separates_expected_and_hard_failures() {
        let expected = tool_rejected(
            ErrorCode::SharedAppServerUnavailable,
            ErrorCode::SharedAppServerUnavailable.model_message(),
        );
        assert_eq!(expected["isError"], false);
        assert_eq!(expected["structuredContent"]["retryable"], false);
        assert_eq!(
            expected["structuredContent"]["message"],
            ErrorCode::SharedAppServerUnavailable.model_message()
        );

        let hard = tool_rejected(
            ErrorCode::InvalidRequest,
            ErrorCode::InvalidRequest.model_message(),
        );
        assert_eq!(hard["isError"], true);
        assert_eq!(
            serde_json::from_str::<Value>(hard["content"][0]["text"].as_str().unwrap()).unwrap(),
            hard["structuredContent"]
        );
    }

    #[test]
    fn rpc_errors_are_static_and_redacted() {
        let error = rpc_error_envelope(Value::Null, -32700, INVALID_JSON_RPC_MESSAGE);
        assert_eq!(error["error"]["message"], INVALID_JSON_RPC_MESSAGE);
        assert_eq!(
            error["error"]["data"],
            json!({"reasonCode": "invalid_request", "retryable": false})
        );
        assert!(error["error"].get("component").is_none());
    }
}
