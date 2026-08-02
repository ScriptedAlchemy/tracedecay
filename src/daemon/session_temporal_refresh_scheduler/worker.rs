use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::PoisonError;
use std::sync::atomic::Ordering;

use tracedecay_store::{
    SessionRefreshCompletionRequestV1, SessionRefreshFailureRequestV1, SessionRefreshFrontierV1,
    SessionRefreshProgressV1, SessionRefreshStore, SessionStoreError,
};

use super::projector::{
    SessionTemporalRefreshEffect, SessionTemporalRefreshPolicy, SessionTemporalRefreshProjector,
    SessionTemporalRefreshProjectorError, SessionTemporalRefreshProjectorErrorClass,
    durable_projector_failure_code, zero_refresh_coverage,
};
use super::registry::{SessionTemporalRefreshPassReport, session_refresh_retry_delay};
use super::wake::{
    PendingBeginRequestGuard, RecoverySelectionGuard, SessionTemporalRefreshRetryClass,
    SessionTemporalRefreshWakeState, TerminalAttemptGuard,
};
use crate::global_db::RegisteredGlobalDb;
use crate::store::{
    GlobalDbSessionTemporalStore, SessionRefreshRecoveryV1, SessionRefreshRestartStateV1,
};

pub(super) async fn run_session_temporal_refresh_scheduler(
    database: Arc<RegisteredGlobalDb>,
    state: Arc<SessionTemporalRefreshWakeState>,
    projector: Arc<dyn SessionTemporalRefreshProjector>,
    policy: SessionTemporalRefreshPolicy,
) {
    let mut retry_attempt = 0u32;
    state.mark_running();
    loop {
        if state.cancelled.load(Ordering::Acquire) {
            return;
        }
        while state.take_dirty() {
            state.begin_pass();
            state.busy.store(true, Ordering::Release);
            state.pass_count.fetch_add(1, Ordering::AcqRel);
            let pass =
                run_session_temporal_refresh_pass(&database, &state, projector.as_ref(), policy);
            tokio::pin!(pass);
            let report = tokio::select! {
                biased;
                () = state.wait_for_cancellation() => return,
                report = &mut pass => report,
            };
            if state.cancelled.load(Ordering::Acquire) {
                return;
            }
            let made_progress = report.begun > 0
                || report.projected_batches > 0
                || report.completed > 0
                || report.failed > 0
                || report.cancelled > 0;
            if let Some(backlog) = report.backlog {
                state.record_pass(backlog, made_progress);
            }
            if let Some(class) = report.retry_class {
                retry_attempt = retry_attempt.saturating_add(1);
                state.mark_recovering(class.into(), class);
                state.dirty.store(true, Ordering::Release);
                tokio::select! {
                    () = state.wait_for_cancellation() => return,
                    () = state.wake.notified() => {}
                    () = tokio::time::sleep(session_refresh_retry_delay(class, retry_attempt)) => {}
                }
            } else {
                state.mark_running();
                retry_attempt = 0;
                if state.has_requests()
                    || report.begun > 0
                    || report.saturated
                    || report.projected_batches > 0
                {
                    state.dirty.store(true, Ordering::Release);
                    tokio::task::yield_now().await;
                }
            }
        }
        state.busy.store(false, Ordering::Release);
        state.idle.notify_waiters();
        let wake = state.wake.notified();
        if state.dirty.load(Ordering::Acquire) {
            continue;
        }
        tokio::select! {
            () = state.wait_for_cancellation() => return,
            () = wake => {}
        }
    }
}

fn classify_store_error(error: &SessionStoreError) -> SessionTemporalRefreshRetryClass {
    if error.is_storage() {
        SessionTemporalRefreshRetryClass::Storage
    } else {
        SessionTemporalRefreshRetryClass::Projector
    }
}

