use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::ops::Range;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::PoisonError;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tracedecay_lcm::LcmError;
use tracedecay_store::{
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshCompletionRequestV1,
    SessionRefreshDispositionV1, SessionRefreshFailureRequestV1, SessionRefreshFrontierV1,
    SessionRefreshProgressV1, SessionRefreshStore, SessionStoreError, SessionStoreResult,
};

use super::history::{
    SessionHistoricalCapacityRelease, SessionHistoricalIngestOutcome,
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
use tracedecay_runtime_core::db::engine::Error as EngineError;
use tracedecay_runtime_core::native_runtime::{self, TimeoutError};
use tracedecay_session_temporal_store::{
    SessionRefreshBeginBatchItemV1, SessionRefreshBeginBatchOutcomeV1, SessionRefreshBeginPlanV1,
    SessionRefreshCompletionStage, SessionRefreshRecoveryV1, SessionRefreshRestartStateV1,
    SessionTemporalAccess, SessionTemporalStore, relations::SessionRelationProjection,
};

const HISTORY_IDLE_RECHECK_INTERVAL: Duration = Duration::from_mins(1);

fn history_allows_summary_convergence(outcome: Option<SessionHistoricalIngestOutcome>) -> bool {
    !outcome
        .as_ref()
        .is_some_and(SessionHistoricalIngestOutcome::needs_another_pass)
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
/// spends its admission, the same permit and bounded budget one history page
/// takes, on the rewrite alone, which caps the rewrite's starvation at that
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
/// has no free permit. The worker retries once a permit is released while
/// projection serving continues unblocked.
pub(super) const HISTORY_ADMISSION_SATURATED_REASON: &str = "history_admission_saturated";

/// How the worker schedules the pass after one that still needs history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistoryContinuation {
    /// The window committed coverage and yielded. Run the next window now.
    Immediate,
    /// The pass needs another window but committed nothing. Wait for the
    /// capacity it was refused to be released.
    AwaitRelease,
    /// History does not need another pass.
    Settled,
}

/// A window that committed coverage resumes from it immediately, whether it
/// yielded as pending or as retryable backpressure. Re-running a window that
/// committed nothing would only re-read the same sources, so it waits for a
/// release instead.
fn history_continuation(outcome: Option<SessionHistoricalIngestOutcome>) -> HistoryContinuation {
    match outcome {
        Some(outcome) if outcome.needs_another_pass() && outcome.made_progress() => {
            HistoryContinuation::Immediate
        }
        Some(outcome) if outcome.needs_another_pass() => HistoryContinuation::AwaitRelease,
        _ => HistoryContinuation::Settled,
    }
}

/// What a history pass that committed nothing waits on before it runs again.
enum HistoryRelease {
    /// The daemon-wide historical admission had no free permit.
    Admission,
    /// The pass ran and reported capacity it could not get.
    Capacity(SessionHistoricalCapacityRelease),
}

impl HistoryRelease {
    /// Resolves once the refused capacity frees. An admission wait returns the
    /// permit its queue turn granted: dropping it and retrying admission would
    /// hand the turn to the next waiter, so the next history pass runs under it.
    async fn released<'a>(
        &mut self,
        admission: &'a tokio::sync::Semaphore,
    ) -> Option<tokio::sync::SemaphorePermit<'a>> {
        match self {
            Self::Admission => match admission.acquire().await {
                Ok(permit) => Some(permit),
                Err(_) => std::future::pending().await,
            },
            Self::Capacity(release) => {
                release.released().await;
                None
            }
        }
    }
}

/// What the worker does after one pass while history still owes another window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowFollowUp {
    /// The admitted window is not searchable yet. Run another projection pass
    /// before opening the next history window.
    PublishBeforeNextHistory,
    /// Publication finished or stalled. Open the next history window now.
    ContinueHistory,
    /// The history window made no progress. Wait for a release.
    AwaitHistoryRelease,
    /// History does not own the next pass.
    Settle,
}

fn projection_published_work(report: &SessionTemporalRefreshPassReport) -> bool {
    report.begun > 0
        || report.joined > 0
        || report.projected_batches > 0
        || report.completed > 0
        || report.failed > 0
}

fn projection_still_unpublished(report: &SessionTemporalRefreshPassReport) -> bool {
    report.backlog.is_some_and(|backlog| backlog > 0) || report.saturated
}

