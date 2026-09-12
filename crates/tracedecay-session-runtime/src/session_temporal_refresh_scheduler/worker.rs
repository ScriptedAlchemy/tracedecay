use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::PoisonError;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tracedecay_lcm::LcmError;
use tracedecay_store::{
    SessionRefreshCompletionRequestV1, SessionRefreshFailureRequestV1, SessionRefreshFrontierV1,
    SessionRefreshProgressV1, SessionRefreshStore, SessionStoreError,
};

use super::history::{
    SessionHistoricalIngestOutcome, SessionHistoricalIngestProgress,
    SharedSessionHistoricalIngestor,
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
use tracedecay_global_db::{RegisteredGlobalDb, RegisteredGlobalDbLeaseV1};
use tracedecay_session_temporal_store::{
    SessionRefreshRecoveryV1, SessionRefreshRestartStateV1, SessionTemporalStore,
};

const HISTORY_IDLE_RECHECK_INTERVAL: Duration = Duration::from_mins(1);

fn history_allows_summary_convergence(outcome: Option<SessionHistoricalIngestOutcome>) -> bool {
    !outcome.is_some_and(SessionHistoricalIngestOutcome::needs_another_pass)
}

/// Consecutive history-priority passes after which the one-shot
/// predecessor-range rewrite takes one bounded page of its own.
///
/// This is the horizon the rewrite buys under perpetually pending history:
/// one `LCM_SCAN_PAGE_ROWS`-row page every eighth pass, so an N-row store
/// converges in `ceil(N / LCM_SCAN_PAGE_ROWS) * 8` worker passes (about 3,800
/// for the 244k-row profile in #843) while history keeps seven of every eight
/// passes. It is a fairness ratio, not a deadline; a terminal history window
/// resets it and admits the full convergence page, which also runs the rewrite.
const HISTORY_PRIORITY_PASSES_BEFORE_RANGE_REWRITE: u32 = 8;

/// What a pass may spend its historical-work admission on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LcmConvergenceAdmission {
    /// Historical continuation owns the pass outright.
    Deferred,
    /// The pass takes the shared admission permit for one bounded page.
    Admitted(LcmConvergencePage),
}

/// Which bounded page an admitted pass runs under the shared permit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LcmConvergencePage {
    /// Historical continuation still owns the next window, but the one-shot
    /// predecessor-range rewrite runs alone for one bounded page.
    PredecessorRangeRewrite,
    /// The raw frontier is terminal for now, so derived convergence runs.
    Full,
}

/// Decides what one pass owes retained LCM convergence.
///
/// Deferring derived summaries to historical continuation is deliberate: a
/// model call placed between source windows delays both project and profile
/// readiness. The one-shot predecessor-range rewrite lives behind the same
/// convergence page but is bounded SQL with no model call, so a profile whose
/// history perpetually needs another pass would otherwise never repair a
/// range persisted before the policy-anchor role filter. Every
/// `HISTORY_PRIORITY_PASSES_BEFORE_RANGE_REWRITE`-th such pass therefore
/// spends its admission — the same permit and bounded budget one history page
/// takes — on the rewrite alone, which caps the rewrite's starvation at that
/// many passes per page while leaving history the other passes.
fn lcm_convergence_admission(
    outcome: Option<SessionHistoricalIngestOutcome>,
    history_priority_passes: &mut u32,
) -> LcmConvergenceAdmission {
    if history_allows_summary_convergence(outcome) {
        *history_priority_passes = 0;
        return LcmConvergenceAdmission::Admitted(LcmConvergencePage::Full);
    }
    *history_priority_passes = history_priority_passes.saturating_add(1);
    if *history_priority_passes >= HISTORY_PRIORITY_PASSES_BEFORE_RANGE_REWRITE {
        *history_priority_passes = 0;
        return LcmConvergenceAdmission::Admitted(LcmConvergencePage::PredecessorRangeRewrite);
    }
    LcmConvergenceAdmission::Deferred
}

/// Typed deferral reported when the daemon-wide historical-ingest admission
/// has no free permit. The worker retries after the history-retry delay while
/// projection serving continues unblocked.
pub(super) const HISTORY_ADMISSION_SATURATED_REASON: &str = "history_admission_saturated";