async fn process_refresh_begin_requests(
    store: &GlobalDbSessionTemporalStore<'_>,
    state: &SessionTemporalRefreshWakeState,
    limit: usize,
    report: &mut SessionTemporalRefreshPassReport,
) {
    for _ in 0..limit {
        let Some(request) = state.take_requests(1).pop() else {
            break;
        };
        let mut pending = PendingBeginRequestGuard::new(state, request);
        if state.cancelled.load(Ordering::Acquire) {
            return;
        }
        match store
            .begin_or_join_session_refresh(pending.request().clone())
            .await
        {
            Ok(receipt) => {
                pending.disarm();
                match receipt.disposition() {
                    tracedecay_store::SessionRefreshDispositionV1::Started => report.begun += 1,
                    tracedecay_store::SessionRefreshDispositionV1::Joined => report.joined += 1,
                }
            }
            Err(error) if error.is_storage() => {
                report.retryable_errors += 1;
                report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
                break;
            }
            Err(_) => {
                pending.disarm();
                report.terminal_errors += 1;
            }
        }
    }
    report.saturated |= state.has_requests();
}

async fn begin_admitted_session_refreshes(
    database: &RegisteredGlobalDb,
    store: &GlobalDbSessionTemporalStore<'_>,
    state: &SessionTemporalRefreshWakeState,
    limit: usize,
    report: &mut SessionTemporalRefreshPassReport,
) {
    let mut requests = match database
        .pending_session_temporal_refresh_requests_result(limit.saturating_add(1))
        .await
    {
        Ok(requests) => requests,
        Err(error) => {
            if classify_store_error(&error) == SessionTemporalRefreshRetryClass::Storage {
                report.retryable_errors += 1;
                report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
            } else {
                report.terminal_errors += 1;
            }
            return;
        }
    };
    report.saturated |= requests.len() > limit;
    requests.truncate(limit);
    for request in requests {
        if state.cancelled.load(Ordering::Acquire) {
            return;
        }
        match store.begin_or_join_session_refresh(request).await {
            Ok(receipt) => match receipt.disposition() {
                tracedecay_store::SessionRefreshDispositionV1::Started => report.begun += 1,
                tracedecay_store::SessionRefreshDispositionV1::Joined => report.joined += 1,
            },
            Err(error) if error.is_storage() => {
                report.retryable_errors += 1;
                report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
            }
            Err(_) => report.terminal_errors += 1,
        }
    }
}

async fn complete_ready_refresh(
    store: &GlobalDbSessionTemporalStore<'_>,
    state: &SessionTemporalRefreshWakeState,
    recovery: &SessionRefreshRecoveryV1,
    report: &mut SessionTemporalRefreshPassReport,
) {
    if !state.claim_terminal_attempt(recovery) {
        return;
    }
    let mut attempt = TerminalAttemptGuard::new(state, recovery);
    let Some(progress) = recovery.progress() else {
        attempt.retain();
        report.terminal_errors += 1;
        return;
    };
    let request = if let Ok(request) = SessionRefreshCompletionRequestV1::new(
        recovery.operation_id().clone(),
        recovery.session_id().clone(),
        progress.frontier(),
        *progress.coverage(),
    ) {
        match progress.source_coverage().cloned() {
            Some(source_coverage) => request.with_source_coverage(source_coverage),
            None => request,
        }
    } else {
        attempt.retain();
        report.terminal_errors += 1;
        return;
    };
    match store.complete_session_refresh(request).await {
        Ok(_) => {
            report.completed += 1;
        }
        Err(error) if error.is_storage() => {
            report.last_error = Some(format!("{error:?}"));
            report.retryable_errors += 1;
            report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
        }
        Err(error) => {
            attempt.retain();
            report.last_error = Some(format!("{error:?}"));
            report.terminal_errors += 1;
        }
    }
}

fn record_projector_error(
    error: SessionTemporalRefreshProjectorError,
    report: &mut SessionTemporalRefreshPassReport,
) {
    report.last_error = Some(error.code);
    match error.class {
        SessionTemporalRefreshProjectorErrorClass::Retryable => {
            report.retryable_errors += 1;
            report.observe_retry(SessionTemporalRefreshRetryClass::Projector);
        }
        SessionTemporalRefreshProjectorErrorClass::Terminal => {
            report.terminal_errors += 1;
        }
    }
}

