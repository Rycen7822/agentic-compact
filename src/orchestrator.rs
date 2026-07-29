mod recovery;
mod runner;

use crate::app_server::AppServerClient;
use crate::checkpoint::{CompactionIntent, Evidence};
use crate::doctor::{load_capability_record, require_ready_capabilities};
use crate::error::{Error, ErrorCode, Result};
use crate::journal::{JournalStore, TransitionJournal, TransitionState};
use crate::lease::ThreadLease;
use crate::metadata::BoundInvocation;
use crate::observability::hash_identifier;
use crate::protocol::{ResumeSnapshot, ThreadRef, completed_regular_turns_after};
use dashmap::DashSet;
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;
use tracing::{info, warn};
use uuid::Uuid;

pub(super) const SOURCE_TURN_TIMEOUT: Duration = Duration::from_secs(15 * 60);
pub(super) const COMPACTION_TIMEOUT: Duration = Duration::from_secs(5 * 60);
pub(super) const CONTINUATION_START_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const PREPARE_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const COOLDOWN_REGULAR_TURNS: usize = 3;
pub(super) const MCP_SERVER_NAMES: &[&str] = &["agentic-compact", "agentic_compact"];
pub(super) const REQUEST_TOOL_NAMES: &[&str] =
    &["request_compaction", "agentic_compact.request_compaction"];

#[derive(Clone, Default)]
pub struct TransitionRegistry {
    active: Arc<DashSet<String>>,
}

impl TransitionRegistry {
    fn reserve(&self, thread_id: &str) -> Result<RegistryPermit> {
        if !self.active.insert(thread_id.to_owned()) {
            return Err(Error::new(
                ErrorCode::TransitionPending,
                "a transition is already active for this thread",
            )
            .component("orchestrator"));
        }
        Ok(RegistryPermit {
            key: thread_id.to_owned(),
            active: Arc::clone(&self.active),
        })
    }

    pub fn is_active(&self, thread_id: &str) -> bool {
        self.active.contains(thread_id)
    }
}

pub(super) struct RegistryPermit {
    key: String,
    active: Arc<DashSet<String>>,
}

impl Drop for RegistryPermit {
    fn drop(&mut self) {
        self.active.remove(&self.key);
    }
}

