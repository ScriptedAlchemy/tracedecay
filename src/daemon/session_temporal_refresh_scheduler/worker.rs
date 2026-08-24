use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::PoisonError;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tracedecay_store::{
    SessionRefreshCompletionRequestV1, SessionRefreshFailureRequestV1, SessionRefreshFrontierV1,
    SessionRefreshProgressV1, SessionRefreshStore, SessionStoreError,
};

use super::history::{SessionHistoricalIngestOutcome, SharedSessionHistoricalIngestor};
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
use crate::global_db::{RegisteredGlobalDb, RegisteredGlobalDbLeaseV1};
use crate::store::{
    GlobalDbSessionTemporalStore, SessionRefreshRecoveryV1, SessionRefreshRestartStateV1,
};

const HISTORY_IDLE_RECHECK_INTERVAL: Duration = Duration::from_secs(60);

#[hotpath::measure]
pub(super) async fn run_session_temporal_refresh_scheduler(
    database: RegisteredGlobalDbLeaseV1,
    state: Arc<SessionTemporalRefreshWakeState>,
    projector: Arc<dyn SessionTemporalRefreshProjector>,
    history: Arc<std::sync::RwLock<Option<SharedSessionHistoricalIngestor>>>,
    policy: SessionTemporalRefreshPolicy,
) {
    let mut retry_attempt = 0u32;
    let _instrumentation = SessionTemporalRefreshWorkerInstrumentation::new(&state);
    state.mark_running();
    loop {
        if state.cancelled.load(Ordering::Acquire) {
            return;
        }
        loop {
            let mut projection_requested = state.take_dirty();
            let history_requested = state.take_historical_dirty();
            if !projection_requested && !history_requested {
                break;
            }
            state.begin_pass();
            state.mark_worker_busy();
            state.pass_count.fetch_add(1, Ordering::AcqRel);
            let history_outcome = if history_requested {
                hotpath::future!(
                    session_history_refresh(&history),
                    label = "session_temporal_refresh.history"
                )
                .await
            } else {
                None
            };
            projection_requested |= state.take_dirty();
            if let Some(outcome) = history_outcome {
                state.record_history_outcome(outcome);
            }
            if matches!(
                history_outcome,
                Some(SessionHistoricalIngestOutcome::Cancelled)
            ) || state.cancelled.load(Ordering::Acquire)
            {
                return;
            }
            match history_outcome {
                Some(SessionHistoricalIngestOutcome::Retryable { reason_code, .. }) => {
                    tracing::debug!(
                        reason_code,
                        "retained historical session ingest pass will retry"
                    );
                }
                Some(SessionHistoricalIngestOutcome::Blocked { reason_code }) => {
                    tracing::warn!(reason_code, "retained historical session ingest is blocked");
                }
                Some(
                    SessionHistoricalIngestOutcome::Complete
                    | SessionHistoricalIngestOutcome::Pending { .. }
                    | SessionHistoricalIngestOutcome::Cancelled,
                )
                | None => {}
            }
            let history_requires_projection = match history_outcome {
                Some(SessionHistoricalIngestOutcome::Complete) => true,
                Some(
                    SessionHistoricalIngestOutcome::Pending { made_progress }
                    | SessionHistoricalIngestOutcome::Retryable { made_progress, .. },
                ) => made_progress,
                Some(
                    SessionHistoricalIngestOutcome::Blocked { .. }
                    | SessionHistoricalIngestOutcome::Cancelled,
                )
                | None => false,
            };
            let report =
                if projection_requested || state.has_requests() || history_requires_projection {
                    let pass = hotpath::future!(
                        session_projection_refresh(&database, &state, projector.as_ref(), policy),
                        label = "session_temporal_refresh.projection"
                    );
                    tokio::pin!(pass);
                    tokio::select! {
                        biased;
                        () = hotpath::future!(
                            state.wait_for_cancellation(),
                            label = "session_temporal_refresh.cancellation_wait"
                        ) => return,
                        report = &mut pass => report,
                    }
                } else {
                    SessionTemporalRefreshPassReport::default()
                };
            if state.cancelled.load(Ordering::Acquire) {
                return;
            }
            let made_progress = report.begun > 0
                || report.projected_batches > 0
                || report.completed > 0
                || report.failed > 0
                || report.cancelled > 0
                || history_outcome.is_some_and(SessionHistoricalIngestOutcome::made_progress);
            let history_needs_another_pass =
                history_outcome.is_some_and(SessionHistoricalIngestOutcome::needs_another_pass);
            observe_pass_report(
                &report,
                !made_progress && (report.retry_class.is_some() || history_needs_another_pass),
            );
            if let Some(backlog) = report.backlog {
                state.record_pass(
                    backlog.saturating_add(usize::from(history_needs_another_pass)),
                    made_progress,
                );
            }
            if let Some(class) = report.retry_class {
                retry_attempt = retry_attempt.saturating_add(1);
                observe_retry(class, retry_attempt);
                state.mark_recovering(class.into(), class);
                state.requeue_projection();
                let retry_delay = session_refresh_retry_delay(class, retry_attempt);
                tokio::select! {
                    () = hotpath::future!(
                        state.wait_for_cancellation(),
                        label = "session_temporal_refresh.cancellation_wait"
                    ) => return,
                    () = hotpath::future!(
                        state.wake.notified(),
                        label = "session_temporal_refresh.wake_wait"
                    ) => {}
                    () = hotpath::future!(
                        tokio::time::sleep(retry_delay),
                        label = "session_temporal_refresh.retry_wait"
                    ) => {}
                }
            } else if history_needs_another_pass {
                state.mark_running();
                retry_attempt = 0;
                state.update_history_retry_state(true);
            } else {
                if history_outcome.is_some() {
                    state.update_history_retry_state(false);
                }
                state.mark_running();
                retry_attempt = 0;
                if state.has_requests()
                    || report.begun > 0
                    || report.saturated
                    || report.projected_batches > 0
                {
                    state.requeue_projection();
                    tokio::task::yield_now().await;
                }
            }
        }
        state.mark_worker_idle();
        state.idle.notify_waiters();
        let wake = hotpath::future!(
            state.wake.notified(),
            label = "session_temporal_refresh.wake_wait"
        );
        if state.has_pending_work() {
            continue;
        }
        if state.history_retry_pending() {
            tokio::select! {
                () = hotpath::future!(
                    state.wait_for_cancellation(),
                    label = "session_temporal_refresh.cancellation_wait"
                ) => return,
                () = wake => {}
                () = hotpath::future!(
                    tokio::time::sleep(Duration::from_millis(250)),
                    label = "session_temporal_refresh.history_retry_wait"
                ) => {
                    state.update_history_retry_state(false);
                    state.wake_history();
                },
            }
        } else {
            tokio::select! {
                () = hotpath::future!(
                    state.wait_for_cancellation(),
                    label = "session_temporal_refresh.cancellation_wait"
                ) => return,
                () = wake => {}
                () = hotpath::future!(
                    tokio::time::sleep(HISTORY_IDLE_RECHECK_INTERVAL),
                    label = "session_temporal_refresh.history_idle_wait"
                ) => {
                    if history
                        .read()
                        .unwrap_or_else(PoisonError::into_inner)
                        .is_some()
                    {
                        state.wake_history();
                    }
                }
            }
        }
    }
}