/// An in-scope Codex window becomes searchable only after its projection
/// backlog reaches an active generation. The next history window, which on a
/// large corpus is the out-of-scope cursor sweep, waits until that publish
/// finishes or a projection pass stops moving.
fn window_follow_up(
    history: HistoryContinuation,
    publication_pending: bool,
    projection_moved: bool,
    holding: bool,
) -> WindowFollowUp {
    // A newest-day yield is often `Retryable` backpressure that committed
    // nothing, so it waits for a release rather than continuing. Waiting before
    // the projection backlog is published hands the worker back to the
    // out-of-scope cursor sweep with the newest day still on a building
    // generation. The release wait applies only when this pass did not move
    // that backlog.
    let owed = publication_pending
        && projection_moved
        && match history {
            HistoryContinuation::Immediate | HistoryContinuation::AwaitRelease => true,
            HistoryContinuation::Settled => holding,
        };
    if owed {
        return WindowFollowUp::PublishBeforeNextHistory;
    }
    match history {
        HistoryContinuation::Immediate => WindowFollowUp::ContinueHistory,
        HistoryContinuation::AwaitRelease => WindowFollowUp::AwaitHistoryRelease,
        HistoryContinuation::Settled if holding => WindowFollowUp::ContinueHistory,
        HistoryContinuation::Settled => WindowFollowUp::Settle,
    }
}

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
    let mut history_release = None;
    let mut granted_history_admission = None;
    let _instrumentation = SessionTemporalRefreshWorkerInstrumentation::new(&state);
    state.mark_running();
    loop {
        if state.cancelled.load(Ordering::Acquire) {
            return;
        }
        loop {
            // Busy before any wake is consumed: a waiter must never see its
            // wake taken while the worker still reads idle.
            state.mark_worker_busy();
            let mut projection_requested = state.take_dirty();
            let mut history_requested = state.take_historical_dirty();
            // A wake that arrives while the admitted window is still
            // unpublished must not start the next history window. That window
            // is the out-of-scope cursor sweep, and it holds this worker until
            // it returns, so the newest day never leaves its building generation.
            if state.history_held_for_projection() {
                history_requested = false;
                projection_requested = true;
            }
            if !projection_requested && !history_requested {
                break;
            }
            state.begin_pass();
            state.pass_count.fetch_add(1, Ordering::AcqRel);
            // A granted turn this iteration does not spend on history is
            // returned before the summary page queues for a permit of its own.
            let granted = granted_history_admission
                .take()
                .filter(|_| history_requested);
            let history_outcome = if history_requested {
                let history_pass: Pin<
                    Box<
                        dyn Future<
                                Output = (SessionHistoricalIngestOutcome, Option<HistoryRelease>),
                            > + Send
                            + '_,
                    >,
                > = Box::pin(session_history_refresh(
                    &history,
                    &history_admission,
                    granted,
                ));
                let (outcome, release) = tracing::Instrument::instrument(
                    history_pass,
                    tracing::trace_span!("daemon.scheduler.session_temporal.history"),
                )
                .await;
                history_release = release;
                Some(outcome)
            } else {
                None
            };
            // A wake that lands after this pass started is served by its
            // projection instead of a pass of its own, so it counts as a
            // pass: `wake_and_wait_until_idle` joins the pass after its wake.
            if state.take_dirty() {
                projection_requested = true;
                state.pass_count.fetch_add(1, Ordering::AcqRel);
            }
            if let Some(outcome) = &history_outcome {
                state.record_history_outcome(outcome.clone());
            }
            if matches!(
                history_outcome,
                Some(SessionHistoricalIngestOutcome::Cancelled)
            ) || state.cancelled.load(Ordering::Acquire)
            {
                return;
            }
            match &history_outcome {
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
                .as_ref()
                .is_some_and(SessionHistoricalIngestOutcome::made_progress);
            let report = if projection_requested
                || state.has_requests()
                || history_requires_projection
            {
                let projection: Pin<
                    Box<dyn Future<Output = SessionTemporalRefreshPassReport> + Send + '_>,
                > = Box::pin(session_projection_refresh(
                    &database,
                    &state,
                    projector.as_ref(),
                    policy,
                ));
                let pass = tracing::Instrument::instrument(
                    projection,
                    tracing::trace_span!("daemon.scheduler.session_temporal.projection"),
                );
                tokio::pin!(pass);
                tokio::select! {
                    biased;
                    () = tracing::Instrument::instrument(state.wait_for_cancellation(), tracing::trace_span!("daemon.scheduler.session_temporal.projection_cancel")) => return,
                    report = &mut pass => report,
                }
            } else {
                SessionTemporalRefreshPassReport::default()
            };
            if state.cancelled.load(Ordering::Acquire) {
                return;
            }
            let holding_publication = state.history_held_for_projection();
            // A projection-only iteration that is finishing the admitted window
            // must not spend the pass on summary convergence. History did not
            // run, so the real outcome is `None`, which would otherwise admit
            // the full page.
            let convergence_outcome = if holding_publication && history_outcome.is_none() {
                Some(SessionHistoricalIngestOutcome::Pending {
                    made_progress: true,
                })
            } else {
                history_outcome.clone()
            };
            let convergence_admission =
                lcm_convergence_admission(convergence_outcome, &mut history_priority_passes);
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
                        () = tracing::Instrument::instrument(state.wait_for_cancellation(), tracing::trace_span!("daemon.scheduler.lcm_summary.admission_cancel")) => return,
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
                        state.mark_worker_idle();
                        state.idle.notify_waiters();
                        let permit = tokio::select! {
                            biased;
                            () = tracing::Instrument::instrument(state.wait_for_cancellation(), tracing::trace_span!("daemon.scheduler.lcm_summary.admission_cancel")) => return,
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
                        let page: Pin<
                            Box<
                                dyn Future<
                                        Output = Result<
                                            crate::lcm_summary_convergence::LcmSummaryConvergencePage,
                                            LcmError,
                                        >,
                                    > + Send,
                            >,
                        > = match convergence_page {
                            LcmConvergencePage::PredecessorRangeRewrite => Box::pin(
                                crate::lcm_summary_convergence::run_predecessor_range_rewrite_page(
                                    database.clone(),
                                ),
                            ),
                            LcmConvergencePage::Full => Box::pin(
                                crate::lcm_summary_convergence::run_summary_convergence_page(
                                    database.clone(),
                                    crate::lcm_summary_convergence::LCM_SUMMARY_CONVERGENCE_PAGE_LIMIT,
                                ),
                            ),
                        };
                        tokio::pin!(page);
                        let result = tokio::select! {
                            biased;
                            () = tracing::Instrument::instrument(state.wait_for_cancellation(), tracing::trace_span!("daemon.scheduler.lcm_summary.cancel")) => return,
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
                                    || page.parked_rows_requeued > 0
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
                || history_outcome
                    .as_ref()
                    .is_some_and(SessionHistoricalIngestOutcome::made_progress)
                || summary_convergence_made_progress;
            if let Some(backlog) = report.backlog {
                state.record_pass(
                    backlog.saturating_add(usize::from(history_needs_another_pass)),
                    made_progress,
                );
            }
            if let Some(class) = report.retry_class {
                retry_attempt = retry_attempt.saturating_add(1);
                tracing::warn!(
                    ?class,
                    retry_attempt,
                    error = report.last_error.as_deref(),
                    "session temporal refresh pass will retry"
                );
                state.mark_recovering(class.into(), class);
                state.requeue_projection();
                let retry_delay = session_refresh_retry_delay(class, retry_attempt);
                tokio::select! {
                    () = tracing::Instrument::instrument(state.wait_for_cancellation(), tracing::trace_span!("daemon.scheduler.session_temporal.retry_cancel")) => return,
                    () = tracing::Instrument::instrument(state.wake.notified(), tracing::trace_span!("daemon.scheduler.session_temporal.wake_wait")) => {}
                    fired = tracing::Instrument::instrument(native_runtime::sleep_until(Instant::now() + retry_delay), tracing::trace_span!("daemon.scheduler.session_temporal.retry_wait")) => {
                        if stop_without_timer(fired) {
                            return;
                        }
                    }
                }
            } else if history_needs_another_pass {
                state.mark_running();
                retry_attempt = 0;
                match window_follow_up(
                    history_continuation(history_outcome),
                    projection_still_unpublished(&report),
                    projection_published_work(&report),
                    holding_publication,
                ) {
                    // The admitted window is not searchable until this backlog
                    // is published. Opening the next history window first is
                    // the out-of-scope cursor sweep, and search stays empty
                    // until that sweep ends.
                    WindowFollowUp::PublishBeforeNextHistory => {
                        state.hold_history_for_projection();
                        state.update_history_retry_state(false);
                        state.requeue_projection();
                        tokio::task::yield_now().await;
                    }
                    WindowFollowUp::ContinueHistory => {
                        state.release_history_for_projection();
                        state.update_history_retry_state(false);
                        state.wake_history();
                    }
                    WindowFollowUp::AwaitHistoryRelease => {
                        state.release_history_for_projection();
                        state.update_history_retry_state(true);
                    }
                    WindowFollowUp::Settle => {}
                }
            } else {
                state.release_history_for_projection();
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
                        () = tracing::Instrument::instrument(state.wait_for_cancellation(), tracing::trace_span!("daemon.scheduler.lcm_summary.retry_cancel")) => return,
                        () = tracing::Instrument::instrument(state.wake.notified(), tracing::trace_span!("daemon.scheduler.lcm_summary.retry_wake")) => {}
                        fired = tracing::Instrument::instrument(native_runtime::sleep_until(Instant::now() + delay), tracing::trace_span!("daemon.scheduler.lcm_summary.retry_wait")) => {
                            if stop_without_timer(fired) {
                                return;
                            }
                        }
                    }
                }
            }
        }
        state.observe_quiescence();
        let release = tracing::Instrument::instrument(
            async {
                match history_release.as_mut() {
                    Some(release) if state.history_retry_pending() => {
                        release.released(&history_admission).await
                    }
                    _ => std::future::pending().await,
                }
            },
            tracing::trace_span!("daemon.scheduler.session_temporal.history_release_wait"),
        );
        tokio::pin!(release);
        // Poll the release wait once so a refused pass joins the admission's
        // FIFO queue before the worker reads idle: an observer that frees a
        // permit once it sees idle must find this worker queued for it.
        let granted_at_once = tokio::select! {
            biased;
            granted = &mut release => Some(granted),
            () = tokio::task::yield_now() => None,
        };
        state.mark_worker_idle();
        state.idle.notify_waiters();
        if let Some(granted) = granted_at_once {
            granted_history_admission = granted;
            state.update_history_retry_state(false);
            state.wake_history();
            continue;
        }
        loop {
            let wake = tracing::Instrument::instrument(
                state.wake.notified(),
                tracing::trace_span!("daemon.scheduler.session_temporal.wake_wait"),
            );
            if state.has_pending_work() {
                break;
            }
            tokio::select! {
                () = tracing::Instrument::instrument(state.wait_for_cancellation(), tracing::trace_span!("daemon.scheduler.session_temporal.idle_cancel")) => return,
                // Every wake sets its work flag first, so a wake without work
                // is a stale permit whose work a pass already took. Waiting
                // on keeps a refused pass's place in the admission queue.
                () = wake => {
                    if state.has_pending_work() {
                        break;
                    }
                }
                granted = &mut release => {
                    granted_history_admission = granted;
                    state.update_history_retry_state(false);
                    state.wake_history();
                    break;
                }
                // Discovery of new sources, and the retry of a refusal nothing
                // signals (a still-mounting authority, an undecidable source),
                // share this cadence. Unchanged sources cost it no reads.
                fired = tracing::Instrument::instrument(native_runtime::sleep_until(Instant::now() + HISTORY_IDLE_RECHECK_INTERVAL), tracing::trace_span!("daemon.scheduler.session_temporal.history_idle_wait")) => {
                    if stop_without_timer(fired) {
                        return;
                    }
                    state.update_history_retry_state(false);
                    if history
                        .read()
                        .unwrap_or_else(PoisonError::into_inner)
                        .is_some()
                    {
                        state.wake_history();
                    }
                    break;
                }
            }
        }
    }
}

