mod recovery;
mod runner;

use crate::app_server::AppServerClient;
use crate::checkpoint::{CompactionIntent, Evidence};
use crate::doctor::require_ready_capabilities;
use crate::error::{Error, ErrorCode, Result};
use crate::journal::{JournalStore, TransitionJournal, TransitionState};
use crate::lease::ThreadLease;
use crate::metadata::BoundInvocation;
use crate::observability::hash_identifier;
use crate::protocol::{ResumeSnapshot, ThreadRef, completed_regular_turns_after};
use dashmap::DashSet;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{Instant, timeout, timeout_at};
use tracing::{info, warn};
use uuid::Uuid;

pub(super) const SOURCE_TURN_TIMEOUT: Duration = Duration::from_secs(15 * 60);
pub(super) const COMPACTION_TIMEOUT: Duration = Duration::from_secs(5 * 60);
pub(super) const CONTINUATION_START_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const PREPARE_TIMEOUT: Duration = Duration::from_secs(5);
const REJECTED_PREPARATION_CLEANUP_TIMEOUT: Duration = Duration::from_millis(250);
pub(super) const COOLDOWN_REGULAR_TURNS: usize = 3;

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

#[derive(Debug, Clone)]
pub struct ScheduleResult {
    pub receipt_id: String,
}

impl Orchestrator {
    pub fn new() -> Result<Self> {
        Ok(Self {
            registry: TransitionRegistry::default(),
            journals: JournalStore::open()?,
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

        let deadline = Instant::now() + PREPARE_TIMEOUT;
        let client = match timeout_at(deadline, AppServerClient::connect_default()).await {
            Ok(Ok(client)) => client,
            Ok(Err(error)) => return Err(error),
            Err(_) => {
                return Err(Error::timeout(
                    "orchestrator",
                    "same-thread attach exceeded the 5 second MCP scheduling deadline",
                ));
            }
        };
        let preparation = timeout_at(deadline, async {
            require_ready_capabilities(&client)?;
            let events = client.subscribe();
            let snapshot = client.thread_resume(&bound.thread_id).await?;
            validate_resume_binding(&bound, &snapshot)?;
            preflight_root_and_cooldown(&snapshot.thread, prior.as_ref())?;
            ensure_no_active_descendant(&client, &bound.thread_id).await?;
            Ok::<_, Error>((events, snapshot))
        })
        .await;

        let (events, snapshot) = match preparation {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(error)) => {
                close_rejected_preparation(&client, &bound.thread_id).await;
                return Err(error);
            }
            Err(_) => {
                close_rejected_preparation(&client, &bound.thread_id).await;
                return Err(Error::timeout(
                    "orchestrator",
                    "same-thread attach exceeded the 5 second MCP scheduling deadline",
                ));
            }
        };

        let receipt_id = format!("rcpt_{}", Uuid::new_v4().simple());
        let checkpoint_id = format!("cp_{}", Uuid::new_v4().simple());
        let accepted = (|| {
            let mut journal = TransitionJournal::new(
                bound.thread_id.clone(),
                bound.turn_id.clone(),
                receipt_id.clone(),
                checkpoint_id.clone(),
                intent,
            )?;
            self.journals.save(&journal)?;
            journal.transition(
                TransitionState::AwaitSourceTurnCompleted,
                "same-thread subscription and resume acknowledged",
            )?;
            self.journals.save(&journal)?;
            Ok::<_, Error>(journal)
        })();
        let mut journal = match accepted {
            Ok(journal) => journal,
            Err(error) => {
                close_rejected_preparation(&client, &bound.thread_id).await;
                return Err(error);
            }
        };

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
        Ok(ScheduleResult { receipt_id })
    }

    pub async fn recover_nonterminal_journals(&self) -> Result<usize> {
        recovery::recover_nonterminal_journals(self).await
    }
}

async fn close_rejected_preparation(client: &AppServerClient, thread_id: &str) {
    let _ = timeout(
        REJECTED_PREPARATION_CLEANUP_TIMEOUT,
        client.unsubscribe(thread_id),
    )
    .await;
    client.close().await;
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
        let completed = completed_regular_turns_after(snapshot, continuation_turn_id)?;
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
        let snapshot = ThreadRef::from_response(
            &json!({"thread":{
                "id":"child",
                "status":{"type":"active","activeFlags":[]},
                "parentThreadId":"root",
                "turns":[]
            }}),
            true,
        )
        .unwrap();
        let error = preflight_root_and_cooldown(&snapshot, None).unwrap_err();
        assert_eq!(error.code, ErrorCode::NotRootThread);
    }

    #[test]
    fn cooldown_anchor_must_be_unique_before_counting_later_turns() {
        let mut journal = TransitionJournal::new(
            "thread".to_owned(),
            "source".to_owned(),
            "receipt".to_owned(),
            "checkpoint".to_owned(),
            CompactionIntent {
                preserve: Vec::new(),
                next_action: "continue".to_owned(),
            },
        )
        .unwrap();
        journal.state = TransitionState::Cooldown;
        journal.continuation_turn_id = Some("continuation".to_owned());
        let mut snapshot = ThreadRef::from_response(
            &json!({"thread": {
                "id": "thread",
                "status": "idle",
                "turns": [
                    {"id": "continuation", "status": "completed", "items": []},
                    {"id": "one", "status": "completed", "items": []},
                    {"id": "two", "status": "completed", "items": []},
                    {"id": "three", "status": "completed", "items": []}
                ]
            }}),
            true,
        )
        .unwrap();
        assert!(preflight_root_and_cooldown(&snapshot, Some(&journal)).is_ok());

        snapshot.turns.push(snapshot.turns[0].clone());
        assert_eq!(
            preflight_root_and_cooldown(&snapshot, Some(&journal))
                .unwrap_err()
                .code,
            ErrorCode::RecoveryAmbiguous
        );

        journal.continuation_turn_id = Some("missing".to_owned());
        assert_eq!(
            preflight_root_and_cooldown(&snapshot, Some(&journal))
                .unwrap_err()
                .code,
            ErrorCode::RecoveryAmbiguous
        );
    }
}