struct SessionTemporalRefreshWorkerInstrumentation<'a> {
    state: &'a SessionTemporalRefreshWakeState,
}

impl<'a> SessionTemporalRefreshWorkerInstrumentation<'a> {
    fn new(state: &'a SessionTemporalRefreshWakeState) -> Self {
        hotpath::gauge!("session_temporal_refresh_workers_active").inc(1.0);
        Self { state }
    }
}

impl Drop for SessionTemporalRefreshWorkerInstrumentation<'_> {
    fn drop(&mut self) {
        stop_worker(self.state);
        hotpath::gauge!("session_temporal_refresh_workers_active").inc(-1.0);
    }
}

fn stop_worker(state: &SessionTemporalRefreshWakeState) {
    state.clear_worker_activity_instrumentation();
}

macro_rules! increment_outcome {
    ($key:literal, $count:expr) => {{
        let count = $count;
        if count > 0 {
            hotpath::gauge!($key).inc(count.min(u32::MAX as usize) as f64);
        }
    }};
}

fn observe_pass_report(report: &SessionTemporalRefreshPassReport, no_progress_retry: bool) {
    hotpath::gauge!("session_temporal_refresh_passes").inc(1.0);
    if no_progress_retry {
        hotpath::gauge!("session_temporal_refresh_no_progress_passes").inc(1.0);
    }
    increment_outcome!("session_temporal_refresh_begun", report.begun);
    increment_outcome!("session_temporal_refresh_joined", report.joined);
    increment_outcome!(
        "session_temporal_refresh_projected_batches",
        report.projected_batches
    );
    increment_outcome!("session_temporal_refresh_completed", report.completed);
    increment_outcome!("session_temporal_refresh_failed", report.failed);
    increment_outcome!("session_temporal_refresh_cancelled", report.cancelled);
    increment_outcome!("session_temporal_refresh_deferred", report.deferred);
    increment_outcome!(
        "session_temporal_refresh_retryable_errors",
        report.retryable_errors
    );
    increment_outcome!(
        "session_temporal_refresh_terminal_errors",
        report.terminal_errors
    );
    increment_outcome!(
        "session_temporal_refresh_deadline_errors",
        report.deadline_errors
    );
}