/// Without a timer every retry or idle wait would return at once and the
/// worker would spin, so an unavailable timer stops it.
fn stop_without_timer(fired: Result<(), String>) -> bool {
    let Err(error) = fired else {
        return false;
    };
    tracing::error!(%error, "session temporal refresh worker stopped without a native timer");
    true
}

/// Records a deadline wait that ended without its work. An elapsed deadline
/// retries; an unavailable timer is terminal because the retry needs it too.
fn record_deadline_failure(
    report: &mut SessionTemporalRefreshPassReport,
    stage: &str,
    error: TimeoutError,
) {
    match error {
        TimeoutError::Elapsed => {
            report.last_error = Some(format!("{stage}_deadline_exceeded"));
            report.deadline_errors += 1;
            report.observe_retry(SessionTemporalRefreshRetryClass::Deadline);
        }
        TimeoutError::TimerUnavailable(error) => {
            report.last_error = Some(format!("{stage}_timer_unavailable: {error}"));
            report.terminal_errors += 1;
        }
    }
}

struct SessionTemporalRefreshWorkerInstrumentation<'a> {
    state: &'a SessionTemporalRefreshWakeState,
}

impl<'a> SessionTemporalRefreshWorkerInstrumentation<'a> {
    fn new(state: &'a SessionTemporalRefreshWakeState) -> Self {
        Self { state }
    }
}

impl Drop for SessionTemporalRefreshWorkerInstrumentation<'_> {
    fn drop(&mut self) {
        stop_worker(self.state);
    }
}

fn stop_worker(state: &SessionTemporalRefreshWakeState) {
    state.clear_worker_activity_instrumentation();
}

/// Runs one historical ingest pass under the daemon-wide bounded admission.
///
/// The permit is held for the whole pass, so at most
/// `MAX_CONCURRENT_HISTORICAL_INGEST_PASSES` passes run concurrently across
/// every mounted project and the profile. A saturated admission defers this
/// worker's pass as typed retryable state rather than queueing behind it:
/// the worker's projection serving continues while the release wait queues
/// for a permit, and the next pass runs under the permit that wait was
/// `granted`. The capacity-release signals are subscribed before the pass
/// runs, so a release during it is not lost.
async fn session_history_refresh(
    history: &Arc<std::sync::RwLock<Option<SharedSessionHistoricalIngestor>>>,
    admission: &tokio::sync::Semaphore,
    granted: Option<tokio::sync::SemaphorePermit<'_>>,
) -> (SessionHistoricalIngestOutcome, Option<HistoryRelease>) {
    let history = history
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    match history {
        Some(history) => {
            let Some(_permit) = granted.or_else(|| admission.try_acquire().ok()) else {
                return (
                    SessionHistoricalIngestOutcome::Retryable {
                        reason_code: HISTORY_ADMISSION_SATURATED_REASON.to_owned(),
                        made_progress: false,
                    },
                    Some(HistoryRelease::Admission),
                );
            };
            let release = history.capacity_release();
            (
                history.run_pass().await,
                Some(HistoryRelease::Capacity(release)),
            )
        }
        None => (
            SessionHistoricalIngestOutcome::Blocked {
                reason_code: "history_ingestor_missing".to_owned(),
                made_progress: false,
            },
            None,
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

/// True when replaying this store failure unchanged could still succeed.
///
/// `is_storage` only says the failure came from the storage adapter; it does
/// not say the failure is transient. A schema-contract trigger refusing the
/// submitted row, or an exact-SQL ceiling refusing the submitted statement, is
/// deterministic: the worker resubmits the identical request every pass, so
/// treating it as retryable is an unbounded spin at the backoff cap rather
/// than a recovery. Those are terminal, and the caller durably fails the
/// refresh instead of retrying it.
fn is_retryable_storage(error: &SessionStoreError) -> bool {
    matches!(error, SessionStoreError::Storage { .. }) && !is_deterministic_refusal(error)
}

/// True when the durable contract refused the exact submitted row or
/// statement: a typed store refusal, or an engine failure that replays
/// identically. Only such a refusal retires a running refresh. An
/// interrupted pass (cancelled control, deadline, budget) and transient
/// storage leave the operation for the next pass, which may hold a
/// different control.
fn is_deterministic_refusal(error: &SessionStoreError) -> bool {
    match error {
        SessionStoreError::Cancelled
        | SessionStoreError::DeadlineExceeded
        | SessionStoreError::BudgetExceeded { .. } => false,
        SessionStoreError::Storage { source, .. } => source
            .downcast_ref::<EngineError>()
            .is_some_and(EngineError::is_deterministic_refusal),
        _ => true,
    }
}

/// A queued begin request whose begin has been replayed and rolled back so
/// the projector can build the first batch before anything commits. The
/// operation becomes durable when the first batch's commit folds the begin's
/// rows in; until then the guard retains the request so a dropped pass
/// re-queues it for the durable-state discovery the request came from.
pub struct PreparedSessionRefresh<'a> {
    request: SessionRefreshBeginOrJoinRequestV1,
    recovery: SessionRefreshRecoveryV1,
    pending: PendingBeginRequestGuard<'a>,
}

impl PreparedSessionRefresh<'_> {
    fn disarm(&mut self) {
        self.pending.disarm();
    }
}

struct DeferredPreparedBegin<'a> {
    prepared: PreparedSessionRefresh<'a>,
    item: SessionRefreshBeginBatchItemV1,
}

struct ClaimedTerminalAttempt<'a> {
    state: &'a SessionTemporalRefreshWakeState,
    recovery: SessionRefreshRecoveryV1,
    retain: bool,
}

impl<'a> ClaimedTerminalAttempt<'a> {
    fn new(state: &'a SessionTemporalRefreshWakeState, recovery: SessionRefreshRecoveryV1) -> Self {
        Self {
            state,
            recovery,
            retain: false,
        }
    }

    fn retain(&mut self) {
        self.retain = true;
    }
}

impl Drop for ClaimedTerminalAttempt<'_> {
    fn drop(&mut self) {
        if !self.retain {
            self.state.release_terminal_attempt(&self.recovery);
        }
    }
}

struct StagedReadyCompletion<'a> {
    claim: ClaimedTerminalAttempt<'a>,
    request: SessionRefreshCompletionRequestV1,
    projection: SessionRelationProjection,
}

