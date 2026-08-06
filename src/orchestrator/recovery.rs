use super::{Orchestrator, runner};
use crate::app_server::AppServerClient;
use crate::doctor::require_ready_capabilities;
use crate::error::{Error, ErrorCode, Result};
use crate::journal::{TransitionJournal, TransitionState};
use crate::lease::ThreadLease;
use crate::protocol::ThreadRef;
use tracing::warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryDisposition {
    CancelBeforeMutation,
    ResumePersistedCheckpoint,
    ConfirmContinuation,
    FailAmbiguous,
    IgnoreTerminal,
}

/// Recovery replays only a checkpoint whose complete capsule was durably saved
/// before the journal entered `INJECTING_CHECKPOINT`. States at or after a
/// possibly accepted mutation never replay that mutation.
pub(super) async fn recover_nonterminal_journals(orchestrator: &Orchestrator) -> Result<usize> {
    let mut recovered = 0;
    for mut journal in orchestrator.journals.nonterminal()? {
        let lease = match ThreadLease::acquire(&journal.thread_id) {
            Ok(lease) => lease,
            Err(error) if error.code == ErrorCode::TransitionPending => continue,
            Err(error) => return Err(error),
        };

        let result = match recovery_disposition(&journal) {
            RecoveryDisposition::CancelBeforeMutation => {
                journal.cancel("process_restarted_before_compaction");
                Ok(())
            }
            RecoveryDisposition::ResumePersistedCheckpoint => {
                resume_persisted_checkpoint(orchestrator, &mut journal).await
            }
            RecoveryDisposition::ConfirmContinuation => confirm_continuation(&mut journal).await,
            RecoveryDisposition::FailAmbiguous => Err(Error::new(
                ErrorCode::RecoveryAmbiguous,
                "journal state cannot be recovered without replaying an ambiguous mutation",
            )
            .component("recovery")),
            RecoveryDisposition::IgnoreTerminal => {
                drop(lease);
                continue;
            }
        };

        if let Err(error) = result {
            if runner::is_cancellation_error(error.code) {
                journal.cancel(error.code.as_str());
            } else {
                journal.fail(error.code.as_str());
            }
        }
        orchestrator.journals.save(&journal)?;
        recovered += 1;
        warn!(
            state = ?journal.state,
            "recovered a non-terminal agentic-compact journal conservatively"
        );
        drop(lease);
    }
    Ok(recovered)
}

fn recovery_disposition(journal: &TransitionJournal) -> RecoveryDisposition {
    use RecoveryDisposition::*;
    use TransitionState::*;
    match journal.state {
        Attaching | AwaitSourceTurnCompleted | ReadyToCompact => CancelBeforeMutation,
        AwaitCompactionTurnCompleted if journal.checkpoint.is_some() => ResumePersistedCheckpoint,
        AwaitContinuationStarted if journal.continuation_turn_id.is_some() => ConfirmContinuation,
        CompactRequestSent
        | AwaitCompactionItem
        | AwaitCompactionTurnCompleted
        | InjectingCheckpoint
        | StartingContinuation
        | AwaitContinuationStarted => FailAmbiguous,
        Cooldown | Cancelled | FailedSafe => IgnoreTerminal,
    }
}

async fn resume_persisted_checkpoint(
    orchestrator: &Orchestrator,
    journal: &mut TransitionJournal,
) -> Result<()> {
    let checkpoint = journal.checkpoint.clone().ok_or_else(|| {
        Error::new(
            ErrorCode::RecoveryAmbiguous,
            "checkpoint capsule is missing before injection recovery",
        )
        .component("recovery")
    })?;
    journal.set_checkpoint(checkpoint.clone())?;

    let client = AppServerClient::connect_default().await?;
    let result = async {
        require_ready_capabilities(&client)?;
        let events = client.subscribe();
        let snapshot = client.thread_resume(&journal.thread_id).await?;
        if !safe_checkpoint_recovery_snapshot(journal, &snapshot.thread) {
            return Err(Error::new(
                ErrorCode::RaceLost,
                "thread changed after compact completion; checkpoint recovery yields to the user",
            )
            .component("recovery"));
        }
        runner::inject_and_continue(
            &orchestrator.journals,
            journal,
            &snapshot.thread.id,
            &client,
            events,
            checkpoint,
        )
        .await
    }
    .await;
    let _ = client.unsubscribe(&journal.thread_id).await;
    client.close().await;
    result
}

async fn confirm_continuation(journal: &mut TransitionJournal) -> Result<()> {
    let continuation_turn_id = journal.continuation_turn_id.as_deref().ok_or_else(|| {
        Error::new(
            ErrorCode::RecoveryAmbiguous,
            "continuation turn ID is missing during recovery",
        )
        .component("recovery")
    })?;
    let client = AppServerClient::connect_default().await?;
    let result = client.thread_read(&journal.thread_id, true).await;
    client.close().await;
    let thread = result?;
    if thread.unique_turn(continuation_turn_id).is_err() {
        return Err(Error::new(
            ErrorCode::RecoveryAmbiguous,
            "the exact acknowledged continuation turn is absent",
        )
        .component("recovery"));
    }
    journal.transition(
        TransitionState::Cooldown,
        "exact acknowledged continuation turn confirmed after restart",
    )
}

fn safe_checkpoint_recovery_snapshot(journal: &TransitionJournal, thread: &ThreadRef) -> bool {
    let Some(compact_turn_id) = journal.compact_turn_id.as_deref() else {
        return false;
    };
    runner::require_completed_compaction_boundary(thread, compact_turn_id).is_ok()
}

#[cfg(test)]
mod tests;