#[derive(Clone)]
pub struct Orchestrator {
    pub(super) registry: TransitionRegistry,
    pub(super) journals: JournalStore,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleResult {
    pub status: &'static str,
    pub receipt_id: String,
    pub checkpoint_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusResult {
    pub version: &'static str,
    pub mode: &'static str,
    pub thread_id_bound: bool,
    pub transition_pending: bool,
    pub cooldown_turns_remaining: usize,
    pub active_context_tokens: Option<i64>,
    pub auto_compact_limit: Option<i64>,
    pub last_transition: Option<LastTransitionStatus>,
    pub guards: GuardStatus,
    pub reason_code: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastTransitionStatus {
    pub checkpoint_id: String,
    pub completed_regular_turns_ago: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardStatus {
    pub root_thread: bool,
    pub no_active_descendants: bool,
    pub shared_app_server: bool,
    pub empty_continuation: bool,
}

impl Orchestrator {
    pub fn new() -> Result<Self> {
        Ok(Self {
            registry: TransitionRegistry::default(),
            journals: JournalStore::open()?,
        })
    }

    pub async fn status(&self, bound: &BoundInvocation) -> Result<StatusResult> {
        let checked = timeout(Duration::from_secs(1), async {
            let client = AppServerClient::connect_default().await?;
            let result = self.status_with_client(bound, &client).await;
            client.close().await;
            result
        })
        .await;

        match checked {
            Ok(result) => result,
            Err(_) => Ok(StatusResult::blocked(
                ErrorCode::SharedAppServerUnavailable,
                false,
            )),
        }
    }

    async fn status_with_client(
        &self,
        bound: &BoundInvocation,
        client: &AppServerClient,
    ) -> Result<StatusResult> {
        let thread = client.thread_read(&bound.thread_id, true).await?;
        let journal = self.journals.load(&bound.thread_id)?;
        let capability_ready =
            load_capability_record(Path::new(&client.initialize_result.codex_home))?
                .is_some_and(|record| record.matches_client(client));
        let no_active_descendants = !active_descendant_exists(client, &bound.thread_id).await?;
        let transition_pending = self.registry.is_active(&bound.thread_id)
            || journal
                .as_ref()
                .is_some_and(|journal| !journal.state.is_terminal());
        let completed_regular_turns = journal
            .as_ref()
            .and_then(|journal| journal.continuation_turn_id.as_deref())
            .and_then(|turn_id| completed_regular_turns_after(&thread, turn_id));
        let cooldown_turns_remaining = journal
            .as_ref()
            .filter(|journal| journal.state == TransitionState::Cooldown)
            .map(|_| {
                COOLDOWN_REGULAR_TURNS.saturating_sub(completed_regular_turns.unwrap_or_default())
            })
            .unwrap_or_default();
        let last_transition = journal.as_ref().and_then(|journal| {
            (journal.state == TransitionState::Cooldown
                && journal.checkpoint_sha256.is_some()
                && journal.continuation_turn_id.is_some())
            .then(|| LastTransitionStatus {
                checkpoint_id: journal.checkpoint_id.clone(),
                completed_regular_turns_ago: completed_regular_turns.unwrap_or_default(),
            })
        });
        let root_thread = thread.parent_thread_id.is_none();
        let reason_code = if !capability_ready {
            Some(ErrorCode::UnsupportedCodex.as_str())
        } else if !root_thread {
            Some(ErrorCode::NotRootThread.as_str())
        } else if !no_active_descendants {
            Some(ErrorCode::ActiveSubagents.as_str())
        } else if transition_pending {
            Some(ErrorCode::TransitionPending.as_str())
        } else if cooldown_turns_remaining > 0 {
            Some(ErrorCode::CooldownActive.as_str())
        } else {
            None
        };

        Ok(StatusResult {
            version: env!("CARGO_PKG_VERSION"),
            mode: if !capability_ready {
                "disabled"
            } else if reason_code.is_none() {
                "ready"
            } else {
                "blocked"
            },
            thread_id_bound: true,
            transition_pending,
            cooldown_turns_remaining,
            active_context_tokens: None,
            auto_compact_limit: None,
            last_transition,
            guards: GuardStatus {
                root_thread,
                no_active_descendants,
                shared_app_server: true,
                empty_continuation: capability_ready,
            },
            reason_code,
        })
    }

    pub async fn schedule(
        &self,
        bound: BoundInvocation,
        intent: CompactionIntent,
    ) -> Result<ScheduleResult> {
        let permit = self.registry.reserve(&bound.thread_id)?;
        let lease = ThreadLease::acquire(&bound.thread_id)?;
        let prior = self.journals.load(&bound.thread_id)?;
        if prior
            .as_ref()
            .is_some_and(|journal| !journal.state.is_terminal())
        {
            return Err(Error::new(
                ErrorCode::RecoveryAmbiguous,
                "a non-terminal journal requires recovery before a new transition",
            )
            .component("orchestrator"));
        }

        let receipt_id = format!("rcpt_{}", Uuid::new_v4().simple());
        let checkpoint_id = format!("cp_{}", Uuid::new_v4().simple());
        let mut journal = TransitionJournal::new(
            bound.thread_id.clone(),
            bound.turn_id.clone(),
            receipt_id.clone(),
            checkpoint_id.clone(),
            intent,
        )?;
        self.journals.save(&journal)?;

        let preparation = timeout(PREPARE_TIMEOUT, async {
            let client = AppServerClient::connect_default().await?;
            require_ready_capabilities(&client)?;
            let events = client.subscribe();
            let snapshot = client.thread_resume(&bound.thread_id).await?;
            validate_resume_binding(&bound, &snapshot)?;
            preflight_root_and_cooldown(&snapshot.thread, prior.as_ref())?;
            ensure_no_active_descendant(&client, &bound.thread_id).await?;
            Ok::<_, Error>((client, events, snapshot))
        })
        .await;

        let (client, events, snapshot) = match preparation {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(error)) => {
                if runner::is_cancellation_error(error.code) {
                    journal.cancel(error.code.as_str());
                } else {
                    journal.fail(error.code.as_str());
                }
                self.journals.save(&journal)?;
                return Err(error);
            }
            Err(_) => {
                journal.fail(ErrorCode::Timeout.as_str());
                self.journals.save(&journal)?;
                return Err(Error::timeout(
                    "orchestrator",
                    "same-thread attach exceeded the 5 second MCP scheduling deadline",
                ));
            }
        };

        journal.transition(
            TransitionState::AwaitSourceTurnCompleted,
            "same-thread subscription and resume acknowledged",
        )?;
        self.journals.save(&journal)?;

        let journals = self.journals.clone();
        let task_bound = bound.clone();
        let task_receipt = receipt_id.clone();
        let task_checkpoint_id = checkpoint_id.clone();
        tokio::spawn(async move {
            let transition_hash = hash_identifier(&task_checkpoint_id);
            let result = runner::run_transition(
                &journals,
                &mut journal,
                &task_bound,
                &snapshot,
                &client,
                events,
                &task_receipt,
            )
            .await;
            if let Err(error) = result {
                if runner::is_cancellation_error(error.code) {
                    journal.cancel(error.code.as_str());
                } else {
                    journal.fail(error.code.as_str());
                }
                if let Err(save_error) = journals.save(&journal) {
                    warn!(
                        reason_code = save_error.code.as_str(),
                        transition_id_hash = %transition_hash,
                        "failed to persist terminal transition state"
                    );
                }
                warn!(
                    reason_code = error.code.as_str(),
                    transition_id_hash = %transition_hash,
                    state = ?journal.state,
                    "agentic compaction stopped"
                );
            }
            let _ = client.unsubscribe(&task_bound.thread_id).await;
            client.close().await;
            drop(lease);
            drop(permit);
        });

        info!(
            thread_id_hash = %hash_identifier(&bound.thread_id),
            transition_id_hash = %hash_identifier(&checkpoint_id),
            "agentic compaction scheduled"
        );
        Ok(ScheduleResult {
            status: "scheduled_after_turn",
            receipt_id,
            checkpoint_id,
        })
    }

    pub async fn recover_nonterminal_journals(&self) -> Result<usize> {
        recovery::recover_nonterminal_journals(self).await
    }
}

impl StatusResult {
    fn blocked(code: ErrorCode, thread_id_bound: bool) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            mode: "blocked",
            thread_id_bound,
            transition_pending: false,
            cooldown_turns_remaining: 0,
            active_context_tokens: None,
            auto_compact_limit: None,
            last_transition: None,
            guards: GuardStatus {
                root_thread: false,
                no_active_descendants: false,
                shared_app_server: false,
                empty_continuation: false,
            },
            reason_code: Some(code.as_str()),
        }
    }
}

pub(super) fn validate_resume_binding(
    bound: &BoundInvocation,
    snapshot: &ResumeSnapshot,
) -> Result<()> {
    if snapshot.thread.id != bound.thread_id {
        return Err(Error::new(
            ErrorCode::MetadataMismatch,
            "thread/resume returned a different thread id",
        )
        .component("orchestrator"));
    }
    if let (Some(expected), Some(actual)) = (bound.model.as_deref(), snapshot.model.as_deref()) {
        if expected != actual {
            return Err(Error::new(
                ErrorCode::MetadataMismatch,
                "thread/resume model differs from the source turn metadata",
            )
            .component("orchestrator"));
        }
    }
    if let (Some(expected), Some(actual)) = (
        bound.reasoning_effort.as_deref(),
        snapshot.reasoning_effort.as_deref(),
    ) {
        if expected != actual {
            return Err(Error::new(
                ErrorCode::MetadataMismatch,
                "thread/resume reasoning effort differs from the source turn metadata",
            )
            .component("orchestrator"));
        }
    }
    Ok(())
}

pub(super) fn preflight_root_and_cooldown(
    snapshot: &ThreadRef,
    prior: Option<&TransitionJournal>,
) -> Result<()> {
    if snapshot.parent_thread_id.is_some() {
        return Err(Error::new(
            ErrorCode::NotRootThread,
            "agentic compaction is restricted to root threads",
        )
        .component("orchestrator"));
    }
    if let Some(prior) = prior.filter(|journal| journal.state == TransitionState::Cooldown) {
        let continuation_turn_id = prior.continuation_turn_id.as_deref().ok_or_else(|| {
            Error::new(
                ErrorCode::RecoveryAmbiguous,
                "cooldown journal is missing continuationTurnId",
            )
            .component("orchestrator")
        })?;
        let completed =
            completed_regular_turns_after(snapshot, continuation_turn_id).ok_or_else(|| {
                Error::new(
                ErrorCode::RecoveryAmbiguous,
                "the previous continuation turn is not uniquely present in the bounded snapshot",
            )
            .component("orchestrator")
            })?;
        if completed < COOLDOWN_REGULAR_TURNS {
            return Err(Error::new(
                ErrorCode::CooldownActive,
                "3 completed regular turns are required after the last transition",
            )
            .component("orchestrator"));
        }
    }
    Ok(())
}

pub(super) async fn ensure_no_active_descendant(
    client: &AppServerClient,
    root_thread_id: &str,
) -> Result<()> {
    if active_descendant_exists(client, root_thread_id).await? {
        return Err(Error::new(
            ErrorCode::ActiveSubagents,
            "an active loaded descendant blocks compaction",
        )
        .component("orchestrator"));
    }
    Ok(())
}

async fn active_descendant_exists(client: &AppServerClient, root_thread_id: &str) -> Result<bool> {
    for thread_id in client.loaded_threads().await? {
        if thread_id == root_thread_id {
            continue;
        }
        let mut current = client.thread_read(&thread_id, false).await?;
        if !current.is_active() {
            continue;
        }

        let mut visited = HashSet::new();
        for depth in 0..32 {
            if !visited.insert(current.id.clone()) {
                return Err(Error::new(
                    ErrorCode::RecoveryAmbiguous,
                    "cycle detected while resolving an active thread parent chain",
                )
                .component("orchestrator"));
            }
            let Some(parent_id) = current.parent_thread_id.clone() else {
                break;
            };
            if parent_id == root_thread_id {
                return Ok(true);
            }
            current = client.thread_read(&parent_id, false).await?;
            if depth == 31 {
                return Err(Error::new(
                    ErrorCode::RecoveryAmbiguous,
                    "active thread parent chain exceeded the bounded depth",
                )
                .component("orchestrator"));
            }
        }
    }
    Ok(false)
}

pub(super) fn evidence_from_snapshot(snapshot: &ThreadRef, source_turn_id: &str) -> Evidence {
    let mut evidence = Evidence::default();
    for turn in &snapshot.turns {
        for item in &turn.items {
            for value in &item.safe_evidence {
                if item.item_type == "userMessage" || turn.id == source_turn_id {
                    evidence.observe_item(value);
                }
            }
        }
        if turn.id == source_turn_id {
            break;
        }
    }
    evidence.normalize();
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_subagent_snapshot() {
        let snapshot = ThreadRef::from_response(&json!({"thread":{
            "id":"child",
            "status":{"type":"active","activeFlags":[]},
            "parentThreadId":"root",
            "turns":[]
        }}))
        .unwrap();
        let error = preflight_root_and_cooldown(&snapshot, None).unwrap_err();
        assert_eq!(error.code, ErrorCode::NotRootThread);
    }
}