pub(super) async fn run_session_temporal_refresh_scheduler(
    database: RegisteredGlobalDbLeaseV1,
    state: Arc<SessionTemporalRefreshWakeState>,
    projector: Arc<dyn SessionTemporalRefreshProjector>,
    history: Arc<std::sync::RwLock<Option<SharedSessionHistoricalIngestor>>>,
    history_admission: Arc<tokio::sync::Semaphore>,
    policy: SessionTemporalRefreshPolicy,
) {
    let mut retry_attempt = 0u32;
    let mut summary_retry_attempt = 0u32;
    let mut history_priority_passes = 0u32;
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
            let history_sequence = history_requested.then(|| state.history_requested_sequence());
            let history_result = if history_requested {
                Some(
                    hotpath::future!(
                        session_history_refresh(&history, &history_admission),
                        label = "daemon.scheduler.session_temporal.history"
                    )
                    .await,
                )
            } else {
                None
            };
            let history_outcome = history_result.map(|result| result.0);
            projection_requested |= state.take_dirty();
            if let Some((outcome, progress)) = history_result {
                state.record_history_progress(progress);
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
                Some(SessionHistoricalIngestOutcome::Blocked { reason_code, .. }) => {
                    tracing::warn!(reason_code, "retained historical session ingest is blocked");
                }
                Some(
                    SessionHistoricalIngestOutcome::Complete
                    | SessionHistoricalIngestOutcome::Pending { .. }
                    | SessionHistoricalIngestOutcome::Cancelled,
                )
                | None => {}
            }
            let history_requires_projection = matches!(
                history_outcome,
                Some(SessionHistoricalIngestOutcome::Complete)
            ) || history_outcome
                .is_some_and(SessionHistoricalIngestOutcome::made_progress);
            let report =
                if projection_requested || state.has_requests() || history_requires_projection {
                    let pass = hotpath::future!(
                        session_projection_refresh(&database, &state, projector.as_ref(), policy),
                        label = "daemon.scheduler.session_temporal.projection"
                    );
                    tokio::pin!(pass);
                    tokio::select! {
                        biased;
                        () = hotpath::future!(
                            state.wait_for_cancellation(),
                            label = "daemon.scheduler.session_temporal.projection_cancel"
                        ) => return,
                        report = &mut pass => report,
                    }
                } else {
                    SessionTemporalRefreshPassReport::default()
                };
            if state.cancelled.load(Ordering::Acquire) {
                return;
            }
            if let (Some(sequence), Some(outcome)) = (history_sequence, history_outcome)
                && matches!(
                    outcome,
                    SessionHistoricalIngestOutcome::Complete
                        | SessionHistoricalIngestOutcome::Blocked { .. }
                )
            {
                state.complete_history_sequence(sequence);
            }
            let convergence_admission =
                lcm_convergence_admission(history_outcome, &mut history_priority_passes);
            // Derived from the admission so the pass report can never disagree
            // with what this pass actually ran.
            let history_needs_another_pass = !matches!(
                convergence_admission,
                LcmConvergenceAdmission::Admitted(LcmConvergencePage::Full)
            );
            let (
                summary_convergence_made_progress,
                summary_convergence_has_more,
                summary_retry_delay,
            ) = match convergence_admission {
                LcmConvergenceAdmission::Deferred => {
                    // Historical continuation owns the next bounded pass. LCM
                    // summaries are independent derived work and can run after
                    // the raw frontier is terminal; placing a model call between
                    // source windows delays both project and profile readiness.
                    (false, false, None)
                }
                LcmConvergenceAdmission::Admitted(convergence_page) => {
                    // Queue through the semaphore's fair async admission even when a
                    // permit appears immediately available. A retrying profile must
                    // not use `try_acquire` to jump ahead of profiles already waiting
                    // for the shared historical-work budget.
                    let admission = history_admission.acquire();
                    tokio::pin!(admission);
                    let registered = tokio::select! {
                        biased;
                        () = hotpath::future!(
                            state.wait_for_cancellation(),
                            label = "daemon.scheduler.lcm_summary.admission_cancel"
                        ) => return,
                        permit = &mut admission => Some(permit),
                        () = tokio::task::yield_now() => None,
                    };
                    let summary_admission = if let Some(permit) = registered {
                        permit
                    } else {
                        // The acquisition future has now been polled and joined the
                        // semaphore's FIFO queue. Only then advertise idle so an
                        // observer cannot release permits before this worker is
                        // registered to receive one.
                        hotpath::gauge!("session_temporal_refresh_history_admission_deferrals")
                            .inc(1.0);
                        state.mark_worker_idle();
                        state.idle.notify_waiters();
                        let permit = tokio::select! {
                            biased;
                            () = hotpath::future!(
                                state.wait_for_cancellation(),
                                label = "daemon.scheduler.lcm_summary.admission_cancel"
                            ) => return,
                            permit = &mut admission => permit,
                        };
                        state.mark_worker_busy();
                        permit
                    };
                    let Ok(summary_admission) = summary_admission else {
                        tracing::warn!(
                            "retained LCM summary convergence admission closed; worker stopped"
                        );
                        return;
                    };
                    let summary_result = {
                        let permit = summary_admission;
                        let page = async {
                            match convergence_page {
                            LcmConvergencePage::PredecessorRangeRewrite => {
                                crate::lcm_summary_convergence::run_predecessor_range_rewrite_page(
                                    database.clone(),
                                )
                                .await
                            }
                            LcmConvergencePage::Full => {
                                crate::lcm_summary_convergence::run_summary_convergence_page(
                                    database.clone(),
                                    crate::lcm_summary_convergence::LCM_SUMMARY_CONVERGENCE_PAGE_LIMIT,
                                )
                                .await
                            }
                        }
                        };
                        tokio::pin!(page);
                        let result = tokio::select! {
                            biased;
                            () = hotpath::future!(
                                state.wait_for_cancellation(),
                                label = "daemon.scheduler.lcm_summary.cancel"
                            ) => return,
                            result = &mut page => result,
                        };
                        drop(permit);
                        result
                    };
                    match summary_result {
                        Ok(page) => {
                            summary_retry_attempt = 0;
                            (
                                !page.sessions.is_empty()
                                    || page.backfill_rows_scanned > 0
                                    || page.predecessor_range_rows_rewritten > 0
                                    || page.relation_receipts_processed > 0,
                                page.has_more,
                                page.next_retry_delay,
                            )
                        }
                        Err(LcmError::Cancelled) => return,
                        Err(error @ LcmError::ProfileResetRequired { .. }) => {
                            tracing::error!(
                                %error,
                                "retained LCM summary convergence is permanently blocked"
                            );
                            (false, false, None)
                        }
                        Err(error) => {
                            let class = if matches!(error, LcmError::DeadlineExceeded) {
                                SessionTemporalRefreshRetryClass::Deadline
                            } else {
                                SessionTemporalRefreshRetryClass::Storage
                            };
                            summary_retry_attempt = summary_retry_attempt.saturating_add(1);
                            tracing::warn!(
                                %error,
                                ?class,
                                "retained LCM summary convergence page will retry"
                            );
                            (
                                false,
                                false,
                                Some(session_refresh_retry_delay(class, summary_retry_attempt)),
                            )
                        }
                    }
                }
            };
            if state.cancelled.load(Ordering::Acquire) {
                return;
            }
            let made_progress = report.begun > 0
                || report.projected_batches > 0
                || report.completed > 0
                || report.failed > 0
                || report.cancelled > 0
                || history_outcome.is_some_and(SessionHistoricalIngestOutcome::made_progress)
                || summary_convergence_made_progress;
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
                        label = "daemon.scheduler.session_temporal.retry_cancel"
                    ) => return,
                    () = hotpath::future!(
                        state.wake.notified(),
                        label = "daemon.scheduler.session_temporal.wake_wait"
                    ) => {}
                    () = hotpath::future!(
                        tokio::time::sleep(retry_delay),
                        label = "daemon.scheduler.session_temporal.retry_wait"
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
                    || summary_convergence_has_more
                {
                    state.requeue_projection();
                    tokio::task::yield_now().await;
                } else if let Some(delay) = summary_retry_delay {
                    state.requeue_projection();
                    tokio::select! {
                        () = hotpath::future!(
                            state.wait_for_cancellation(),
                            label = "daemon.scheduler.lcm_summary.retry_cancel"
                        ) => return,
                        () = hotpath::future!(
                            state.wake.notified(),
                            label = "daemon.scheduler.lcm_summary.retry_wake"
                        ) => {}
                        () = hotpath::future!(
                            tokio::time::sleep(delay),
                            label = "daemon.scheduler.lcm_summary.retry_wait"
                        ) => {}
                    }
                }
            }
        }
        state.mark_worker_idle();
        state.idle.notify_waiters();
        let wake = hotpath::future!(
            state.wake.notified(),
            label = "daemon.scheduler.session_temporal.wake_wait"
        );
        if state.has_pending_work() {
            continue;
        }
        if state.history_retry_pending() {
            tokio::select! {
                () = hotpath::future!(
                    state.wait_for_cancellation(),
                    label = "daemon.scheduler.session_temporal.history_retry_cancel"
                ) => return,
                () = wake => {}
                () = hotpath::future!(
                    tokio::time::sleep(Duration::from_millis(250)),
                    label = "daemon.scheduler.session_temporal.history_retry_wait"
                ) => {
                    state.update_history_retry_state(false);
                    state.wake_history();
                },
            }
        } else {
            tokio::select! {
                () = hotpath::future!(
                    state.wait_for_cancellation(),
                    label = "daemon.scheduler.session_temporal.idle_cancel"
                ) => return,
                () = wake => {}
                () = hotpath::future!(
                    tokio::time::sleep(HISTORY_IDLE_RECHECK_INTERVAL),
                    label = "daemon.scheduler.session_temporal.history_idle_wait"
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

/// Runs one historical ingest pass under the daemon-wide bounded admission.
///
/// The permit is held for the whole pass, so at most
/// `MAX_CONCURRENT_HISTORICAL_INGEST_PASSES` passes run concurrently across
/// every mounted project and the profile. A saturated admission defers this
/// worker's pass as typed retryable state rather than queueing behind it:
/// the worker's projection serving continues, and the history-retry wait
/// re-attempts admission shortly after.
async fn session_history_refresh(
    history: &Arc<std::sync::RwLock<Option<SharedSessionHistoricalIngestor>>>,
    admission: &tokio::sync::Semaphore,
) -> (
    SessionHistoricalIngestOutcome,
    SessionHistoricalIngestProgress,
) {
    let history = history
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    match history {
        Some(history) => {
            let Ok(_permit) = admission.try_acquire() else {
                hotpath::gauge!("session_temporal_refresh_history_admission_deferrals").inc(1.0);
                return (
                    SessionHistoricalIngestOutcome::Retryable {
                        reason_code: HISTORY_ADMISSION_SATURATED_REASON,
                        made_progress: false,
                    },
                    SessionHistoricalIngestProgress::default(),
                );
            };
            let outcome = history.run_pass().await;
            (outcome, history.take_progress())
        }
        None => (
            SessionHistoricalIngestOutcome::Blocked {
                reason_code: "history_ingestor_missing",
                made_progress: false,
            },
            SessionHistoricalIngestProgress::default(),
        ),
    }
}

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

pub async fn process_refresh_begin_requests(
    store: &SessionTemporalStore<'_, tracedecay_global_db::RegisteredGlobalDb>,
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

#[hotpath::measure(
    label = "daemon.scheduler.session_temporal.begin_admitted",
    future = true
)]
pub async fn begin_admitted_session_refreshes(
    database: &RegisteredGlobalDb,
    store: &SessionTemporalStore<'_, tracedecay_global_db::RegisteredGlobalDb>,
    state: &SessionTemporalRefreshWakeState,
    limit: usize,
    report: &mut SessionTemporalRefreshPassReport,
) {
    if state.has_requests() {
        report.saturated = true;
        return;
    }
    let active_after = state.projection_discovery_after();
    let active_scan_slots = state.projection_discovery_active_slots(limit);
    let page = match database
        .pending_session_temporal_refresh_page_result(
            limit,
            active_scan_slots,
            active_after.as_ref(),
        )
        .await
    {
        Ok(page) => page,
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
    let (requests, active_scanned_through, has_more) = page.into_parts();
    for request in requests.into_iter().rev() {
        state.requeue_request(request);
    }
    state.update_projection_discovery_cursor(active_scanned_through);
    report.saturated |= has_more;
    process_refresh_begin_requests(store, state, limit, report).await;
}

async fn complete_ready_refresh(
    store: &SessionTemporalStore<'_, tracedecay_global_db::RegisteredGlobalDb>,
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

pub async fn apply_refresh_effect(
    store: &SessionTemporalStore<'_, tracedecay_global_db::RegisteredGlobalDb>,
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
    store: &SessionTemporalStore<'_, tracedecay_global_db::RegisteredGlobalDb>,
    state: &SessionTemporalRefreshWakeState,
    projector: &dyn SessionTemporalRefreshProjector,
    policy: SessionTemporalRefreshPolicy,
    recovery: &SessionRefreshRecoveryV1,
    report: &mut SessionTemporalRefreshPassReport,
) {
    let deadline_at = tokio::time::Instant::now() + policy.operation_deadline;
    let projection = hotpath::future!(
        projector.project(database, recovery.clone()),
        label = "daemon.scheduler.session_temporal.projector"
    );
    tokio::pin!(projection);
    let deadline = hotpath::future!(
        tokio::time::sleep_until(deadline_at),
        label = "daemon.scheduler.session_temporal.projector_deadline"
    );
    tokio::pin!(deadline);
    let effect = tokio::select! {
        biased;
        () = hotpath::future!(
            state.wait_for_cancellation(),
            label = "daemon.scheduler.session_temporal.projector_cancel"
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
        label = "daemon.scheduler.session_temporal.effect_apply_deadline"
    );
    tokio::pin!(deadline);
    tokio::select! {
        biased;
        () = hotpath::future!(
            state.wait_for_cancellation(),
            label = "daemon.scheduler.session_temporal.effect_apply_cancel"
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

pub async fn run_session_temporal_refresh_pass(
    database: &RegisteredGlobalDbLeaseV1,
    state: &Arc<SessionTemporalRefreshWakeState>,
    projector: &dyn SessionTemporalRefreshProjector,
    policy: SessionTemporalRefreshPolicy,
) -> SessionTemporalRefreshPassReport {
    let store = SessionTemporalStore::new(database.as_ref());
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
    use tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness;
    use tracedecay_runtime_core::db::engine::params;
    use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};
    use tracedecay_store::ParseOffset;

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

    #[test]
    fn pending_history_windows_take_precedence_over_derived_summaries() {
        assert!(!history_allows_summary_convergence(Some(
            SessionHistoricalIngestOutcome::Pending {
                made_progress: true,
            },
        )));
        assert!(!history_allows_summary_convergence(Some(
            SessionHistoricalIngestOutcome::Retryable {
                reason_code: "provider_busy",
                made_progress: false,
            },
        )));
        assert!(history_allows_summary_convergence(Some(
            SessionHistoricalIngestOutcome::Complete,
        )));
        assert!(history_allows_summary_convergence(Some(
            SessionHistoricalIngestOutcome::Blocked {
                reason_code: "invalid_observation_contract",
                made_progress: false,
            },
        )));
        assert!(history_allows_summary_convergence(None));
    }

    #[test]
    fn a_terminal_history_window_releases_the_full_convergence_page() {
        let mut passes = 7;
        assert_eq!(
            lcm_convergence_admission(Some(SessionHistoricalIngestOutcome::Complete), &mut passes),
            LcmConvergenceAdmission::Admitted(LcmConvergencePage::Full)
        );
        assert_eq!(
            passes, 0,
            "a released pass must not carry priority debt forward"
        );
    }

    /// A profile whose history perpetually needs another window must still
    /// converge the one-shot predecessor-range rewrite: the fix for #843 moved
    /// that rewrite behind this scheduler, so a permanently prioritized
    /// history lane would leave a pre-fix widened range in place forever.
    #[tokio::test]
    async fn perpetually_pending_history_cannot_starve_the_range_rewrite() {
        let harness = RegisteredGlobalDbHarness::open("lcm-range-rewrite-fairness").await;
        let database = harness.registered.clone();
        let session_id = "range-rewrite-fairness-session";
        seed_pre_fix_widened_range(&database, session_id).await;
        assert!(
            rewrite_has_work(&database).await,
            "the seeded store must owe the rewrite a pass"
        );

        // One bounded page per admitted pass, so the whole rewrite may need
        // several; the bound is generous enough to prove convergence rather
        // than to pin the page count.
        const PASS_BOUND: u32 = HISTORY_PRIORITY_PASSES_BEFORE_RANGE_REWRITE * 4;
        let pending = Some(SessionHistoricalIngestOutcome::Pending {
            made_progress: true,
        });
        let mut history_priority_passes = 0u32;
        let mut admitted_rewrites = 0u32;
        let mut passes = 0u32;
        while rewrite_has_work(&database).await {
            passes = passes.saturating_add(1);
            assert!(
                passes <= PASS_BOUND,
                "a perpetually pending history lane starved the range rewrite for \
                 {passes} passes"
            );
            let admission = lcm_convergence_admission(pending, &mut history_priority_passes);
            assert_ne!(
                admission,
                LcmConvergenceAdmission::Admitted(LcmConvergencePage::Full),
                "historical continuation must keep priority over derived summaries"
            );
            if admission
                == LcmConvergenceAdmission::Admitted(LcmConvergencePage::PredecessorRangeRewrite)
            {
                admitted_rewrites = admitted_rewrites.saturating_add(1);
                crate::lcm_summary_convergence::run_predecessor_range_rewrite_page(
                    database.clone(),
                )
                .await
                .expect("bounded predecessor-range rewrite page");
            }
        }
        assert!(
            admitted_rewrites > 0 && passes >= HISTORY_PRIORITY_PASSES_BEFORE_RANGE_REWRITE,
            "the rewrite must converge through admitted passes, not by skipping priority"
        );
        assert_eq!(
            persisted_range(&database, session_id).await,
            Some((2, 2)),
            "the admitted pages must narrow the pre-fix interval off the policy anchor"
        );
    }

    /// Ingests one policy anchor plus two conversational rows, then models a
    /// store written before the policy-anchor role filter: the interval starts
    /// at the anchor and the rewrite journal has never run.
    async fn seed_pre_fix_widened_range(database: &RegisteredGlobalDbLeaseV1, session_id: &str) {
        let session = SessionRecord {
            provider: "claude".to_string(),
            session_id: session_id.to_string(),
            project_key: "project.range-rewrite-fairness".to_string(),
            project_path: "/tmp/range-rewrite-fairness".to_string(),
            title: None,
            started_at: Some(1),
            ended_at: None,
            transcript_path: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        };
        let messages = ["system", "user", "user"]
            .into_iter()
            .enumerate()
            .map(|(index, role)| {
                let ordinal = index as i64 + 1;
                SessionMessageRecord {
                    provider: "claude".to_string(),
                    message_id: format!("{session_id}-message-{ordinal}"),
                    session_id: session_id.to_string(),
                    role: role.to_string(),
                    timestamp: Some(ordinal),
                    ordinal,
                    text: format!("durable conversational context {ordinal}"),
                    kind: Some("message".to_string()),
                    model: None,
                    tool_names: None,
                    source_path: None,
                    source_offset: None,
                    metadata_json: None,
                }
            })
            .collect::<Vec<_>>();
        assert!(
            database
                .upsert_transcript_batch(
                    &session,
                    &messages,
                    &format!("/tmp/{session_id}.jsonl"),
                    ParseOffset::default(),
                )
                .await
        );
        let transaction = database
            .begin_write_transaction()
            .await
            .expect("write transaction");
        transaction
            .execute(
                "UPDATE lcm_raw_predecessor_ranges SET from_store_id = 1
                 WHERE provider = 'claude' AND session_id = ?1",
                params![session_id],
            )
            .await
            .expect("widen the persisted interval");
        transaction
            .execute(
                "DELETE FROM lcm_gc_meta WHERE key = 'predecessor_range_role_filter_v1'",
                (),
            )
            .await
            .expect("clear the rewrite journal");
        transaction.commit().await.expect("commit pre-fix store");
    }

    async fn rewrite_has_work(database: &RegisteredGlobalDbLeaseV1) -> bool {
        let snapshot = database.read_snapshot().await.expect("read snapshot");
        tracedecay_lcm::summary_convergence::predecessor_range_rewrite_has_work(&snapshot)
            .await
            .expect("journaled rewrite frontier")
    }

    async fn persisted_range(
        database: &RegisteredGlobalDbLeaseV1,
        session_id: &str,
    ) -> Option<(i64, i64)> {
        let snapshot = database.read_snapshot().await.expect("read snapshot");
        let mut rows = snapshot
            .query(
                "SELECT from_store_id, to_store_id
                 FROM lcm_raw_predecessor_ranges
                 WHERE provider = 'claude' AND session_id = ?1
                 ORDER BY to_store_id DESC
                 LIMIT 1",
                params![session_id],
            )
            .await
            .expect("persisted interval");
        let row = rows.next().await.expect("interval row")?;
        Some((
            row.get::<i64>(0).expect("from store id"),
            row.get::<i64>(1).expect("to store id"),
        ))
    }
}