pub async fn process_refresh_begin_requests<'a>(
    store: &SessionTemporalStore<'_, tracedecay_global_db::RegisteredGlobalDb>,
    state: &'a SessionTemporalRefreshWakeState,
    limit: usize,
    report: &mut SessionTemporalRefreshPassReport,
) -> Vec<PreparedSessionRefresh<'a>> {
    let mut prepared = Vec::new();
    for _ in 0..limit {
        let Some(request) = state.take_requests(1).pop() else {
            break;
        };
        let mut pending = PendingBeginRequestGuard::new(state, request);
        if state.cancelled.load(Ordering::Acquire) {
            return prepared;
        }
        match store
            .plan_session_refresh_begin(pending.request().clone())
            .await
        {
            Ok(SessionRefreshBeginPlanV1::Prepared(recovery)) => {
                prepared.push(PreparedSessionRefresh {
                    request: pending.request().clone(),
                    recovery: *recovery,
                    pending,
                });
            }
            // A pending reset committed the begin instead of folding; the
            // running operation joins this pass's durable recoveries.
            Ok(SessionRefreshBeginPlanV1::Begun) => {
                pending.disarm();
                report.begun += 1;
            }
            Ok(SessionRefreshBeginPlanV1::Joined) => {
                pending.disarm();
                report.joined += 1;
            }
            Err(error) if is_retryable_storage(&error) => {
                report.last_error = Some(format!("{error:?}"));
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
    prepared
}

#[tracing::instrument(
    name = "daemon.scheduler.session_temporal.begin_admitted",
    level = "trace",
    skip_all
)]
pub async fn begin_admitted_session_refreshes<'a>(
    database: &RegisteredGlobalDb,
    store: &SessionTemporalStore<'_, tracedecay_global_db::RegisteredGlobalDb>,
    state: &'a SessionTemporalRefreshWakeState,
    limit: usize,
    report: &mut SessionTemporalRefreshPassReport,
) -> Vec<PreparedSessionRefresh<'a>> {
    if state.has_requests() {
        report.saturated = true;
        return Vec::new();
    }
    let cursor = state.projection_discovery_cursor();
    let active_scan_slots = state.projection_discovery_active_slots(limit);
    let page = match SessionTemporalAccess::new(database)
        .pending_session_temporal_refresh_page_result(limit, active_scan_slots, &cursor)
        .await
    {
        Ok(page) => page,
        Err(error) => {
            if is_retryable_storage(&error) {
                report.last_error = Some(format!("{error:?}"));
                report.retryable_errors += 1;
                report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
            } else {
                report.terminal_errors += 1;
            }
            return Vec::new();
        }
    };
    let (requests, next_cursor, has_more) = page.into_parts();
    for request in requests.into_iter().rev() {
        if !state.suppresses_discovered_request(&request) {
            state.requeue_request(request);
        }
    }
    state.update_projection_discovery_cursor(next_cursor);
    report.saturated |= has_more;
    process_refresh_begin_requests(store, state, limit, report).await
}

fn completion_request(
    recovery: &SessionRefreshRecoveryV1,
) -> Option<SessionRefreshCompletionRequestV1> {
    let progress = recovery.progress()?;
    let request = SessionRefreshCompletionRequestV1::new(
        recovery.operation_id().clone(),
        recovery.session_id().clone(),
        progress.frontier(),
        *progress.coverage(),
    )
    .ok()?;
    Some(match progress.source_coverage().cloned() {
        Some(source_coverage) => request.with_source_coverage(source_coverage),
        None => request,
    })
}

struct RefreshPassContext<'a> {
    database: &'a RegisteredGlobalDbLeaseV1,
    store: &'a SessionTemporalStore<'a, tracedecay_global_db::RegisteredGlobalDb>,
    state: &'a SessionTemporalRefreshWakeState,
    projector: &'a dyn SessionTemporalRefreshProjector,
    policy: SessionTemporalRefreshPolicy,
}

struct RefreshDrain<'a> {
    deferred_begins: Vec<DeferredPreparedBegin<'a>>,
    staged_completions: Vec<StagedReadyCompletion<'a>>,
}

async fn stage_ready_completion<'a>(
    access: &SessionTemporalAccess<'_, RegisteredGlobalDb>,
    store: &SessionTemporalStore<'_, tracedecay_global_db::RegisteredGlobalDb>,
    state: &'a SessionTemporalRefreshWakeState,
    policy: SessionTemporalRefreshPolicy,
    recovery: SessionRefreshRecoveryV1,
    drain: &mut RefreshDrain<'a>,
    report: &mut SessionTemporalRefreshPassReport,
) {
    if !state.claim_terminal_attempt(&recovery) {
        return;
    }
    let mut claim = ClaimedTerminalAttempt::new(state, recovery);
    let Some(request) = completion_request(&claim.recovery) else {
        claim.retain();
        report.terminal_errors += 1;
        return;
    };
    match native_runtime::timeout(
        policy.operation_deadline,
        access.stage_session_refresh_completion(request, &state.completion_control()),
    )
    .await
    {
        Err(error) => record_deadline_failure(report, "completion", error),
        Ok(Ok(SessionRefreshCompletionStage::Durable(_))) => {
            report.completed += 1;
        }
        Ok(Ok(SessionRefreshCompletionStage::Ready {
            request,
            relation_projection,
        })) => drain.staged_completions.push(StagedReadyCompletion {
            claim,
            request,
            projection: *relation_projection,
        }),
        Ok(Err(error)) => {
            reject_staged_completion(store, state, claim, error, report).await;
        }
    }
}

async fn reject_staged_completion<'a>(
    store: &SessionTemporalStore<'_, tracedecay_global_db::RegisteredGlobalDb>,
    state: &'a SessionTemporalRefreshWakeState,
    claim: ClaimedTerminalAttempt<'a>,
    error: SessionStoreError,
    report: &mut SessionTemporalRefreshPassReport,
) {
    if matches!(error, SessionStoreError::DeadlineExceeded) {
        report.last_error = Some(format!("{error:?}"));
        report.deadline_errors += 1;
        report.observe_retry(SessionTemporalRefreshRetryClass::Deadline);
        return;
    }
    if is_retryable_storage(&error) {
        report.last_error = Some(format!("{error:?}"));
        report.retryable_errors += 1;
        report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
        return;
    }
    if is_deterministic_refusal(&error) {
        report.last_error = Some(format!("{error:?}"));
        let recovery = claim.recovery.clone();
        drop(claim);
        match durable_failure_request(
            &recovery,
            durable_projector_failure_code(REFRESH_COMPLETION_REFUSED),
        ) {
            Some(request) => apply_fail_effect(store, state, &recovery, request, report).await,
            None => report.terminal_errors += 1,
        }
        return;
    }
    let mut claim = claim;
    claim.retain();
    report.last_error = Some(format!("{error:?}"));
    report.terminal_errors += 1;
}

/// A timed-out writer transaction rolls back its whole group. Retrying each
/// item alone under the same operation deadline lets every individually
/// affordable item commit, and a degraded pass waits for at most one more
/// timeout per item than the group's own.
async fn commit_refresh_groups<T, F, Fut>(
    item_count: usize,
    deadline: Duration,
    mut commit: F,
) -> SessionStoreResult<Vec<SessionStoreResult<T>>>
where
    F: FnMut(Range<usize>) -> Fut,
    Fut: Future<Output = SessionStoreResult<Vec<SessionStoreResult<T>>>>,
{
    if let Some(outcomes) = commit_refresh_group(0..item_count, deadline, &mut commit).await? {
        return Ok(outcomes);
    }
    let mut outcomes = Vec::with_capacity(item_count);
    if item_count == 1 {
        outcomes.push(Err(SessionStoreError::DeadlineExceeded));
        return Ok(outcomes);
    }
    for item in 0..item_count {
        match commit_refresh_group(item..item + 1, deadline, &mut commit).await? {
            Some(alone) => outcomes.extend(alone),
            None => outcomes.push(Err(SessionStoreError::DeadlineExceeded)),
        }
    }
    Ok(outcomes)
}

/// `None` when the group's transaction did not commit within `deadline`.
async fn commit_refresh_group<T, F, Fut>(
    range: Range<usize>,
    deadline: Duration,
    commit: &mut F,
) -> SessionStoreResult<Option<Vec<SessionStoreResult<T>>>>
where
    F: FnMut(Range<usize>) -> Fut,
    Fut: Future<Output = SessionStoreResult<Vec<SessionStoreResult<T>>>>,
{
    let items = range.len();
    let result = native_runtime::timeout(
        deadline,
        tracing::Instrument::instrument(
            commit(range),
            tracing::trace_span!("session_temporal.commit_refresh_group", items),
        ),
    )
    .await;
    match result {
        Err(TimeoutError::Elapsed) | Ok(Err(SessionStoreError::DeadlineExceeded)) => Ok(None),
        Err(error @ TimeoutError::TimerUnavailable(_)) => {
            Err(SessionStoreError::storage("time refresh group", error))
        }
        Ok(Err(error)) => Err(error),
        Ok(Ok(outcomes)) if outcomes.len() == items => Ok(Some(outcomes)),
        Ok(Ok(_)) => Err(SessionStoreError::InvalidStateTransition {
            context: "refresh group returned inconsistent item count",
        }),
    }
}

