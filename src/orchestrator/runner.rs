use super::{
    COMPACTION_TIMEOUT, CONTINUATION_START_TIMEOUT, SOURCE_TURN_TIMEOUT,
    ensure_no_active_descendant, evidence_from_snapshot,
};
use crate::app_server::AppServerClient;
use crate::checkpoint::{Checkpoint, Evidence};
use crate::error::{Error, ErrorCode, Result};
use crate::journal::{JournalStore, TransitionJournal, TransitionState};
use crate::metadata::BoundInvocation;
use crate::protocol::{AppEvent, ThreadRef, has_active_work_through_turn};
use std::collections::HashSet;
use tokio::sync::broadcast;
use tokio::time::timeout;

pub(super) async fn run_transition(
    journals: &JournalStore,
    journal: &mut TransitionJournal,
    bound: &BoundInvocation,
    client: &AppServerClient,
    mut events: broadcast::Receiver<AppEvent>,
    receipt_id: &str,
) -> Result<()> {
    await_source_turn(&mut events, bound).await?;

    reject_queued_competing_turn(&mut events, &bound.thread_id, &bound.turn_id)?;
    let thread = client.thread_read(&bound.thread_id, true).await?;
    let evidence = validate_final_source_snapshot(&thread, &bound.turn_id, receipt_id)?;
    ensure_no_active_descendant(client, &bound.thread_id).await?;
    reject_queued_competing_turn(&mut events, &bound.thread_id, &bound.turn_id)?;

    journal.transition(
        TransitionState::ReadyToCompact,
        "final full snapshot proved a completed quiescent source boundary",
    )?;
    journals.save(journal)?;

    journal.transition(
        TransitionState::CompactRequestSent,
        "persisted before thread/compact/start; request is never blindly retried",
    )?;
    journals.save(journal)?;
    client
        .compact_start(&bound.thread_id)
        .await
        .map_err(|error| {
            Error::new(
                ErrorCode::CompactionFailed,
                format!("thread/compact/start failed or is ambiguous: {error}"),
            )
            .component("orchestrator")
            .retryable(false)
        })?;

    let compact_turn_id = await_compaction(
        &mut events,
        &bound.thread_id,
        &bound.turn_id,
        journal,
        journals,
    )
    .await?;

    reject_queued_competing_turn(&mut events, &bound.thread_id, &compact_turn_id)?;
    let post_compact = client.thread_read(&bound.thread_id, true).await?;
    require_completed_compaction_boundary(&post_compact, &compact_turn_id)?;

    let checkpoint = Checkpoint::build(
        journal.checkpoint_id.clone(),
        journal.receipt_id.clone(),
        bound.thread_id.clone(),
        bound.turn_id.clone(),
        compact_turn_id,
        journal.intent.clone(),
        evidence,
    )?;
    journal.set_checkpoint(checkpoint.clone())?;
    journals.save(journal)?;

    inject_and_continue(
        journals,
        journal,
        &bound.thread_id,
        client,
        events,
        checkpoint,
    )
    .await
}

