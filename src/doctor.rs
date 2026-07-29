use crate::app_server::AppServerClient;
use crate::checkpoint::{Checkpoint, CompactionIntent, Evidence, injection_items};
use crate::cli::DoctorArgs;
use crate::error::{Error, ErrorCode, Result};
use crate::protocol::AppEvent;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;
use tokio::time::timeout;
use uuid::Uuid;

const RECORD_SCHEMA_VERSION: u32 = 1;
const PROBE_TIMEOUT: Duration = Duration::from_secs(180);
const RECORD_FILE: &str = "agentic-compact/capabilities.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityRecord {
    pub schema_version: u32,
    pub plugin_version: String,
    pub codex_user_agent: String,
    pub platform_family: String,
    pub platform_os: String,
    pub empty_continuation: bool,
    pub reentrant_attach_acknowledged: bool,
    pub hidden_checkpoint_acknowledged: bool,
    pub checked_at_ms: i64,
}

impl CapabilityRecord {
    fn same_build(&self, client: &AppServerClient) -> bool {
        self.schema_version == RECORD_SCHEMA_VERSION
            && self.plugin_version == env!("CARGO_PKG_VERSION")
            && self.codex_user_agent == client.initialize_result.user_agent
            && self.platform_family == client.initialize_result.platform_family
            && self.platform_os == client.initialize_result.platform_os
    }