async fn commit_deferred_begins<'a>(
    access: &SessionTemporalAccess<'_, RegisteredGlobalDb>,
    state: &SessionTemporalRefreshWakeState,
    policy: SessionTemporalRefreshPolicy,
    deferred: Vec<DeferredPreparedBegin<'a>>,
    report: &mut SessionTemporalRefreshPassReport,
) {
    if deferred.is_empty() {
        return;
    }
    let (items, prepared): (Vec<_>, Vec<_>) = deferred
        .into_iter()
        .map(|deferred| (deferred.item, deferred.prepared))
        .unzip();
    let outcomes = match commit_refresh_groups(items.len(), policy.operation_deadline, |range| {
        let committed = tracedecay_runtime_core::erase_send!(
            access.commit_session_refresh_begin_batch_group(
                &items[range],
                state.completion_control(),
            ),
            SessionStoreResult<Vec<SessionStoreResult<SessionRefreshBeginBatchOutcomeV1>>>
        );
        committed
    })
    .await
    {
        Err(error) => {
            report.last_error = Some(format!("{error:?}"));
            if is_retryable_storage(&error) {
                report.retryable_errors += 1;
                report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
            } else {
                report.terminal_errors += 1;
            }
            return;
        }
        Ok(outcomes) => outcomes,
    };
    for (mut prepared, outcome) in prepared.into_iter().zip(outcomes) {
        match outcome {
            Ok(SessionRefreshBeginBatchOutcomeV1::Persisted { .. }) => {
                prepared.disarm();
                report.begun += 1;
                report.projected_batches += 1;
            }
            Ok(SessionRefreshBeginBatchOutcomeV1::BeganOnly) => {
                prepared.disarm();
                report.begun += 1;
            }
            Ok(SessionRefreshBeginBatchOutcomeV1::Joined) => {
                prepared.disarm();
                report.joined += 1;
            }
            Err(SessionStoreError::DeadlineExceeded) => {
                report.last_error = Some("begin_commit_deadline_exceeded".to_string());
                report.deadline_errors += 1;
                report.observe_retry(SessionTemporalRefreshRetryClass::Deadline);
            }
            Err(error) if is_retryable_storage(&error) => {
                report.last_error = Some(format!("{error:?}"));
                report.retryable_errors += 1;
                report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
            }
            Err(error) => {
                report.last_error = Some(format!("{error:?}"));
                prepared.disarm();
                report.terminal_errors += 1;
            }
        }
    }
}

async fn drain_staged_completions<'a>(
    database: &RegisteredGlobalDbLeaseV1,
    access: &SessionTemporalAccess<'_, RegisteredGlobalDb>,
    store: &SessionTemporalStore<'_, tracedecay_global_db::RegisteredGlobalDb>,
    state: &'a SessionTemporalRefreshWakeState,
    policy: SessionTemporalRefreshPolicy,
    staged: Vec<StagedReadyCompletion<'a>>,
    report: &mut SessionTemporalRefreshPassReport,
) {
    if staged.is_empty() {
        return;
    }
    let projections = staged
        .iter()
        .map(|item| item.projection.clone())
        .collect::<Vec<_>>();
    let receipt_control = state.completion_control();
    let receipt_outcomes =
        match commit_refresh_groups(projections.len(), policy.operation_deadline, |range| {
            let recorded = tracedecay_runtime_core::erase_send!(
                access.record_relation_receipts_group(&projections[range], &receipt_control),
                SessionStoreResult<Vec<SessionStoreResult<tracedecay_graph_db::GraphWatermark>>>
            );
            recorded
        })
        .await
        {
            Err(error) => {
                report.last_error = Some(format!("{error:?}"));
                if is_retryable_storage(&error) {
                    report.retryable_errors += 1;
                    report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
                } else {
                    report.terminal_errors += 1;
                }
                return;
            }
            Ok(outcomes) => outcomes,
        };
    let mut survivors = Vec::with_capacity(staged.len());
    for (item, outcome) in staged.into_iter().zip(receipt_outcomes) {
        match outcome {
            Ok(_) => survivors.push(item),
            Err(error) => {
                reject_staged_completion(store, state, item.claim, error, report).await;
            }
        }
    }
    let snapshot =
        match native_runtime::timeout(policy.operation_deadline, database.as_ref().read_snapshot())
            .await
        {
            Err(error) => {
                record_deadline_failure(report, "snapshot", error);
                return;
            }
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(error)) => {
                report.last_error = Some(format!("{error:?}"));
                report.retryable_errors += 1;
                report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
                return;
            }
        };
    let mut applied = Vec::with_capacity(survivors.len());
    for item in survivors {
        match native_runtime::timeout(
            policy.operation_deadline,
            access.apply_candidate_relation_projection(
                &snapshot,
                &item.projection,
                &state.completion_control(),
            ),
        )
        .await
        {
            Err(error) => record_deadline_failure(report, "apply", error),
            Ok(Ok(verified)) => {
                applied.push((item, verified));
            }
            Ok(Err(error)) => {
                reject_staged_completion(store, state, item.claim, error, report).await;
            }
        }
    }
    if applied.is_empty() {
        return;
    }
    let complete_items = applied
        .iter()
        .map(|(item, verified)| (item.request.clone(), verified.clone()))
        .collect::<Vec<_>>();
    let outcomes =
        match commit_refresh_groups(complete_items.len(), policy.operation_deadline, |range| {
            let completed = tracedecay_runtime_core::erase_send!(
                access.complete_session_refresh_group(
                    &complete_items[range],
                    state.completion_control(),
                ),
                SessionStoreResult<
                    Vec<SessionStoreResult<tracedecay_store::SessionRefreshReceiptV1>>,
                >
            );
            completed
        })
        .await
        {
            Err(error) => {
                report.last_error = Some(format!("{error:?}"));
                if is_retryable_storage(&error) {
                    report.retryable_errors += 1;
                    report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
                } else {
                    report.terminal_errors += 1;
                }
                return;
            }
            Ok(outcomes) => outcomes,
        };
    for ((item, _), outcome) in applied.into_iter().zip(outcomes) {
        match outcome {
            Ok(_) => report.completed += 1,
            Err(error) => {
                reject_staged_completion(store, state, item.claim, error, report).await;
            }
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

/// Typed failure recorded when the durable contract refuses the projected
/// progress row. It is not a projector fault: the row was well formed for the
/// state the projector read, and the durable state disagrees.
const REFRESH_PROGRESS_REFUSED: &str = "refresh_progress_refused";

/// Typed failure recorded when the durable contract refuses to activate a
/// refresh whose progress is complete.
const REFRESH_COMPLETION_REFUSED: &str = "refresh_completion_refused";

/// Builds the durable failure request that retires one running refresh.
fn durable_failure_request(
    recovery: &SessionRefreshRecoveryV1,
    failure_code: String,
) -> Option<SessionRefreshFailureRequestV1> {
    let (frontier, coverage) = match recovery.progress() {
        Some(progress) => (progress.frontier(), *progress.coverage()),
        None => (
            SessionRefreshFrontierV1::new(
                recovery.target_frontier().observed_through(),
                recovery.source_frontier(),
            )
            .ok()?,
            zero_refresh_coverage(),
        ),
    };
    let request = SessionRefreshFailureRequestV1::new(
        recovery.operation_id().clone(),
        recovery.session_id().clone(),
        frontier,
        coverage,
        failure_code,
    )
    .ok()?;
    Some(
        match recovery
            .progress()
            .and_then(SessionRefreshProgressV1::source_coverage)
            .cloned()
            .or_else(|| recovery.source_coverage(frontier.committed_through()).ok())
        {
            Some(source_coverage) => request.with_source_coverage(source_coverage),
            None => request,
        },
    )
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
                Err(error) if is_retryable_storage(&error) => {
                    report.last_error = Some(format!("{error:?}"));
                    report.retryable_errors += 1;
                    report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
                }
                Err(error) if is_deterministic_refusal(&error) => {
                    // A refused progress row is not work the next pass can
                    // finish: rediscovery hands the projector the same durable
                    // state and the same row comes back refused. Retire the
                    // operation so it leaves `running` and a fresh refresh can
                    // be admitted, instead of resubmitting it forever.
                    report.last_error = Some(format!("{error:?}"));
                    match durable_failure_request(
                        recovery,
                        durable_projector_failure_code(REFRESH_PROGRESS_REFUSED),
                    ) {
                        Some(request) => {
                            apply_fail_effect(store, state, recovery, request, report).await;
                        }
                        None => report.terminal_errors += 1,
                    }
                }
                Err(error) => {
                    // Cancelled control, budget ceiling: this pass could not
                    // persist, but the row itself was not refused, so the
                    // operation stays `running` for the next pass.
                    report.last_error = Some(format!("{error:?}"));
                    report.terminal_errors += 1;
                }
            }
        }
        SessionTemporalRefreshEffect::Fail(request) => {
            apply_fail_effect(store, state, recovery, request, report).await;
        }
        SessionTemporalRefreshEffect::Deferred => report.deferred += 1,
    }
}