fn observe_retry(class: SessionTemporalRefreshRetryClass, attempt: u32) {
    match class {
        SessionTemporalRefreshRetryClass::Storage => {
            hotpath::gauge!("session_temporal_refresh_storage_retries").inc(1.0);
        }
        SessionTemporalRefreshRetryClass::Projector => {
            hotpath::gauge!("session_temporal_refresh_projector_retries").inc(1.0);
        }
        SessionTemporalRefreshRetryClass::Deadline => {
            hotpath::gauge!("session_temporal_refresh_deadline_retries").inc(1.0);
        }
    }
    hotpath::gauge!("session_temporal_refresh_last_retry_attempt").set(attempt);
}

#[hotpath::measure]
async fn session_history_refresh(
    history: &Arc<std::sync::RwLock<Option<SharedSessionHistoricalIngestor>>>,
) -> Option<SessionHistoricalIngestOutcome> {
    let history = history
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    match history {
        Some(history) => Some(history.run_pass().await),
        None => None,
    }
}

#[hotpath::measure]
async fn session_projection_refresh(
    database: &RegisteredGlobalDbLeaseV1,
    state: &Arc<SessionTemporalRefreshWakeState>,
    projector: &dyn SessionTemporalRefreshProjector,
    policy: SessionTemporalRefreshPolicy,
) -> SessionTemporalRefreshPassReport {
    run_session_temporal_refresh_pass(database, state, projector, policy).await
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

#[hotpath::measure]
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
    match store
        .complete_session_refresh(request, state.completion_control())
        .await
    {
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
                .persist_session_refresh_projection_batch_controlled(
                    progress,
                    batch,
                    state.completion_control(),
                )
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
        SessionTemporalRefreshEffect::Deferred => report.deferred += 1,
    }
}

