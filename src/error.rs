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
    RecentNativeCompaction,
    NotRootThread,
    ActiveSubagents,
    ActiveWork,
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
            Self::RecentNativeCompaction => "recent_native_compaction",
            Self::NotRootThread => "not_root_thread",
            Self::ActiveSubagents => "active_subagents",
            Self::ActiveWork => "active_work",
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

    pub const fn is_expected_rejection(self) -> bool {
        matches!(
            self,
            Self::UnsupportedCodex
                | Self::SharedAppServerUnavailable
                | Self::AppServerOverloaded
                | Self::TransitionPending
                | Self::CooldownActive
                | Self::RecentNativeCompaction
                | Self::NotRootThread
                | Self::ActiveSubagents
                | Self::ActiveWork
        )
    }

    pub const fn model_message(self) -> &'static str {
        match self {
            Self::InvalidRequest => "The compaction request is invalid.",
            Self::InvalidMetadata => "The host did not provide valid thread metadata.",
            Self::MetadataMismatch => "The compaction request does not match the active thread.",
            Self::SensitiveCheckpointInput => {
                "The continuity input may contain sensitive data; continue without compaction."
            }
            Self::CheckpointTooLarge => {
                "The continuity input is too large; continue without compaction."
            }
            Self::CheckpointInvalid => "The continuity checkpoint is invalid.",
            Self::SharedAppServerUnavailable => {
                "The shared Codex app-server is unavailable; continue without compaction."
            }
            Self::AppServerOverloaded => {
                "The shared Codex app-server is overloaded; continue without retrying this turn."
            }
            Self::ThreadSnapshotTooLarge => "The thread snapshot is too large to compact safely.",
            Self::UnsupportedCodex => "This Codex version is not supported for Agentic Compact.",
            Self::TransitionPending => {
                "A compaction transition is already pending; continue the current task."
            }
            Self::CooldownActive => {
                "A recent compaction is still in cooldown; continue normal work."
            }
            Self::RecentNativeCompaction => {
                "Codex recently compacted this thread; continue normal work."
            }
            Self::NotRootThread => "Agentic Compact runs only in a root thread.",
            Self::ActiveSubagents => "Active subagents must finish before compaction.",
            Self::ActiveWork => "Active tool work must finish before compaction.",
            Self::QuiescenceViolation => "The source turn did not reach a safe completed boundary.",
            Self::SourceTurnFailed => "The source turn did not complete successfully.",
            Self::RaceLost => "A newer turn took priority; continue in that turn.",
            Self::CompactionFailed => "Codex-native compaction did not complete safely.",
            Self::InjectionFailed => "Continuity state could not be injected safely.",
            Self::ContinuationUnsupported => "An empty same-thread continuation is not supported.",
            Self::ServerRequestReceived => {
                "A server request interrupted the compaction transition."
            }
            Self::RecoveryAmbiguous => {
                "The prior compaction state is ambiguous and was not replayed."
            }
            Self::ConfigUserModified => "Managed Agentic Compact configuration was modified.",
            Self::Io => "Agentic Compact encountered an I/O failure.",
            Self::Protocol => "The Codex app-server response violated the supported protocol.",
            Self::Timeout => "The compaction transition timed out without unsafe retry.",
            Self::Internal => "Agentic Compact encountered an internal failure.",
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

#[cfg(test)]
mod tests {
    use super::ErrorCode;

    const CASES: &[(ErrorCode, &str)] = &[
        (
            ErrorCode::InvalidRequest,
            "The compaction request is invalid.",
        ),
        (
            ErrorCode::InvalidMetadata,
            "The host did not provide valid thread metadata.",
        ),
        (
            ErrorCode::MetadataMismatch,
            "The compaction request does not match the active thread.",
        ),
        (
            ErrorCode::SensitiveCheckpointInput,
            "The continuity input may contain sensitive data; continue without compaction.",
        ),
        (
            ErrorCode::CheckpointTooLarge,
            "The continuity input is too large; continue without compaction.",
        ),
        (
            ErrorCode::CheckpointInvalid,
            "The continuity checkpoint is invalid.",
        ),
        (
            ErrorCode::SharedAppServerUnavailable,
            "The shared Codex app-server is unavailable; continue without compaction.",
        ),
        (
            ErrorCode::AppServerOverloaded,
            "The shared Codex app-server is overloaded; continue without retrying this turn.",
        ),
        (
            ErrorCode::ThreadSnapshotTooLarge,
            "The thread snapshot is too large to compact safely.",
        ),
        (
            ErrorCode::UnsupportedCodex,
            "This Codex version is not supported for Agentic Compact.",
        ),
        (
            ErrorCode::TransitionPending,
            "A compaction transition is already pending; continue the current task.",
        ),
        (
            ErrorCode::CooldownActive,
            "A recent compaction is still in cooldown; continue normal work.",
        ),
        (
            ErrorCode::RecentNativeCompaction,
            "Codex recently compacted this thread; continue normal work.",
        ),
        (
            ErrorCode::NotRootThread,
            "Agentic Compact runs only in a root thread.",
        ),
        (
            ErrorCode::ActiveSubagents,
            "Active subagents must finish before compaction.",
        ),
        (
            ErrorCode::ActiveWork,
            "Active tool work must finish before compaction.",
        ),
        (
            ErrorCode::QuiescenceViolation,
            "The source turn did not reach a safe completed boundary.",
        ),
        (
            ErrorCode::SourceTurnFailed,
            "The source turn did not complete successfully.",
        ),
        (
            ErrorCode::RaceLost,
            "A newer turn took priority; continue in that turn.",
        ),
        (
            ErrorCode::CompactionFailed,
            "Codex-native compaction did not complete safely.",
        ),
        (
            ErrorCode::InjectionFailed,
            "Continuity state could not be injected safely.",
        ),
        (
            ErrorCode::ContinuationUnsupported,
            "An empty same-thread continuation is not supported.",
        ),
        (
            ErrorCode::ServerRequestReceived,
            "A server request interrupted the compaction transition.",
        ),
        (
            ErrorCode::RecoveryAmbiguous,
            "The prior compaction state is ambiguous and was not replayed.",
        ),
        (
            ErrorCode::ConfigUserModified,
            "Managed Agentic Compact configuration was modified.",
        ),
        (ErrorCode::Io, "Agentic Compact encountered an I/O failure."),
        (
            ErrorCode::Protocol,
            "The Codex app-server response violated the supported protocol.",
        ),
        (
            ErrorCode::Timeout,
            "The compaction transition timed out without unsafe retry.",
        ),
        (
            ErrorCode::Internal,
            "Agentic Compact encountered an internal failure.",
        ),
    ];

    #[test]
    fn model_messages_and_expected_rejections_are_stable() {
        let expected = [
            ErrorCode::UnsupportedCodex,
            ErrorCode::SharedAppServerUnavailable,
            ErrorCode::AppServerOverloaded,
            ErrorCode::TransitionPending,
            ErrorCode::CooldownActive,
            ErrorCode::RecentNativeCompaction,
            ErrorCode::NotRootThread,
            ErrorCode::ActiveSubagents,
            ErrorCode::ActiveWork,
        ];

        for (code, message) in CASES {
            assert_eq!(code.model_message(), *message);
            assert_eq!(code.is_expected_rejection(), expected.contains(code));
        }
    }
}