pub(super) async fn inject_and_continue(
    journals: &JournalStore,
    journal: &mut TransitionJournal,
    thread_id: &str,
    client: &AppServerClient,
    mut events: broadcast::Receiver<AppEvent>,
    checkpoint: Checkpoint,
) -> Result<()> {
    if drain_started_turn(&mut events, thread_id)?.is_some() {
        return Err(Error::new(
            ErrorCode::RaceLost,
            "another turn started before checkpoint injection",
        )
        .component("orchestrator"));
    }
    ensure_no_active_descendant(client, thread_id).await?;
    if drain_started_turn(&mut events, thread_id)?.is_some() {
        return Err(Error::new(
            ErrorCode::RaceLost,
            "another turn started during the checkpoint injection guard",
        )
        .component("orchestrator"));
    }
    journal.transition(
        TransitionState::InjectingCheckpoint,
        "checkpoint capsule persisted before injection; ambiguous injection is not retried",
    )?;
    journals.save(journal)?;
    client
        .inject_checkpoint(thread_id, &checkpoint)
        .await
        .map_err(|error| {
            Error::new(
                ErrorCode::InjectionFailed,
                format!("checkpoint injection failed or is ambiguous: {error}"),
            )
            .component("orchestrator")
            .retryable(false)
        })?;

    journal.transition(
        TransitionState::StartingContinuation,
        "checkpoint injection acknowledged",
    )?;
    journals.save(journal)?;

    if let Some(user_turn_id) = drain_started_turn(&mut events, thread_id)? {
        return complete_user_wins(journals, journal, user_turn_id);
    }

    let continuation_turn_id = match client.start_empty_turn(thread_id).await {
        Ok(turn_id) => turn_id,
        Err(error) => {
            if error.rpc_code.is_some() {
                if let Some(user_turn_id) = drain_started_turn(&mut events, thread_id)? {
                    return complete_user_wins(journals, journal, user_turn_id);
                }
            }
            return Err(Error::new(
                ErrorCode::ContinuationUnsupported,
                format!("empty continuation failed or is ambiguous: {error}"),
            )
            .component("orchestrator")
            .retryable(false));
        }
    };
    journal.set_continuation_turn(continuation_turn_id.clone())?;
    journal.transition(
        TransitionState::AwaitContinuationStarted,
        "empty continuation RPC acknowledged",
    )?;
    journals.save(journal)?;
    if let Err(event_error) =
        await_continuation_started(&mut events, thread_id, &continuation_turn_id).await
    {
        if event_error.code != ErrorCode::RecoveryAmbiguous {
            return Err(event_error);
        }
        let confirmed = client
            .thread_read(thread_id, true)
            .await
            .is_ok_and(|thread| thread.unique_turn(&continuation_turn_id).is_ok());
        if !confirmed {
            return Err(event_error);
        }
    }
    journal.transition(
        TransitionState::Cooldown,
        "same-thread empty continuation started",
    )?;
    journals.save(journal)?;
    Ok(())
}

fn complete_user_wins(
    journals: &JournalStore,
    journal: &mut TransitionJournal,
    user_turn_id: String,
) -> Result<()> {
    journal.set_continuation_turn(user_turn_id)?;
    journal.transition(
        TransitionState::Cooldown,
        "user-started turn won after checkpoint injection and consumes the checkpoint",
    )?;
    journals.save(journal)
}

pub(super) fn is_cancellation_error(code: ErrorCode) -> bool {
    matches!(
        code,
        ErrorCode::RaceLost
            | ErrorCode::QuiescenceViolation
            | ErrorCode::SourceTurnFailed
            | ErrorCode::ActiveSubagents
            | ErrorCode::RecentNativeCompaction
            | ErrorCode::ActiveWork
    )
}