async fn project_running_refresh(
    database: &RegisteredGlobalDbLeaseV1,
    store: &GlobalDbSessionTemporalStore<'_>,
    state: &SessionTemporalRefreshWakeState,
    projector: &dyn SessionTemporalRefreshProjector,
    policy: SessionTemporalRefreshPolicy,
    recovery: &SessionRefreshRecoveryV1,
    report: &mut SessionTemporalRefreshPassReport,
) {
    let deadline_at = tokio::time::Instant::now() + policy.operation_deadline;
    let projection = hotpath::future!(
        projector.project(database, recovery.clone()),
        label = "session_temporal_refresh.projector"
    );
    tokio::pin!(projection);
    let deadline = hotpath::future!(
        tokio::time::sleep_until(deadline_at),
        label = "session_temporal_refresh.operation_deadline"
    );
    tokio::pin!(deadline);
    let effect = tokio::select! {
        biased;
        () = hotpath::future!(
            state.wait_for_cancellation(),
            label = "session_temporal_refresh.cancellation_wait"
        ) => return,
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
    let deadline = hotpath::future!(
        tokio::time::sleep_until(deadline_at),
        label = "session_temporal_refresh.operation_deadline"
    );
    tokio::pin!(deadline);
    tokio::select! {
        biased;
        () = hotpath::future!(
            state.wait_for_cancellation(),
            label = "session_temporal_refresh.cancellation_wait"
        ) => {}
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

#[hotpath::measure]
pub(super) async fn run_session_temporal_refresh_pass(
    database: &RegisteredGlobalDbLeaseV1,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    use tempfile::TempDir;
    use tracedecay_domain::{SessionId, TemporalCoverageCountsV1, UtcMicros};
    use tracedecay_store::{SessionRefreshBeginOrJoinRequestV1, SessionTemporalProjectionBatchV1};
    use tracedecay_usecases::host_admission::HostAdmissionScope;

    use crate::host_admission::HostAdmissionTestRuntimeV1;

    #[test]
    fn dropping_worker_instrumentation_clears_pending_state_once() {
        let state = SessionTemporalRefreshWakeState::default();
        {
            let _instrumentation = SessionTemporalRefreshWorkerInstrumentation::new(&state);
            state.requeue_projection();
            state.wake_history();
            state.mark_worker_busy();
            state.update_history_retry_state(true);
        }

        assert!(state.dirty.load(Ordering::Acquire));
        assert!(state.has_pending_work());
        assert!(!state.busy.load(Ordering::Acquire));
        assert!(!state.history_retry_pending());

        state.cancel();
        assert!(!state.dirty.load(Ordering::Acquire));
        assert!(!state.has_pending_work());
    }

    #[tokio::test]
    async fn cancelled_worker_control_prevents_projection_batch_persistence() {
        let temp = TempDir::new().unwrap();
        let runtime = HostAdmissionTestRuntimeV1::profile(temp.path())
            .await
            .unwrap();
        let store = runtime
            .session_temporal_store_for_test(HostAdmissionScope::Profile)
            .unwrap();
        let session_id = SessionId::new("session.scheduler.cancelled-persistence").unwrap();
        let started = store
            .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
                session_id.clone(),
                SessionRefreshFrontierV1::new(0, 0).unwrap(),
            ))
            .await
            .unwrap();
        let recovery = store
            .session_refresh_recovery(&session_id)
            .await
            .unwrap()
            .unwrap();
        let progress = SessionRefreshProgressV1::new(
            started.operation_id().clone(),
            session_id.clone(),
            SessionRefreshFrontierV1::new(0, 0).unwrap(),
            TemporalCoverageCountsV1 {
                visible: 0,
                hidden: 0,
                unknown: 0,
                redacted: 0,
            },
            1,
            0,
            UtcMicros(
                i64::try_from(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_micros(),
                )
                .unwrap(),
            ),
        );
        let batch = SessionTemporalProjectionBatchV1::new(
            session_id.clone(),
            recovery.candidate_generation(),
            recovery.frozen_watermarks().clone(),
            vec![],
            vec![],
            vec![],
        )
        .unwrap()
        .with_checkpoint(0, 0, 0)
        .unwrap();
        let state = SessionTemporalRefreshWakeState::default();
        state.cancel();
        let mut report = SessionTemporalRefreshPassReport::default();

        apply_refresh_effect(
            &store,
            &state,
            &recovery,
            SessionTemporalRefreshEffect::Projection { progress, batch },
            &mut report,
        )
        .await;

        assert_eq!(report.projected_batches, 0);
        assert_eq!(report.terminal_errors, 1);
        assert_eq!(
            store
                .session_refresh_recovery(&session_id)
                .await
                .unwrap()
                .unwrap()
                .restart_state(),
            SessionRefreshRestartStateV1::BeginProjection
        );
    }
}