async fn apply_refresh_effect(
    store: &GlobalDbSessionTemporalStore<'_>,
    state: &SessionTemporalRefreshWakeState,
    recovery: &SessionRefreshRecoveryV1,
    effect: SessionTemporalRefreshEffect,
    report: &mut SessionTemporalRefreshPassReport,
) {
    match effect {
        SessionTemporalRefreshEffect::Projection { progress, batch } => {
            match store
                .persist_session_refresh_projection_batch(progress, batch)
                .await
            {
                Ok(_) => report.projected_batches += 1,
                Err(error) if error.is_storage() => {
                    report.last_error = Some(format!("{error:?}"));
                    report.retryable_errors += 1;
                    report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
                }
                Err(error) => {
                    report.last_error = Some(format!("{error:?}"));
                    report.terminal_errors += 1;
                }
            }
        }
        SessionTemporalRefreshEffect::Fail(request) => {
            if !state.claim_terminal_attempt(recovery) {
                return;
            }
            let mut attempt = TerminalAttemptGuard::new(state, recovery);
            match store.fail_session_refresh(request).await {
                Ok(_) => {
                    report.failed += 1;
                }
                Err(error) if error.is_storage() => {
                    report.last_error = Some(format!("{error:?}"));
                    report.retryable_errors += 1;
                    report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
                }
                Err(error) => {
                    attempt.retain();
                    report.last_error = Some(format!("{error:?}"));
                    report.terminal_errors += 1;
                }
            }
        }
        SessionTemporalRefreshEffect::Cancel(request) => {
            if !state.claim_terminal_attempt(recovery) {
                return;
            }
            let mut attempt = TerminalAttemptGuard::new(state, recovery);
            match store.cancel_session_refresh(request).await {
                Ok(_) => {
                    report.cancelled += 1;
                }
                Err(error) if error.is_storage() => {
                    report.last_error = Some(format!("{error:?}"));
                    report.retryable_errors += 1;
                    report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
                }
                Err(error) => {
                    attempt.retain();
                    report.last_error = Some(format!("{error:?}"));
                    report.terminal_errors += 1;
                }
            }
        }
        SessionTemporalRefreshEffect::Deferred => report.deferred += 1,
    }
}

async fn project_running_refresh(
    database: &Arc<RegisteredGlobalDb>,
    store: &GlobalDbSessionTemporalStore<'_>,
    state: &SessionTemporalRefreshWakeState,
    projector: &dyn SessionTemporalRefreshProjector,
    policy: SessionTemporalRefreshPolicy,
    recovery: &SessionRefreshRecoveryV1,
    report: &mut SessionTemporalRefreshPassReport,
) {
    let deadline_at = tokio::time::Instant::now() + policy.operation_deadline;
    let projection = projector.project(database, recovery.clone());
    tokio::pin!(projection);
    let deadline = tokio::time::sleep_until(deadline_at);
    tokio::pin!(deadline);
    let effect = tokio::select! {
        biased;
        () = state.wait_for_cancellation() => return,
        () = &mut deadline => {
            report.deadline_errors += 1;
            report.observe_retry(SessionTemporalRefreshRetryClass::Deadline);
            return;
        }
        effect = &mut projection => effect,
    };
    let effect = match effect {
        Ok(effect) => effect,
        Err(error) if error.class == SessionTemporalRefreshProjectorErrorClass::Retryable => {
            record_projector_error(error, report);
            return;
        }
        Err(error) => {
            let failure_code = durable_projector_failure_code(&error.code);
            report.last_error = Some(failure_code.clone());
            let (frontier, coverage) = if let Some(progress) = recovery.progress() {
                (progress.frontier(), *progress.coverage())
            } else {
                let Ok(frontier) = SessionRefreshFrontierV1::new(
                    recovery.target_frontier().observed_through(),
                    recovery.source_frontier(),
                ) else {
                    report.terminal_errors += 1;
                    return;
                };
                (frontier, zero_refresh_coverage())
            };
            let request = if let Ok(request) = SessionRefreshFailureRequestV1::new(
                recovery.operation_id().clone(),
                recovery.session_id().clone(),
                frontier,
                coverage,
                failure_code,
            ) {
                match recovery
                    .progress()
                    .and_then(SessionRefreshProgressV1::source_coverage)
                    .cloned()
                    .or_else(|| recovery.source_coverage(frontier.committed_through()).ok())
                {
                    Some(source_coverage) => request.with_source_coverage(source_coverage),
                    None => request,
                }
            } else {
                report.terminal_errors += 1;
                return;
            };
            SessionTemporalRefreshEffect::Fail(request)
        }
    };
    if state.cancelled.load(Ordering::Acquire) {
        return;
    }
    let deadline = tokio::time::sleep_until(deadline_at);
    tokio::pin!(deadline);
    tokio::select! {
        biased;
        () = state.wait_for_cancellation() => {}
        () = &mut deadline => {
            report.deadline_errors += 1;
            report.observe_retry(SessionTemporalRefreshRetryClass::Deadline);
        }
        () = apply_refresh_effect(store, state, recovery, effect, report) => {}
    }
}