async fn await_source_turn(
    events: &mut broadcast::Receiver<AppEvent>,
    bound: &BoundInvocation,
) -> Result<()> {
    timeout(SOURCE_TURN_TIMEOUT, async {
        loop {
            let event = recv_event(events).await?;
            match event {
                AppEvent::TurnCompleted {
                    thread_id, turn, ..
                } if thread_id == bound.thread_id && turn.id == bound.turn_id => {
                    if turn.status != "completed" {
                        return Err(Error::new(
                            ErrorCode::SourceTurnFailed,
                            "source turn did not complete successfully",
                        )
                        .component("orchestrator"));
                    }
                    return Ok(());
                }
                AppEvent::TurnStarted { thread_id, turn }
                    if thread_id == bound.thread_id && turn.id != bound.turn_id =>
                {
                    return Err(Error::new(
                        ErrorCode::RaceLost,
                        "another turn started before the source turn completed",
                    )
                    .component("orchestrator"));
                }
                AppEvent::ServerRequest { .. } => {
                    return Err(Error::new(
                        ErrorCode::ServerRequestReceived,
                        "control connection received a server-initiated request",
                    )
                    .component("orchestrator"));
                }
                AppEvent::ConnectionClosed { .. } => {
                    return Err(Error::new(
                        ErrorCode::SharedAppServerUnavailable,
                        "app-server connection closed while awaiting source completion",
                    )
                    .component("orchestrator"));
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| Error::timeout("orchestrator", "source turn completion timed out"))?
}

async fn await_compaction(
    events: &mut broadcast::Receiver<AppEvent>,
    thread_id: &str,
    source_turn_id: &str,
    journal: &mut TransitionJournal,
    journals: &JournalStore,
) -> Result<String> {
    timeout(COMPACTION_TIMEOUT, async {
        let mut candidate_turn: Option<String> = None;
        let mut compaction_item: Option<String> = None;
        let mut completed_compaction_items = HashSet::new();
        loop {
            let event = recv_event(events).await?;
            match event {
                AppEvent::TurnStarted {
                    thread_id: event_thread,
                    turn,
                } if event_thread == thread_id && turn.id != source_turn_id => {
                    if let Some(existing) = candidate_turn.as_deref() {
                        if existing == turn.id {
                            continue;
                        }
                        return Err(Error::new(
                            ErrorCode::RaceLost,
                            "another turn started before compaction was uniquely bound",
                        )
                        .component("orchestrator"));
                    }
                    candidate_turn = Some(turn.id.clone());
                    journal.set_compact_turn(turn.id)?;
                    journal.transition(
                        TransitionState::AwaitCompactionItem,
                        "candidate compact turn started",
                    )?;
                    journals.save(journal)?;
                }
                AppEvent::ItemStarted {
                    thread_id: event_thread,
                    turn_id,
                    item,
                } if event_thread == thread_id
                    && candidate_turn.as_deref() == Some(turn_id.as_str()) =>
                {
                    if item.item_type == "contextCompaction" {
                        match compaction_item.as_deref() {
                            Some(existing) if existing == item.id => {}
                            Some(_) => {
                                return Err(Error::new(
                                    ErrorCode::RecoveryAmbiguous,
                                    "candidate turn contains multiple compaction items",
                                )
                                .component("orchestrator"));
                            }
                            None => compaction_item = Some(item.id),
                        }
                    } else if !item.is_allowed_in_compaction_turn() {
                        return Err(Error::new(
                            ErrorCode::RaceLost,
                            "candidate turn is not a pure compaction turn",
                        )
                        .component("orchestrator"));
                    }
                }
                AppEvent::ItemCompleted {
                    thread_id: event_thread,
                    turn_id,
                    item,
                } if event_thread == thread_id
                    && candidate_turn.as_deref() == Some(turn_id.as_str()) =>
                {
                    if item.item_type == "contextCompaction" {
                        if !item.completed_successfully() {
                            return Err(Error::new(
                                ErrorCode::CompactionFailed,
                                "contextCompaction item did not complete successfully",
                            )
                            .component("orchestrator"));
                        }
                        match compaction_item.as_deref() {
                            Some(existing) if existing == item.id => {}
                            Some(_) => {
                                return Err(Error::new(
                                    ErrorCode::RecoveryAmbiguous,
                                    "candidate turn completed a second compaction item",
                                )
                                .component("orchestrator"));
                            }
                            None => {
                                return Err(Error::new(
                                    ErrorCode::RecoveryAmbiguous,
                                    "contextCompaction completed before its start event",
                                )
                                .component("orchestrator"));
                            }
                        }
                        if completed_compaction_items.insert(item.id) {
                            journal.transition(
                                TransitionState::AwaitCompactionTurnCompleted,
                                "contextCompaction item completed",
                            )?;
                            journals.save(journal)?;
                        }
                    } else if !item.is_allowed_in_compaction_turn() {
                        return Err(Error::new(
                            ErrorCode::RaceLost,
                            "candidate turn completed an unrelated item",
                        )
                        .component("orchestrator"));
                    }
                }
                AppEvent::TurnCompleted {
                    thread_id: event_thread,
                    turn,
                } if event_thread == thread_id
                    && candidate_turn.as_deref() == Some(turn.id.as_str()) =>
                {
                    if turn.status != "completed" {
                        return Err(Error::new(
                            ErrorCode::CompactionFailed,
                            "compaction turn did not complete successfully",
                        )
                        .component("orchestrator"));
                    }
                    let Some(item_id) = compaction_item.as_deref() else {
                        return Err(Error::new(
                            ErrorCode::RecoveryAmbiguous,
                            "compaction turn contained no contextCompaction item",
                        )
                        .component("orchestrator"));
                    };
                    if completed_compaction_items.len() != 1
                        || !completed_compaction_items.contains(item_id)
                    {
                        return Err(Error::new(
                            ErrorCode::CompactionFailed,
                            "contextCompaction item did not complete exactly once",
                        )
                        .component("orchestrator"));
                    }
                    return Ok(turn.id);
                }
                AppEvent::ServerRequest { .. } => {
                    return Err(Error::new(
                        ErrorCode::ServerRequestReceived,
                        "control connection received a server-initiated request",
                    )
                    .component("orchestrator"));
                }
                AppEvent::ConnectionClosed { .. } => {
                    return Err(Error::new(
                        ErrorCode::RecoveryAmbiguous,
                        "app-server connection closed during compaction",
                    )
                    .component("orchestrator"));
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| Error::timeout("orchestrator", "compaction lifecycle timed out"))?
}

async fn await_continuation_started(
    events: &mut broadcast::Receiver<AppEvent>,
    thread_id: &str,
    continuation_turn_id: &str,
) -> Result<()> {
    timeout(CONTINUATION_START_TIMEOUT, async {
        loop {
            match recv_event(events).await? {
                AppEvent::TurnStarted {
                    thread_id: event_thread,
                    turn,
                } if event_thread == thread_id && turn.id == continuation_turn_id => {
                    return Ok(());
                }
                AppEvent::TurnStarted {
                    thread_id: event_thread,
                    turn,
                } if event_thread == thread_id && turn.id != continuation_turn_id => {
                    return Err(Error::new(
                        ErrorCode::RaceLost,
                        "a different turn started before the acknowledged continuation",
                    )
                    .component("orchestrator"));
                }
                AppEvent::ServerRequest { .. } => {
                    return Err(Error::new(
                        ErrorCode::ServerRequestReceived,
                        "control connection received a server-initiated request",
                    )
                    .component("orchestrator"));
                }
                AppEvent::ConnectionClosed { .. } => {
                    return Err(Error::new(
                        ErrorCode::RecoveryAmbiguous,
                        "app-server connection closed before continuation start",
                    )
                    .component("orchestrator"));
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| Error::timeout("orchestrator", "continuation start timed out"))?
}

fn is_passive_source_item(item_type: &str) -> bool {
    matches!(item_type, "agentMessage" | "reasoning")
}

fn validate_final_source_snapshot(
    thread: &ThreadRef,
    source_turn_id: &str,
    receipt_id: &str,
) -> Result<Evidence> {
    let source = thread.ensure_exact_last_turn(source_turn_id)?;
    if source.status != "completed" || !thread.is_idle() {
        return Err(Error::new(
            ErrorCode::RaceLost,
            "thread is not idle with a completed source turn at the compaction boundary",
        )
        .component("orchestrator"));
    }
    if source.is_compaction() {
        return Err(Error::new(
            ErrorCode::RecentNativeCompaction,
            "source turn already performed native context compaction",
        )
        .component("orchestrator"));
    }
    if has_active_work_through_turn(thread, source_turn_id)? {
        return Err(Error::new(
            ErrorCode::ActiveWork,
            "active tool work remains in the final source snapshot",
        )
        .component("orchestrator"));
    }

    let mut requests = source
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.is_request_compaction_call());
    let (request_index, request) = requests.next().ok_or_else(|| {
        Error::new(
            ErrorCode::Protocol,
            "final source snapshot is missing request_compaction",
        )
        .component("orchestrator")
    })?;
    if requests.next().is_some() {
        return Err(Error::new(
            ErrorCode::RecoveryAmbiguous,
            "final source snapshot contains multiple request_compaction items",
        )
        .component("orchestrator"));
    }
    if !request.completed_successfully() {
        return Err(Error::new(
            ErrorCode::SourceTurnFailed,
            "request_compaction tool item did not complete successfully",
        )
        .component("orchestrator"));
    }
    if request.receipt_id.as_deref() != Some(receipt_id) {
        return Err(Error::new(
            ErrorCode::Protocol,
            "request_compaction result receipt did not match the schedule",
        )
        .component("orchestrator"));
    }
    if source.items[request_index + 1..]
        .iter()
        .any(|item| !is_passive_source_item(&item.item_type))
    {
        return Err(Error::new(
            ErrorCode::QuiescenceViolation,
            "source turn contains non-passive work after request_compaction",
        )
        .component("orchestrator"));
    }
    evidence_from_snapshot(thread, source_turn_id)
}

pub(super) fn require_completed_compaction_boundary(
    thread: &ThreadRef,
    compact_turn_id: &str,
) -> Result<()> {
    let compact = thread.ensure_exact_last_turn(compact_turn_id)?;
    if !thread.is_idle() || !compact.is_completed_pure_compaction() {
        return Err(Error::new(
            ErrorCode::CompactionFailed,
            "thread is not idle with one completed contextCompaction item",
        )
        .component("orchestrator"));
    }
    Ok(())
}

fn reject_queued_competing_turn(
    events: &mut broadcast::Receiver<AppEvent>,
    thread_id: &str,
    allowed_turn_id: &str,
) -> Result<()> {
    loop {
        match events.try_recv() {
            Ok(AppEvent::TurnStarted {
                thread_id: event_thread,
                turn,
            }) if event_thread == thread_id && turn.id != allowed_turn_id => {
                return Err(
                    Error::new(ErrorCode::RaceLost, "a competing turn was already queued")
                        .component("orchestrator"),
                );
            }
            Ok(AppEvent::ServerRequest { .. }) => {
                return Err(Error::new(
                    ErrorCode::ServerRequestReceived,
                    "control connection received a server-initiated request",
                )
                .component("orchestrator"));
            }
            Ok(AppEvent::ConnectionClosed { .. }) => {
                return Err(Error::new(
                    ErrorCode::SharedAppServerUnavailable,
                    "app-server connection closed at a transition boundary",
                )
                .component("orchestrator"));
            }
            Ok(_) => {}
            Err(broadcast::error::TryRecvError::Empty) => return Ok(()),
            Err(broadcast::error::TryRecvError::Closed) => {
                return Err(Error::new(
                    ErrorCode::SharedAppServerUnavailable,
                    "app-server event stream closed",
                )
                .component("orchestrator"));
            }
            Err(broadcast::error::TryRecvError::Lagged(_)) => {
                return Err(Error::new(
                    ErrorCode::RecoveryAmbiguous,
                    "app-server event stream lagged at a transition boundary",
                )
                .component("orchestrator"));
            }
        }
    }
}

fn drain_started_turn(
    events: &mut broadcast::Receiver<AppEvent>,
    thread_id: &str,
) -> Result<Option<String>> {
    let mut started = None;
    loop {
        match events.try_recv() {
            Ok(AppEvent::TurnStarted {
                thread_id: event_thread,
                turn,
            }) if event_thread == thread_id => {
                if started.replace(turn.id).is_some() {
                    return Err(Error::new(
                        ErrorCode::RecoveryAmbiguous,
                        "multiple turns started after checkpoint injection",
                    )
                    .component("orchestrator"));
                }
            }
            Ok(AppEvent::ServerRequest { .. }) => {
                return Err(Error::new(
                    ErrorCode::ServerRequestReceived,
                    "control connection received a server-initiated request",
                )
                .component("orchestrator"));
            }
            Ok(AppEvent::ConnectionClosed { .. }) => {
                return Err(Error::new(
                    ErrorCode::RecoveryAmbiguous,
                    "app-server connection closed after checkpoint injection",
                )
                .component("orchestrator"));
            }
            Ok(_) => {}
            Err(broadcast::error::TryRecvError::Empty) => return Ok(started),
            Err(broadcast::error::TryRecvError::Closed) => {
                return Err(Error::new(
                    ErrorCode::SharedAppServerUnavailable,
                    "app-server event stream closed",
                )
                .component("orchestrator"));
            }
            Err(broadcast::error::TryRecvError::Lagged(_)) => {
                return Err(Error::new(
                    ErrorCode::RecoveryAmbiguous,
                    "app-server event stream lagged after checkpoint injection",
                )
                .component("orchestrator"));
            }
        }
    }
}

async fn recv_event(events: &mut broadcast::Receiver<AppEvent>) -> Result<AppEvent> {
    events.recv().await.map_err(|error| match error {
        broadcast::error::RecvError::Closed => Error::new(
            ErrorCode::SharedAppServerUnavailable,
            "app-server event stream closed",
        )
        .component("orchestrator"),
        broadcast::error::RecvError::Lagged(skipped) => Error::new(
            ErrorCode::RecoveryAmbiguous,
            format!("app-server event stream lagged by {skipped} messages"),
        )
        .component("orchestrator"),
    })
}

#[cfg(test)]
mod tests;
