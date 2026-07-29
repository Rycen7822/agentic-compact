use crate::error::{Error, ErrorCode, Result};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const THREAD_ID_KEY: &str = "threadId";
const TURN_METADATA_KEY: &str = "x-codex-turn-metadata";
const MAX_TURN_METADATA_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundInvocation {
    pub thread_id: String,
    pub turn_id: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TurnMetadata {
    thread_id: String,
    turn_id: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
}

impl BoundInvocation {
    pub fn from_meta(meta: &Value) -> Result<Self> {
        let object = meta.as_object().ok_or_else(|| {
            Error::new(
                ErrorCode::InvalidMetadata,
                "MCP request _meta must be an object",
            )
            .component("metadata")
        })?;
        let outer_thread_id = object
            .get(THREAD_ID_KEY)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::InvalidMetadata,
                    "MCP request _meta.threadId is missing",
                )
                .component("metadata")
            })?;

        let metadata_value = object.get(TURN_METADATA_KEY).ok_or_else(|| {
            Error::new(
                ErrorCode::InvalidMetadata,
                "MCP request _meta.x-codex-turn-metadata is missing",
            )
            .component("metadata")
        })?;
        let parsed = parse_turn_metadata(metadata_value)?;

        validate_identifier("threadId", &outer_thread_id)?;
        validate_identifier("thread_id", &parsed.thread_id)?;
        validate_identifier("turn_id", &parsed.turn_id)?;
        if outer_thread_id != parsed.thread_id {
            return Err(Error::new(
                ErrorCode::MetadataMismatch,
                "outer threadId differs from x-codex-turn-metadata.thread_id",
            )
            .component("metadata"));
        }

        Ok(Self {
            thread_id: parsed.thread_id,
            turn_id: parsed.turn_id,
            model: parsed.model.filter(|value| !value.trim().is_empty()),
            reasoning_effort: parsed
                .reasoning_effort
                .filter(|value| !value.trim().is_empty()),
        })
    }
}

fn parse_turn_metadata(value: &Value) -> Result<TurnMetadata> {
    match value {
        Value::Object(_) => {
            let bytes = serde_json::to_vec(value)?;
            if bytes.len() > MAX_TURN_METADATA_BYTES {
                return Err(invalid_turn_metadata());
            }
            serde_json::from_slice(&bytes).map_err(|_| invalid_turn_metadata())
        }
        Value::String(text) if text.len() <= MAX_TURN_METADATA_BYTES => {
            if let Ok(parsed) = serde_json::from_str(text) {
                return Ok(parsed);
            }
            parse_encoded_turn_metadata(text)
        }
        _ => Err(invalid_turn_metadata()),
    }
}

fn parse_encoded_turn_metadata(text: &str) -> Result<TurnMetadata> {
    if !text.is_ascii() {
        return Err(invalid_turn_metadata());
    }
    let encoded = text.strip_prefix("base64:").unwrap_or(text);
    for engine in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD] {
        let Ok(bytes) = engine.decode(encoded) else {
            continue;
        };
        if bytes.len() <= MAX_TURN_METADATA_BYTES {
            if let Ok(parsed) = serde_json::from_slice(&bytes) {
                return Ok(parsed);
            }
        }
    }
    Err(invalid_turn_metadata())
}

fn invalid_turn_metadata() -> Error {
    Error::new(
        ErrorCode::InvalidMetadata,
        "x-codex-turn-metadata must be a bounded object, JSON string, or base64 JSON string",
    )
    .component("metadata")
}

fn validate_identifier(label: &str, value: &str) -> Result<()> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(Error::new(
            ErrorCode::InvalidMetadata,
            format!("{label} has an invalid shape"),
        )
        .component("metadata"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn binds_matching_metadata() {
        let bound = BoundInvocation::from_meta(&json!({
            "threadId": "thr_1",
            "x-codex-turn-metadata": {
                "thread_id": "thr_1",
                "turn_id": "turn_1",
                "model": "gpt-test",
                "reasoning_effort": "high"
            }
        }))
        .unwrap();
        assert_eq!(bound.thread_id, "thr_1");
        assert_eq!(bound.turn_id, "turn_1");
    }

    #[test]
    fn rejects_cross_thread_metadata() {
        let error = BoundInvocation::from_meta(&json!({
            "threadId": "thr_a",
            "x-codex-turn-metadata": {
                "thread_id": "thr_b",
                "turn_id": "turn_1"
            }
        }))
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::MetadataMismatch);
    }

    #[test]
    fn accepts_json_and_ascii_safe_encoded_strings() {
        let inner = r#"{"thread_id":"thr_1","turn_id":"turn_1"}"#;
        for encoded in [
            inner.to_owned(),
            STANDARD.encode(inner),
            URL_SAFE_NO_PAD.encode(inner),
            format!("base64:{}", STANDARD.encode(inner)),
        ] {
            let bound = BoundInvocation::from_meta(&json!({
                "threadId": "thr_1",
                "x-codex-turn-metadata": encoded
            }))
            .unwrap();
            assert_eq!(bound.turn_id, "turn_1");
        }
    }

    #[test]
    fn rejects_missing_malformed_and_oversized_metadata() {
        for meta in [
            json!({}),
            json!({"threadId": "thr_1"}),
            json!({
                "threadId": "thr_1",
                "x-codex-turn-metadata": "not-json-or-base64"
            }),
            json!({
                "threadId": "thr_1",
                "x-codex-turn-metadata": "a".repeat(MAX_TURN_METADATA_BYTES + 1)
            }),
        ] {
            assert!(BoundInvocation::from_meta(&meta).is_err());
        }
    }
}