/// Applies the projected effect for a begin that has not committed yet.
/// The first projected batch folds the begin's rows into one commit;
/// deferral, refusal, and durable failure keep the request queued or retire
/// it through the same paths a durable operation uses.
async fn apply_prepared_refresh_effect(
    store: &SessionTemporalStore<'_, tracedecay_global_db::RegisteredGlobalDb>,
    state: &SessionTemporalRefreshWakeState,
    mut prepared: PreparedSessionRefresh<'_>,
    effect: SessionTemporalRefreshEffect,
    report: &mut SessionTemporalRefreshPassReport,
) {
    match effect {
        SessionTemporalRefreshEffect::Projection { progress, batch } => {
            let committed: Pin<
                Box<
                    dyn Future<
                            Output = SessionStoreResult<
                                tracedecay_session_temporal_store::SessionRefreshBeginBatchOutcomeV1,
                            >,
                        > + Send
                        + '_,
                >,
            > = Box::pin(store.commit_session_refresh_begin_batch(
                prepared.request.clone(),
                prepared.recovery.accepted_at(),
                progress,
                batch,
                state.completion_control(),
            ));
            match committed.await {
                Ok(SessionRefreshBeginBatchOutcomeV1::Persisted { .. }) => {
                    prepared.disarm();
                    report.begun += 1;
                    report.projected_batches += 1;
                }
                // The begin committed alone; durable recovery resumes the
                // operation next pass.
                Ok(SessionRefreshBeginBatchOutcomeV1::BeganOnly) => {
                    prepared.disarm();
                    report.begun += 1;
                }
                Ok(SessionRefreshBeginBatchOutcomeV1::Joined) => {
                    prepared.disarm();
                    report.joined += 1;
                }
                Err(error) if is_retryable_storage(&error) => {
                    report.last_error = Some(format!("{error:?}"));
                    report.retryable_errors += 1;
                    report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
                }
                Err(error) => {
                    report.last_error = Some(format!("{error:?}"));
                    prepared.disarm();
                    report.terminal_errors += 1;
                }
            }
        }
        SessionTemporalRefreshEffect::Fail(request) => {
            // The durable failure marker needs an operation row: begin through
            // the committing path, then retire it like any durable refresh.
            let begun: Pin<
                Box<
                    dyn Future<
                            Output = SessionStoreResult<
                                tracedecay_store::SessionRefreshBeginOrJoinReceiptV1,
                            >,
                        > + Send
                        + '_,
                >,
            > = Box::pin(store.begin_or_join_session_refresh(prepared.request.clone()));
            match begun.await {
                Ok(receipt) => {
                    prepared.disarm();
                    match receipt.disposition() {
                        SessionRefreshDispositionV1::Started => report.begun += 1,
                        SessionRefreshDispositionV1::Joined => report.joined += 1,
                    }
                    if receipt.operation_id() == prepared.recovery.operation_id() {
                        apply_fail_effect(store, state, &prepared.recovery, request, report).await;
                    } else {
                        report.terminal_errors += 1;
                    }
                }
                Err(error) if is_retryable_storage(&error) => {
                    report.last_error = Some(format!("{error:?}"));
                    report.retryable_errors += 1;
                    report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
                }
                Err(_) => {
                    prepared.disarm();
                    report.terminal_errors += 1;
                }
            }
        }
        SessionTemporalRefreshEffect::Deferred => report.deferred += 1,
    }
}

async fn apply_fail_effect(
    store: &SessionTemporalStore<'_, tracedecay_global_db::RegisteredGlobalDb>,
    state: &SessionTemporalRefreshWakeState,
    recovery: &SessionRefreshRecoveryV1,
    request: SessionRefreshFailureRequestV1,
    report: &mut SessionTemporalRefreshPassReport,
) {
    if !state.claim_terminal_attempt(recovery) {
        return;
    }
    let mut attempt = TerminalAttemptGuard::new(state, recovery);
    let session = recovery.session_id().as_str().to_owned();
    let cause = report.last_error.clone();
    match store.fail_session_refresh(request).await {
        Ok(_) => {
            tracing::warn!(
                session,
                error = cause.as_deref(),
                "session temporal refresh failed durably"
            );
            report.failed += 1;
            state.record_terminal_discovery_failure(recovery);
        }
        Err(error) if is_retryable_storage(&error) => {
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

/// One projection unit for a pass: the recovery the projector reads and,
/// when its begin has not committed yet, the prepared begin whose rows fold
/// into the first projected batch's commit.
struct ProjectionWorkItem<'a> {
    recovery: SessionRefreshRecoveryV1,
    prepared: Option<PreparedSessionRefresh<'a>>,
}

async fn project_running_refresh<'a>(
    context: &RefreshPassContext<'a>,
    work: ProjectionWorkItem<'a>,
    drain: &mut RefreshDrain<'a>,
    report: &mut SessionTemporalRefreshPassReport,
) {
    let ProjectionWorkItem { recovery, prepared } = work;
    let policy = context.policy;
    let state = context.state;
    let store = context.store;
    let deadline_at = Instant::now() + policy.operation_deadline;
    let projection = tracing::Instrument::instrument(
        context
            .projector
            .project(context.database, recovery.clone()),
        tracing::trace_span!("daemon.scheduler.session_temporal.projector"),
    );
    tokio::pin!(projection);
    let deadline = tracing::Instrument::instrument(
        native_runtime::sleep_until(deadline_at),
        tracing::trace_span!("daemon.scheduler.session_temporal.projector_deadline"),
    );
    tokio::pin!(deadline);
    let effect = tokio::select! {
        biased;
        () = tracing::Instrument::instrument(state.wait_for_cancellation(), tracing::trace_span!("daemon.scheduler.session_temporal.projector_cancel")) => return,
        fired = &mut deadline => {
            let error = fired.map_or_else(TimeoutError::TimerUnavailable, |()| TimeoutError::Elapsed);
            record_deadline_failure(report, "projector", error);
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
            let Some(request) = durable_failure_request(&recovery, failure_code) else {
                report.terminal_errors += 1;
                return;
            };
            SessionTemporalRefreshEffect::Fail(request)
        }
    };
    if state.cancelled.load(Ordering::Acquire) {
        return;
    }
    // The generation seed commits each page. Dropping this apply at the
    // projector deadline rolled back only the in-flight page, then the next
    // pass never recorded progress because the batch itself had not committed.
    tokio::select! {
        biased;
        () = tracing::Instrument::instrument(state.wait_for_cancellation(), tracing::trace_span!("daemon.scheduler.session_temporal.effect_apply_cancel")) => {}
        () = async {
            match prepared {
                Some(prepared) => match effect {
                    SessionTemporalRefreshEffect::Projection { progress, batch } => {
                        drain.deferred_begins.push(DeferredPreparedBegin {
                            item: SessionRefreshBeginBatchItemV1 {
                                request: prepared.request.clone(),
                                accepted_at: prepared.recovery.accepted_at(),
                                progress,
                                batch,
                            },
                            prepared,
                        });
                    }
                    effect => {
                        let applied: Pin<Box<dyn Future<Output = ()> + Send + '_>> = Box::pin(
                            apply_prepared_refresh_effect(
                                store, state, prepared, effect, report,
                            ),
                        );
                        applied.await;
                    }
                },
                None => {
                    let applied: Pin<Box<dyn Future<Output = ()> + Send + '_>> =
                        Box::pin(apply_refresh_effect(store, state, &recovery, effect, report));
                    applied.await;
                }
            }
        } => {}
    }
}

