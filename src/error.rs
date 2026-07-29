use serde::Serialize;
use std::borrow::Cow;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    InvalidMetadata,
    MetadataMismatch,
    SensitiveCheckpointInput,
    CheckpointTooLarge,
    CheckpointInvalid,
    SharedAppServerUnavailable,
    AppServerOverloaded,
    ThreadSnapshotTooLarge,
    UnsupportedCodex,
    TransitionPending,
    CooldownActive,
    NotRootThread,
    ActiveSubagents,
    QuiescenceViolation,
    SourceTurnFailed,
    RaceLost,
    CompactionFailed,
    InjectionFailed,
    ContinuationUnsupported,
    ServerRequestReceived,
    RecoveryAmbiguous,
    ConfigUserModified,
    Io,
    Protocol,
    Timeout,
    Internal,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::InvalidMetadata => "invalid_metadata",
            Self::MetadataMismatch => "metadata_mismatch",
            Self::SensitiveCheckpointInput => "sensitive_checkpoint_input",
            Self::CheckpointTooLarge => "checkpoint_too_large",
            Self::CheckpointInvalid => "checkpoint_invalid",
            Self::SharedAppServerUnavailable => "shared_app_server_unavailable",
            Self::AppServerOverloaded => "app_server_overloaded",
            Self::ThreadSnapshotTooLarge => "thread_snapshot_too_large",
            Self::UnsupportedCodex => "unsupported_codex",
            Self::TransitionPending => "transition_pending",
            Self::CooldownActive => "cooldown_active",
            Self::NotRootThread => "not_root_thread",
            Self::ActiveSubagents => "active_subagents",
            Self::QuiescenceViolation => "quiescence_violation",
            Self::SourceTurnFailed => "source_turn_failed",
            Self::RaceLost => "race_lost",
            Self::CompactionFailed => "compaction_failed",
            Self::InjectionFailed => "injection_failed",
            Self::ContinuationUnsupported => "continuation_unsupported",
            Self::ServerRequestReceived => "server_request_received",
            Self::RecoveryAmbiguous => "recovery_ambiguous",
            Self::ConfigUserModified => "config_user_modified",
            Self::Io => "io_error",
            Self::Protocol => "protocol_error",
            Self::Timeout => "timeout",
            Self::Internal => "internal_error",
        }
    }
}

#[derive(Debug, thiserror::Error, Serialize)]
#[error("{code}: {message}", code = .code.as_str())]
pub struct Error {
    pub code: ErrorCode,
    #[serde(skip)]
    pub message: Cow<'static, str>,
    pub component: &'static str,
    pub retryable: bool,
    pub rpc_code: Option<i64>,
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            code,
            message: message.into(),
            component: "agentic_compact",
            retryable: matches!(
                code,
                ErrorCode::AppServerOverloaded
                    | ErrorCode::SharedAppServerUnavailable
                    | ErrorCode::Timeout
            ),
            rpc_code: None,
        }
    }

    pub fn component(mut self, component: &'static str) -> Self {
        self.component = component;
        self
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn protocol(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(ErrorCode::Protocol, message).component("protocol")
    }

    pub fn invalid(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(ErrorCode::InvalidRequest, message)
    }

    pub fn timeout(component: &'static str, message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(ErrorCode::Timeout, message).component(component)
    }

    pub fn rpc(code: i64, message: impl Into<Cow<'static, str>>) -> Self {
        let mapped = if code == -32001 {
            ErrorCode::AppServerOverloaded
        } else {
            ErrorCode::Protocol
        };
        Self::new(mapped, message)
            .component("app_server")
            .with_rpc_code(code)
    }

    fn with_rpc_code(mut self, code: i64) -> Self {
        self.rpc_code = Some(code);
        self
    }

    pub fn exit_code(&self) -> i32 {
        match self.code {
            ErrorCode::InvalidRequest
            | ErrorCode::InvalidMetadata
            | ErrorCode::MetadataMismatch
            | ErrorCode::SensitiveCheckpointInput => 2,
            ErrorCode::UnsupportedCodex
            | ErrorCode::SharedAppServerUnavailable
            | ErrorCode::ContinuationUnsupported => 3,
            _ => 1,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::new(ErrorCode::Io, value.to_string()).component("io")
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::new(ErrorCode::Protocol, value.to_string()).component("json")
    }
}

impl From<toml_edit::TomlError> for Error {
    fn from(value: toml_edit::TomlError) -> Self {
        Self::new(ErrorCode::Protocol, value.to_string()).component("toml")
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for Error {
    fn from(value: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::new(ErrorCode::Protocol, value.to_string()).component("websocket")
    }
}
