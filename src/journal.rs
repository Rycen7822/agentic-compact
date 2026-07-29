use crate::checkpoint::{Checkpoint, CompactionIntent};
use crate::error::{Error, ErrorCode, Result};
use crate::lease::{secure_dir, secure_file, state_root};
use crate::observability::hash_identifier;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const JOURNAL_SCHEMA_VERSION: u32 = 1;
const MAX_JOURNAL_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TransitionState {
    Attaching,
    AwaitSourceTurnCompleted,
    ReadyToCompact,
    CompactRequestSent,
    AwaitCompactionItem,
    AwaitCompactionTurnCompleted,
    InjectingCheckpoint,
    StartingContinuation,
    AwaitContinuationStarted,
    Cooldown,
    Cancelled,
    FailedSafe,
}

impl TransitionState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Cooldown | Self::Cancelled | Self::FailedSafe)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionJournal {
    pub schema_version: u32,
    pub thread_id: String,
    pub source_turn_id: String,
    pub receipt_id: String,
    pub checkpoint_id: String,
    pub intent: CompactionIntent,
    pub state: TransitionState,
    pub compact_turn_id: Option<String>,
    #[serde(default)]
    pub checkpoint: Option<Checkpoint>,
    pub checkpoint_sha256: Option<String>,
    pub continuation_turn_id: Option<String>,
    pub reason_code: Option<String>,
    pub last_detail: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone)]
pub struct JournalStore {
    root: PathBuf,
}

impl TransitionJournal {
    pub fn new(
        thread_id: String,
        source_turn_id: String,
        receipt_id: String,
        checkpoint_id: String,
        intent: CompactionIntent,
    ) -> Result<Self> {
        if thread_id.is_empty() || source_turn_id.is_empty() {
            return Err(Error::invalid(
                "journal thread/turn identifiers cannot be empty",
            ));
        }
        let now = now_ms();
        Ok(Self {
            schema_version: JOURNAL_SCHEMA_VERSION,
            thread_id,
            source_turn_id,
            receipt_id,
            checkpoint_id,
            intent,
            state: TransitionState::Attaching,
            compact_turn_id: None,
            checkpoint: None,
            checkpoint_sha256: None,
            continuation_turn_id: None,
            reason_code: None,
            last_detail: "intent accepted".to_owned(),
            created_at_ms: now,
            updated_at_ms: now,
        })
    }

    pub fn transition(&mut self, next: TransitionState, detail: impl Into<String>) -> Result<()> {
        if !transition_allowed(self.state, next) {
            return Err(Error::new(
                ErrorCode::Internal,
                format!("invalid transition {:?} -> {:?}", self.state, next),
            )
            .component("journal"));
        }
        self.state = next;
        self.last_detail = detail.into();
        self.updated_at_ms = now_ms();
        Ok(())
    }

    pub fn set_compact_turn(&mut self, turn_id: String) -> Result<()> {
        set_once(&mut self.compact_turn_id, turn_id, "compact turn")
    }

    pub fn set_checkpoint(&mut self, checkpoint: Checkpoint) -> Result<()> {
        checkpoint.verify()?;
        if checkpoint.checkpoint_id != self.checkpoint_id
            || checkpoint.receipt_id != self.receipt_id
            || checkpoint.source_thread_id != self.thread_id
            || checkpoint.source_turn_id != self.source_turn_id
            || self.compact_turn_id.as_deref() != Some(checkpoint.compact_turn_id.as_str())
        {
            return Err(Error::new(
                ErrorCode::RecoveryAmbiguous,
                "checkpoint identity does not match its transition journal",
            )
            .component("journal"));
        }
        set_once(
            &mut self.checkpoint_sha256,
            checkpoint.sha256.clone(),
            "checkpoint SHA-256",
        )?;
        match &self.checkpoint {
            None => {
                self.checkpoint = Some(checkpoint);
                Ok(())
            }
            Some(existing) if serde_json::to_vec(existing)? == serde_json::to_vec(&checkpoint)? => {
                Ok(())
            }
            Some(_) => Err(Error::new(
                ErrorCode::RecoveryAmbiguous,
                "checkpoint was already bound to a different capsule",
            )
            .component("journal")),
        }
    }

    pub fn set_continuation_turn(&mut self, turn_id: String) -> Result<()> {
        set_once(&mut self.continuation_turn_id, turn_id, "continuation turn")
    }