async fn running_refreshes(
    store: &SessionTemporalStore<'_, tracedecay_global_db::RegisteredGlobalDb>,
    report: &mut SessionTemporalRefreshPassReport,
) -> Option<Vec<SessionRefreshRecoveryV1>> {
    match store.running_session_refreshes().await {
        Ok(recoveries) => Some(recoveries),
        Err(error) => {
            report.last_error = Some(format!("{error:?}"));
            if is_retryable_storage(&error) {
                report.retryable_errors += 1;
                report.observe_retry(SessionTemporalRefreshRetryClass::Storage);
            } else {
                report.terminal_errors += 1;
            }
            None
        }
    }
}

async fn recoveries_for_pass<'a>(
    database: &RegisteredGlobalDbLeaseV1,
    store: &SessionTemporalStore<'_, tracedecay_global_db::RegisteredGlobalDb>,
    state: &'a SessionTemporalRefreshWakeState,
    policy: SessionTemporalRefreshPolicy,
    report: &mut SessionTemporalRefreshPassReport,
) -> Option<(
    Vec<SessionRefreshRecoveryV1>,
    Vec<PreparedSessionRefresh<'a>>,
    bool,
)> {
    let mut recoveries = running_refreshes(store, report).await?;
    if !recoveries.is_empty() {
        // Existing durable work owns this pass; discovery waits until these
        // recoveries drain.
        return Some((recoveries, Vec::new(), true));
    }
    let prepared = begin_admitted_session_refreshes(
        database,
        store,
        state,
        policy.max_begin_requests_per_pass,
        report,
    )
    .await;
    recoveries = running_refreshes(store, report).await?;
    Some((recoveries, prepared, false))
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
    let access = SessionTemporalAccess::new(database.as_ref());
    let context = RefreshPassContext {
        database,
        store: &store,
        state: state.as_ref(),
        projector,
        policy,
    };
    let mut report = SessionTemporalRefreshPassReport::default();
    if state.cancelled.load(Ordering::Acquire) {
        return report;
    }
    let begins: Pin<Box<dyn Future<Output = Vec<PreparedSessionRefresh<'_>>> + Send + '_>> =
        Box::pin(process_refresh_begin_requests(
            &store,
            state,
            policy.max_begin_requests_per_pass,
            &mut report,
        ));
    let mut prepared = begins.await;
    let recovered = tracedecay_runtime_core::erase_send!(
        recoveries_for_pass(database, &store, state, policy, &mut report,),
        Option<(
            Vec<SessionRefreshRecoveryV1>,
            Vec<PreparedSessionRefresh<'_>>,
            bool,
        )>
    );
    let Some((mut recoveries, admitted, discovery_deferred)) = recovered.await else {
        return report;
    };
    prepared.extend(admitted);
    // Planned begins are not durable: a durable operation holding the same
    // key owns the request already, and a second plan of an equivalent
    // request attaches to the first plan.
    let durable_keys = recoveries.iter().map(recovery_key).collect::<HashSet<_>>();
    let mut prepared_by_key = HashMap::new();
    for mut prepared_begin in prepared {
        let key = recovery_key(&prepared_begin.recovery);
        if durable_keys.contains(&key) || prepared_by_key.contains_key(&key) {
            prepared_begin.disarm();
            report.joined += 1;
            continue;
        }
        prepared_by_key.insert(key, prepared_begin);
    }
    recoveries.extend(
        prepared_by_key
            .values()
            .map(|prepared_begin| prepared_begin.recovery.clone()),
    );
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
    let mut drain = RefreshDrain {
        deferred_begins: Vec::new(),
        staged_completions: Vec::new(),
    };
    for recovery in selected {
        let operation = recovery_key(&recovery);
        if state.cancelled.load(Ordering::Acquire) {
            return report;
        }
        match recovery.restart_state() {
            SessionRefreshRestartStateV1::ReadyToComplete => {
                let staged: Pin<Box<dyn Future<Output = ()> + Send + '_>> =
                    Box::pin(stage_ready_completion(
                        &access,
                        &store,
                        state,
                        policy,
                        recovery,
                        &mut drain,
                        &mut report,
                    ));
                staged.await;
            }
            SessionRefreshRestartStateV1::BeginProjection
            | SessionRefreshRestartStateV1::ResumeProjection { .. } => {
                let projected: Pin<Box<dyn Future<Output = ()> + Send + '_>> =
                    Box::pin(project_running_refresh(
                        &context,
                        ProjectionWorkItem {
                            recovery,
                            prepared: prepared_by_key.remove(&operation),
                        },
                        &mut drain,
                        &mut report,
                    ));
                projected.await;
            }
        }
        selection.complete(&operation);
        if report.retry_class.is_some() {
            break;
        }
    }
    let committed: Pin<Box<dyn Future<Output = ()> + Send + '_>> = Box::pin(
        commit_deferred_begins(&access, state, policy, drain.deferred_begins, &mut report),
    );
    committed.await;
    let drained: Pin<Box<dyn Future<Output = ()> + Send + '_>> =
        Box::pin(drain_staged_completions(
            database,
            &access,
            &store,
            state,
            policy,
            drain.staged_completions,
            &mut report,
        ));
    drained.await;
    let terminal = report
        .completed
        .saturating_add(report.failed)
        .saturating_add(report.cancelled);
    report.saturated |= discovery_deferred && (report.projected_batches > 0 || terminal > 0);
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
    use tracedecay_domain::SessionId;
    use tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness;
    use tracedecay_runtime_core::db::engine::params;
    use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};

    /// Wide enough that one item's commit lands inside it on a loaded host.
    const COMMIT_DEADLINE: Duration = Duration::from_secs(2);

    /// Commits `item_count` items. A group of several items never commits
    /// before the deadline, and neither does a `slow` item alone; every other
    /// item alone commits at once. Returns how many commits the deadline cut
    /// short.
    async fn timed_refresh_group_progress(item_count: usize, slow: &[usize]) -> usize {
        let harness = RegisteredGlobalDbHarness::open("refresh-group-progress").await;
        let database = &harness.registered;
        let transaction = database.begin_write_transaction().await.unwrap();
        transaction
            .execute(
                "CREATE TABLE refresh_group_progress (item INTEGER PRIMARY KEY)",
                (),
            )
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let cut_short = std::sync::atomic::AtomicUsize::new(0);
        let cut_short = &cut_short;
        let outcomes = commit_refresh_groups(item_count, COMMIT_DEADLINE, |range| async move {
            let transaction = database
                .begin_write_transaction()
                .await
                .map_err(|error| SessionStoreError::storage("begin refresh group", error))?;
            for item in range.clone() {
                transaction
                    .execute(
                        "INSERT INTO refresh_group_progress (item) VALUES (?1)",
                        params![item as i64],
                    )
                    .await
                    .map_err(|error| SessionStoreError::storage("write refresh group", error))?;
            }
            if range.len() > 1 || slow.contains(&range.start) {
                cut_short.fetch_add(1, Ordering::SeqCst);
                std::future::pending::<()>().await;
            }
            transaction
                .commit()
                .await
                .map_err(|error| SessionStoreError::storage("commit refresh group", error))?;
            Ok(range.map(Ok).collect())
        })
        .await
        .unwrap();

        assert_eq!(outcomes.len(), item_count);
        for (item, outcome) in outcomes.into_iter().enumerate() {
            if slow.contains(&item) {
                assert!(matches!(outcome, Err(SessionStoreError::DeadlineExceeded)));
            } else {
                assert_eq!(outcome.unwrap(), item);
            }
        }
        let snapshot = database.read_snapshot().await.unwrap();
        let mut rows = snapshot
            .query("SELECT item FROM refresh_group_progress ORDER BY item", ())
            .await
            .unwrap();
        let mut committed = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            committed.push(row.get::<i64>(0).unwrap() as usize);
        }
        assert_eq!(
            committed,
            (0..item_count)
                .filter(|item| !slow.contains(item))
                .collect::<Vec<_>>(),
            "cancelled groups must roll back before their items are retried"
        );
        cut_short.load(Ordering::SeqCst)
    }

    #[tokio::test]
    async fn timed_refresh_groups_commit_individually_affordable_items() {
        assert_eq!(timed_refresh_group_progress(16, &[]).await, 1);
    }

    #[tokio::test]
    async fn timed_refresh_groups_do_not_starve_items_after_a_slow_one() {
        assert_eq!(timed_refresh_group_progress(16, &[0]).await, 2);
    }

    #[tokio::test]
    async fn timed_refresh_groups_wait_at_most_one_timeout_per_item_beyond_the_group() {
        assert_eq!(
            timed_refresh_group_progress(4, &[0, 1, 2, 3]).await,
            5,
            "a pass of slow items must wait for the group and then each item once"
        );
    }

    #[tokio::test]
    async fn a_timed_out_completion_stays_claimable_by_the_next_pass() {
        let harness = RegisteredGlobalDbHarness::open("refresh-timed-out-completion").await;
        let store = SessionTemporalStore::new(harness.registered.as_ref());
        store
            .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
                SessionId::new("timed-out-completion").unwrap(),
                SessionRefreshFrontierV1::new(0, 0).unwrap(),
            ))
            .await
            .unwrap();
        let recovery = store.running_session_refreshes().await.unwrap().remove(0);
        let state = SessionTemporalRefreshWakeState::default();
        assert!(state.claim_terminal_attempt(&recovery));
        let mut report = SessionTemporalRefreshPassReport::default();

        reject_staged_completion(
            &store,
            &state,
            ClaimedTerminalAttempt::new(&state, recovery.clone()),
            SessionStoreError::DeadlineExceeded,
            &mut report,
        )
        .await;

        assert_eq!(
            report.retry_class,
            Some(SessionTemporalRefreshRetryClass::Deadline)
        );
        assert_eq!(report.terminal_errors, 0);
        assert!(
            state.claim_terminal_attempt(&recovery),
            "a completion that missed its deadline must be retried, not held as terminal"
        );
    }

    #[test]
    fn deterministic_storage_refusals_are_not_retryable() {
        // The schema-contract trigger that refused eleven hours of identical
        // progress rows in #1794: transport-level `Storage`, but replaying it
        // can never succeed.
        let refused = SessionStoreError::storage(
            "persist session refresh progress",
            EngineError::Sqlite {
                operation: "execute",
                code: Some(19),
                extended_code: Some(1811),
                message: "invalid session refresh progress".to_owned(),
            },
        );
        assert!(!is_retryable_storage(&refused));

        // Contention is the transient case the retry loop exists for.
        assert!(is_retryable_storage(&SessionStoreError::storage(
            "persist session refresh progress",
            EngineError::Busy,
        )));

        // Typed contract failures were already terminal and stay terminal.
        assert!(!is_retryable_storage(
            &SessionStoreError::InvalidStateTransition {
                context: "refresh progress successor",
            }
        ));
    }

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
    fn a_window_that_committed_coverage_continues_and_one_that_did_not_awaits_release() {
        assert_eq!(
            history_continuation(Some(SessionHistoricalIngestOutcome::Pending {
                made_progress: true,
            })),
            HistoryContinuation::Immediate
        );
        assert_eq!(
            history_continuation(Some(SessionHistoricalIngestOutcome::Pending {
                made_progress: false,
            })),
            HistoryContinuation::AwaitRelease
        );
        assert_eq!(
            history_continuation(Some(SessionHistoricalIngestOutcome::Retryable {
                reason_code: "ingest_pass_backpressured".to_owned(),
                made_progress: true,
            })),
            HistoryContinuation::Immediate,
            "a backpressured window resumes from the coverage it committed"
        );
        assert_eq!(
            history_continuation(Some(SessionHistoricalIngestOutcome::Retryable {
                reason_code: "history_admission_saturated".to_owned(),
                made_progress: false,
            })),
            HistoryContinuation::AwaitRelease
        );
        assert_eq!(
            history_continuation(Some(SessionHistoricalIngestOutcome::Complete)),
            HistoryContinuation::Settled
        );
    }

    #[test]
    fn an_admitted_window_publishes_before_the_next_history_window() {
        let unpublished = SessionTemporalRefreshPassReport {
            projected_batches: 1,
            backlog: Some(84),
            ..SessionTemporalRefreshPassReport::default()
        };
        assert_eq!(
            window_follow_up(
                HistoryContinuation::Immediate,
                projection_still_unpublished(&unpublished),
                projection_published_work(&unpublished),
                false,
            ),
            WindowFollowUp::PublishBeforeNextHistory,
            "a progressing history window with a projection backlog must not open the next window"
        );
        assert_eq!(
            window_follow_up(
                HistoryContinuation::AwaitRelease,
                projection_still_unpublished(&unpublished),
                projection_published_work(&unpublished),
                false,
            ),
            WindowFollowUp::PublishBeforeNextHistory,
            "a retryable yield must publish before its release opens the next window"
        );

        let still_moving = SessionTemporalRefreshPassReport {
            completed: 16,
            backlog: Some(68),
            ..SessionTemporalRefreshPassReport::default()
        };
        assert_eq!(
            window_follow_up(
                HistoryContinuation::Settled,
                projection_still_unpublished(&still_moving),
                projection_published_work(&still_moving),
                true,
            ),
            WindowFollowUp::PublishBeforeNextHistory,
            "projection-only passes keep the hold while the admitted window is still unpublished"
        );

        let published = SessionTemporalRefreshPassReport {
            completed: 16,
            backlog: Some(0),
            ..SessionTemporalRefreshPassReport::default()
        };
        assert_eq!(
            window_follow_up(
                HistoryContinuation::Settled,
                projection_still_unpublished(&published),
                projection_published_work(&published),
                true,
            ),
            WindowFollowUp::ContinueHistory,
            "a published window releases the next history window"
        );

        let stalled = SessionTemporalRefreshPassReport {
            backlog: Some(68),
            ..SessionTemporalRefreshPassReport::default()
        };
        assert_eq!(
            window_follow_up(
                HistoryContinuation::Settled,
                projection_still_unpublished(&stalled),
                projection_published_work(&stalled),
                true,
            ),
            WindowFollowUp::ContinueHistory,
            "a projection pass that moves nothing must not spin ahead of history"
        );

        assert_eq!(
            window_follow_up(HistoryContinuation::AwaitRelease, true, false, false),
            WindowFollowUp::AwaitHistoryRelease,
            "a yield whose projection pass moved nothing still waits for a release"
        );
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
                reason_code: "provider_busy".to_owned(),
                made_progress: false,
            },
        )));
        assert!(history_allows_summary_convergence(Some(
            SessionHistoricalIngestOutcome::Complete,
        )));
        assert!(history_allows_summary_convergence(Some(
            SessionHistoricalIngestOutcome::Blocked {
                reason_code: "invalid_observation_contract".to_owned(),
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
            let admission =
                lcm_convergence_admission(pending.clone(), &mut history_priority_passes);
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
        assert!(database.upsert_session(&session).await);
        let storage_root = database.db_path().parent().unwrap();
        for message in &messages {
            database
                .lcm_ingest_raw_message(storage_root, message)
                .await
                .unwrap();
        }
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