    pub fn matches_client(&self, client: &AppServerClient) -> bool {
        self.same_build(client)
            && self.empty_continuation
            && self.reentrant_attach_acknowledged
            && self.hidden_checkpoint_acknowledged
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorReport {
    ready: bool,
    shared_app_server: bool,
    loaded_thread_count: usize,
    codex_user_agent: String,
    platform_family: String,
    platform_os: String,
    empty_continuation: bool,
    reentrant_attach_acknowledged: bool,
    hidden_checkpoint_acknowledged: bool,
    capability_record: Option<String>,
    notes: Vec<&'static str>,
}

pub async fn run(args: DoctorArgs) -> Result<()> {
    let client = AppServerClient::connect_default().await?;
    let loaded = client.loaded_threads().await?;
    let existing = load_capability_record(Path::new(&client.initialize_result.codex_home))?;

    let empty_continuation = if args.probe {
        probe_empty_continuation(&client).await?
    } else {
        existing
            .as_ref()
            .is_some_and(|record| record.same_build(&client) && record.empty_continuation)
    };

    let record = CapabilityRecord {
        schema_version: RECORD_SCHEMA_VERSION,
        plugin_version: env!("CARGO_PKG_VERSION").to_owned(),
        codex_user_agent: client.initialize_result.user_agent.clone(),
        platform_family: client.initialize_result.platform_family.clone(),
        platform_os: client.initialize_result.platform_os.clone(),
        empty_continuation,
        reentrant_attach_acknowledged: args.ack_reentrant_attach
            || existing.as_ref().is_some_and(|record| {
                record.same_build(&client) && record.reentrant_attach_acknowledged
            }),
        hidden_checkpoint_acknowledged: args.ack_hidden_checkpoint
            || existing.as_ref().is_some_and(|record| {
                record.same_build(&client) && record.hidden_checkpoint_acknowledged
            }),
        checked_at_ms: now_ms(),
    };

    let ready = record.empty_continuation
        && record.reentrant_attach_acknowledged
        && record.hidden_checkpoint_acknowledged;
    let record_path = if args.probe || args.ack_reentrant_attach || args.ack_hidden_checkpoint {
        Some(save_capability_record(
            Path::new(&client.initialize_result.codex_home),
            &record,
        )?)
    } else {
        capability_path(Path::new(&client.initialize_result.codex_home))
    };

    let report = DoctorReport {
        ready,
        shared_app_server: true,
        loaded_thread_count: loaded.len(),
        codex_user_agent: client.initialize_result.user_agent.clone(),
        platform_family: client.initialize_result.platform_family.clone(),
        platform_os: client.initialize_result.platform_os.clone(),
        empty_continuation: record.empty_continuation,
        reentrant_attach_acknowledged: record.reentrant_attach_acknowledged,
        hidden_checkpoint_acknowledged: record.hidden_checkpoint_acknowledged,
        capability_record: args
            .verbose
            .then(|| record_path.map(|path| path.display().to_string()))
            .flatten(),
        notes: vec![
            "--probe creates a disposable ephemeral thread and may consume model usage.",
            "reentrant attach and hidden-checkpoint behavior require stock-TUI manual acknowledgement.",
            "a capability record is bound to the active Codex user agent and platform projection.",
        ],
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    client.close().await;

    if ready {
        Ok(())
    } else {
        Err(Error::new(
            ErrorCode::UnsupportedCodex,
            "doctor gates are incomplete; run --probe and acknowledge both stock-TUI checks",
        )
        .component("doctor"))
    }
}

pub fn load_capability_record(codex_home: &Path) -> Result<Option<CapabilityRecord>> {
    let Some(path) = capability_path(codex_home) else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path)?;
    if bytes.len() > 64 * 1024 {
        return Err(Error::new(
            ErrorCode::UnsupportedCodex,
            "capability record exceeds 64 KiB",
        )
        .component("doctor"));
    }
    let record: CapabilityRecord = serde_json::from_slice(&bytes)?;
    Ok(Some(record))
}

pub fn require_ready_capabilities(client: &AppServerClient) -> Result<()> {
    let home = Path::new(&client.initialize_result.codex_home);
    let ready = load_capability_record(home)?.is_some_and(|record| record.matches_client(client));
    if ready {
        Ok(())
    } else {
        Err(Error::new(
            ErrorCode::UnsupportedCodex,
            "no valid capability record exists for the active Codex build; run agentic-compact doctor",
        )
        .component("doctor"))
    }
}

async fn probe_empty_continuation(client: &AppServerClient) -> Result<bool> {
    let mut events = client.subscribe();
    let snapshot = client.start_ephemeral_thread().await?;
    let thread_id = snapshot.thread.id.clone();
    let result = async {
        let intent = CompactionIntent {
            preserve: Vec::new(),
            next_action: "Reply with exactly probe-ok. Do not call tools.".to_owned(),
        }
        .validate()?;
        let checkpoint = Checkpoint::build(
            format!("cp_{}", Uuid::new_v4().simple()),
            format!("rcpt_{}", Uuid::new_v4().simple()),
            thread_id.clone(),
            "doctor_probe_source".to_owned(),
            "doctor_probe_compact".to_owned(),
            intent,
            Evidence::default(),
        )?;
        client
            .inject_items(&thread_id, injection_items(&checkpoint)?)
            .await?;
        let turn_id = client.start_empty_turn(&thread_id).await?;
        await_probe_turn(&mut events, &thread_id, &turn_id).await?;
        Ok::<bool, Error>(true)
    }
    .await;

    if let Err(error) = client.unsubscribe(&thread_id).await {
        tracing::warn!(
            reason_code = error.code.as_str(),
            "failed to unsubscribe from disposable doctor thread"
        );
    }
    result
}

async fn await_probe_turn(
    events: &mut broadcast::Receiver<AppEvent>,
    thread_id: &str,
    turn_id: &str,
) -> Result<()> {
    timeout(PROBE_TIMEOUT, async {
        let mut started = false;
        loop {
            match events.recv().await {
                Ok(AppEvent::TurnStarted {
                    thread_id: actual,
                    turn,
                }) if actual == thread_id && turn.id == turn_id => {
                    started = true;
                }
                Ok(AppEvent::TurnCompleted {
                    thread_id: actual,
                    turn,
                }) if actual == thread_id && turn.id == turn_id => {
                    if !started || turn.status != "completed" {
                        return Err(Error::new(
                            ErrorCode::ContinuationUnsupported,
                            "empty continuation did not complete successfully",
                        )
                        .component("doctor"));
                    }
                    return Ok(());
                }
                Ok(AppEvent::ItemStarted {
                    thread_id: actual,
                    turn_id: actual_turn,
                    item,
                })
                | Ok(AppEvent::ItemCompleted {
                    thread_id: actual,
                    turn_id: actual_turn,
                    item,
                }) if actual == thread_id
                    && actual_turn == turn_id
                    && item.item_type == "userMessage" =>
                {
                    return Err(Error::new(
                        ErrorCode::ContinuationUnsupported,
                        "empty continuation created a synthetic userMessage",
                    )
                    .component("doctor"));
                }
                Ok(AppEvent::ServerRequest { .. }) => {
                    return Err(Error::new(
                        ErrorCode::ServerRequestReceived,
                        "doctor probe received an approval or other server request",
                    )
                    .component("doctor"));
                }
                Ok(AppEvent::ConnectionClosed { .. }) => {
                    return Err(Error::new(
                        ErrorCode::SharedAppServerUnavailable,
                        "app-server closed during doctor probe",
                    )
                    .component("doctor"));
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    return Err(Error::new(
                        ErrorCode::RecoveryAmbiguous,
                        "doctor probe event stream lagged",
                    )
                    .component("doctor"));
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(Error::new(
                        ErrorCode::SharedAppServerUnavailable,
                        "doctor probe event stream closed",
                    )
                    .component("doctor"));
                }
            }
        }
    })
    .await
    .map_err(|_| Error::timeout("doctor", "empty continuation probe timed out"))?
}

fn save_capability_record(codex_home: &Path, record: &CapabilityRecord) -> Result<PathBuf> {
    let path = codex_home.join(RECORD_FILE);
    let parent = path
        .parent()
        .ok_or_else(|| Error::new(ErrorCode::Io, "invalid capability path"))?;
    fs::create_dir_all(parent)?;
    set_private_dir(parent)?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec_pretty(record)?;
    fs::write(&temporary, bytes)?;
    set_private_file(&temporary)?;
    fs::rename(&temporary, &path)?;
    Ok(path)
}

fn capability_path(codex_home: &Path) -> Option<PathBuf> {
    (!codex_home.as_os_str().is_empty()).then(|| codex_home.join(RECORD_FILE))
}

#[cfg(unix)]
fn set_private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_file(_path: &Path) -> Result<()> {
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_requires_all_three_gates() {
        let record = CapabilityRecord {
            schema_version: RECORD_SCHEMA_VERSION,
            plugin_version: env!("CARGO_PKG_VERSION").to_owned(),
            codex_user_agent: "codex-test".to_owned(),
            platform_family: "unix".to_owned(),
            platform_os: "linux".to_owned(),
            empty_continuation: true,
            reentrant_attach_acknowledged: true,
            hidden_checkpoint_acknowledged: false,
            checked_at_ms: 0,
        };
        assert!(!record.hidden_checkpoint_acknowledged);
    }
}