    pub fn fail(&mut self, reason_code: &str) {
        self.state = TransitionState::FailedSafe;
        self.reason_code = Some(reason_code.to_owned());
        self.last_detail = "transition stopped in fail-closed mode".to_owned();
        self.updated_at_ms = now_ms();
    }

    pub fn cancel(&mut self, reason_code: &str) {
        self.state = TransitionState::Cancelled;
        self.reason_code = Some(reason_code.to_owned());
        self.last_detail = "transition cancelled; user/session work takes precedence".to_owned();
        self.updated_at_ms = now_ms();
    }
}

impl JournalStore {
    pub fn open() -> Result<Self> {
        Self::open_in(state_root()?.join("journals"))
    }

    fn open_in(root: PathBuf) -> Result<Self> {
        secure_dir(&root)?;
        Ok(Self { root })
    }

    #[cfg(test)]
    pub(crate) fn for_test(root: PathBuf) -> Result<Self> {
        Self::open_in(root)
    }

    pub fn load(&self, thread_id: &str) -> Result<Option<TransitionJournal>> {
        let path = self.path_for(thread_id);
        if !path.exists() {
            return Ok(None);
        }
        let metadata = std::fs::metadata(&path)?;
        if metadata.len() > MAX_JOURNAL_BYTES {
            return Err(Error::new(
                ErrorCode::RecoveryAmbiguous,
                "journal exceeds the 64 KiB safety bound",
            )
            .component("journal"));
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        File::open(&path)?.read_to_end(&mut bytes)?;
        let journal: TransitionJournal = serde_json::from_slice(&bytes)?;
        if journal.schema_version != JOURNAL_SCHEMA_VERSION || journal.thread_id != thread_id {
            return Err(Error::new(
                ErrorCode::RecoveryAmbiguous,
                "journal identity or schema version does not match",
            )
            .component("journal"));
        }
        Ok(Some(journal))
    }

    pub fn save(&self, journal: &TransitionJournal) -> Result<()> {
        let path = self.path_for(&journal.thread_id);
        let bytes = serde_json::to_vec_pretty(journal)?;
        if bytes.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(Error::new(
                ErrorCode::Internal,
                "serialized journal exceeds the 64 KiB safety bound",
            )
            .component("journal"));
        }
        atomic_write(&path, &bytes)
    }

    pub fn nonterminal(&self) -> Result<Vec<TransitionJournal>> {
        let mut journals = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let metadata = entry.metadata()?;
            if metadata.len() > MAX_JOURNAL_BYTES {
                continue;
            }
            let bytes = std::fs::read(entry.path())?;
            let Ok(journal) = serde_json::from_slice::<TransitionJournal>(&bytes) else {
                continue;
            };
            if !journal.state.is_terminal() {
                journals.push(journal);
            }
        }
        Ok(journals)
    }

    fn path_for(&self, thread_id: &str) -> PathBuf {
        self.root
            .join(format!("{}.json", hash_identifier(thread_id)))
    }
}

fn transition_allowed(current: TransitionState, next: TransitionState) -> bool {
    use TransitionState::*;
    matches!(
        (current, next),
        (Attaching, AwaitSourceTurnCompleted)
            | (AwaitSourceTurnCompleted, ReadyToCompact)
            | (ReadyToCompact, CompactRequestSent)
            | (CompactRequestSent, AwaitCompactionItem)
            | (AwaitCompactionItem, AwaitCompactionTurnCompleted)
            | (AwaitCompactionTurnCompleted, InjectingCheckpoint)
            | (InjectingCheckpoint, StartingContinuation)
            | (StartingContinuation, Cooldown)
            | (StartingContinuation, AwaitContinuationStarted)
            | (AwaitContinuationStarted, Cooldown)
    ) || next == Cancelled
        || next == FailedSafe
}

fn set_once(slot: &mut Option<String>, value: String, label: &str) -> Result<()> {
    if value.is_empty() {
        return Err(
            Error::new(ErrorCode::Internal, format!("{label} cannot be empty"))
                .component("journal"),
        );
    }
    match slot {
        None => {
            *slot = Some(value);
            Ok(())
        }
        Some(existing) if existing == &value => Ok(()),
        Some(_) => Err(Error::new(
            ErrorCode::RecoveryAmbiguous,
            format!("{label} was already bound to a different value"),
        )
        .component("journal")),
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::new(ErrorCode::Io, "journal path has no parent"))?;
    secure_dir(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp.{}.{}",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("journal"),
        std::process::id(),
        now_ms()
    ));
    let write_result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        secure_file(path)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    write_result
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests;
