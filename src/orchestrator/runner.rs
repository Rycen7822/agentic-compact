use super::{
    COMPACTION_TIMEOUT, CONTINUATION_START_TIMEOUT, SOURCE_TURN_TIMEOUT,
    ensure_no_active_descendant, evidence_from_snapshot,
};
use crate::app_server::AppServerClient;
use crate::checkpoint::{Checkpoint, Evidence};
use crate::error::{Error, ErrorCode, Result};
use crate::journal::{JournalStore, TransitionJournal, TransitionState};
use crate::metadata::BoundInvocation;
use crate::protocol::{AppEvent, ResumeSnapshot, ThreadRef};
use std::collections::HashSet;
use tokio::sync::broadcast;
use tokio::time::timeout;

pub(super) async fn run_transition(
    journals: &JournalStore,
    journal: &mut TransitionJournal,
    bound: &BoundInvocation,
    snapshot: &ResumeSnapshot,
    client: &AppServerClient,
    mut events: broadcast::Receiver<AppEvent>,
    receipt_id: &str,
) -> Result<()> {
    let mut evidence = evidence_from_snapshot(&snapshot.thread, &bound.turn_id);
    await_source_turn(&mut events, bound, receipt_id, &mut evidence).await?;

    journal.transition(
        TransitionState::ReadyToCompact,
        "source turn completed and request item remained quiescent",
    )?;
    journals.save(journal)?;

    reject_queued_competing_turn(&mut events, &bound.thread_id, &bound.turn_id)?;
    let thread = client.thread_read(&bound.thread_id, true).await?;
    let source = thread.find_turn(&bound.turn_id).ok_or_else(|| {
        Error::new(
            ErrorCode::RecoveryAmbiguous,
            "source turn is absent from the final pre-compaction snapshot",
        )
        .component("orchestrator")
    })?;
    if source.status != "completed" || !thread.is_idle() {
        return Err(Error::new(
            ErrorCode::RaceLost,
            "thread is not idle with a completed source turn at the compaction boundary",
        )
        .component("orchestrator"));
    }
    ensure_no_active_descendant(client, &bound.thread_id).await?;

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
    if !post_compact.is_idle() {
        return Err(Error::new(
            ErrorCode::RaceLost,
            "thread stopped being idle before checkpoint injection",
        )
        .component("orchestrator"));
    }

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
        journal.set_continuation_turn(user_turn_id)?;
        journal.transition(
            TransitionState::Cooldown,
            "user-started turn won after checkpoint injection and consumes the checkpoint",
        )?;
        journals.save(journal)?;
        return Ok(());
    }

    let continuation_turn_id = client.start_empty_turn(thread_id).await.map_err(|error| {
        Error::new(
            ErrorCode::ContinuationUnsupported,
            format!("empty continuation failed or is ambiguous: {error}"),
        )
        .component("orchestrator")
        .retryable(false)
    })?;
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
            .is_ok_and(|thread| contains_exact_turn(&thread, &continuation_turn_id));
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

pub(super) fn is_cancellation_error(code: ErrorCode) -> bool {
    matches!(
        code,
        ErrorCode::RaceLost
            | ErrorCode::QuiescenceViolation
            | ErrorCode::SourceTurnFailed
            | ErrorCode::ActiveSubagents
    )
}

async fn await_source_turn(
    events: &mut broadcast::Receiver<AppEvent>,
    bound: &BoundInvocation,
    receipt_id: &str,
    evidence: &mut Evidence,
) -> Result<()> {
    timeout(SOURCE_TURN_TIMEOUT, async {
        let mut request_item_id: Option<String> = None;
        loop {
            let event = recv_event(events).await?;
            match event {
                AppEvent::RequestCompactionResultInvalid { thread_id, turn_id }
                    if thread_id == bound.thread_id && turn_id == bound.turn_id =>
                {
                    return Err(Error::new(
                        ErrorCode::Protocol,
                        "request_compaction result metadata is invalid",
                    )
                    .component("orchestrator"));
                }
                AppEvent::ItemCompleted {
                    thread_id,
                    turn_id,
                    item,
                } if thread_id == bound.thread_id && turn_id == bound.turn_id => {
                    if request_item_id.is_none() && item.is_request_compaction_call() {
                        if item.receipt_id.as_deref() != Some(receipt_id) {
                            return Err(Error::new(
                                ErrorCode::Protocol,
                                "request_compaction result receipt did not match the schedule",
                            )
                            .component("orchestrator"));
                        }
                        if !item.completed_successfully() {
                            return Err(Error::new(
                                ErrorCode::SourceTurnFailed,
                                "request_compaction tool item did not complete successfully",
                            )
                            .component("orchestrator"));
                        }
                        request_item_id = Some(item.id.clone());
                        continue;
                    }
                    if request_item_id.is_some() && !is_passive_source_item(&item.item_type) {
                        return Err(Error::new(
                            ErrorCode::QuiescenceViolation,
                            "source turn completed another item after request_compaction",
                        )
                        .component("orchestrator"));
                    }
                    for value in &item.safe_evidence {
                        evidence.observe_item(value);
                    }
                }
                AppEvent::ItemStarted {
                    thread_id,
                    turn_id,
                    item,
                } if thread_id == bound.thread_id
                    && turn_id == bound.turn_id
                    && request_item_id.is_some()
                    && !is_passive_source_item(&item.item_type) =>
                {
                    return Err(Error::new(
                        ErrorCode::QuiescenceViolation,
                        "source turn started another item after request_compaction",
                    )
                    .component("orchestrator"));
                }
                AppEvent::TurnCompleted {
                    thread_id, turn, ..
                } if thread_id == bound.thread_id && turn.id == bound.turn_id => {
                    if request_item_id.is_none() {
                        return Err(Error::new(
                            ErrorCode::RecoveryAmbiguous,
                            "source turn completed before the request tool item was identified",
                        )
                        .component("orchestrator"));
                    }
                    if turn.status != "completed" {
                        return Err(Error::new(
                            ErrorCode::SourceTurnFailed,
                            "source turn did not complete successfully",
                        )
                        .component("orchestrator"));
                    }
                    evidence.normalize();
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

fn contains_exact_turn(thread: &ThreadRef, turn_id: &str) -> bool {
    thread.turns.iter().any(|turn| turn.id == turn_id)
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