fn recovery_key(recovery: &SessionRefreshRecoveryV1) -> String {
    format!(
        "{}\0{}",
        recovery.session_id().as_str(),
        recovery.operation_id().as_str()
    )
}

pub(super) async fn run_session_temporal_refresh_pass(
    database: &Arc<RegisteredGlobalDb>,
    state: &Arc<SessionTemporalRefreshWakeState>,
    projector: &dyn SessionTemporalRefreshProjector,
    policy: SessionTemporalRefreshPolicy,
) -> SessionTemporalRefreshPassReport {
    let store = GlobalDbSessionTemporalStore::new(database.as_ref());
    let mut report = SessionTemporalRefreshPassReport::default();
    if state.cancelled.load(Ordering::Acquire) {
        return report;
    }
    process_refresh_begin_requests(
        &store,
        state,
        policy.max_begin_requests_per_pass,
        &mut report,
    )
    .await;
    begin_admitted_session_refreshes(
        database,
        &store,
        state,
        policy.max_begin_requests_per_pass,
        &mut report,
    )
    .await;
    let mut recoveries = match store.running_session_refreshes().await {
        Ok(recoveries) => recoveries,
        Err(error) => {
            if classify_store_error(&error) == SessionTemporalRefreshRetryClass::Storage {
                report.retryable_errors += 1;
                report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
            } else {
                report.terminal_errors += 1;
            }
            return report;
        }
    };
    recoveries.sort_by_cached_key(recovery_key);
    state.observe_durable_backlog(recoveries.len());
    let ordered_keys = recoveries.iter().map(recovery_key).collect::<Vec<_>>();
    let current_keys = ordered_keys.iter().cloned().collect::<HashSet<_>>();
    let (selected_keys, recoveries_remaining) = {
        let mut pending = state
            .recovery_cycle_pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        pending.retain(|operation| current_keys.contains(operation));
        if pending.is_empty() {
            pending.extend(ordered_keys);
        }
        let limit = policy.max_operations_per_pass.max(1);
        let mut selected = Vec::with_capacity(limit.min(pending.len()));
        for _ in 0..limit {
            let Some(operation) = pending.pop_front() else {
                break;
            };
            selected.push(operation);
        }
        let remaining = !pending.is_empty();
        (selected, remaining)
    };
    let mut recoveries_by_key = recoveries
        .into_iter()
        .map(|recovery| (recovery_key(&recovery), recovery))
        .collect::<HashMap<_, _>>();
    let mut selection = RecoverySelectionGuard::new(state, selected_keys.clone());
    let selected = selected_keys
        .into_iter()
        .filter_map(|operation| recoveries_by_key.remove(&operation))
        .collect::<Vec<_>>();
    let selected_count = selected.len();
    report.saturated |= recoveries_remaining;
    for recovery in selected {
        let operation = recovery_key(&recovery);
        if state.cancelled.load(Ordering::Acquire) {
            return report;
        }
        match recovery.restart_state() {
            SessionRefreshRestartStateV1::ReadyToComplete => {
                if tokio::time::timeout(
                    policy.operation_deadline,
                    complete_ready_refresh(&store, state, &recovery, &mut report),
                )
                .await
                .is_err()
                {
                    report.deadline_errors += 1;
                    report.observe_retry(SessionTemporalRefreshRetryClass::Deadline);
                }
            }
            SessionRefreshRestartStateV1::BeginProjection
            | SessionRefreshRestartStateV1::ResumeProjection { .. } => {
                project_running_refresh(
                    database,
                    &store,
                    state,
                    projector,
                    policy,
                    &recovery,
                    &mut report,
                )
                .await;
            }
        }
        selection.complete(&operation);
    }
    let terminal = report
        .completed
        .saturating_add(report.failed)
        .saturating_add(report.cancelled);
    report.backlog = Some(
        recoveries_by_key
            .len()
            .saturating_add(selected_count.saturating_sub(terminal)),
    );
    report
}
