use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_domain::SessionId;
use tracedecay_store::SessionRefreshBeginOrJoinRequestV1;
use tracedecay_temporal_query::ports::ExecutionControl;

use super::history::SessionHistoricalIngestOutcome;
use tracedecay_session_temporal_store::SessionRefreshRecoveryV1;
use tracedecay_sessions::serving::{
    SessionProjectionServingState, SessionProjectionServingStatus,
    SessionProjectionServingStatusPort, SessionProjectionStaleReason,
    SessionProjectionUnavailableReason, SessionProjectionWorkerBlocker,
    SessionProjectionWorkerRetryClass,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionTemporalRefreshRetryClass {
    Storage,
    Projector,
    Deadline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionTemporalRefreshUnavailableReason {
    Missing,
    Recovering,
    Stalled,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionTemporalRefreshBlocker {
    WorkerMissing,
    WorkerPanicked,
    WorkerStopped,
    Storage,
    Projector,
    Deadline,
}

impl From<SessionTemporalRefreshRetryClass> for SessionTemporalRefreshBlocker {
    fn from(value: SessionTemporalRefreshRetryClass) -> Self {
        match value {
            SessionTemporalRefreshRetryClass::Storage => Self::Storage,
            SessionTemporalRefreshRetryClass::Projector => Self::Projector,
            SessionTemporalRefreshRetryClass::Deadline => Self::Deadline,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionTemporalRefreshWorkerStatus {
    pub last_progress_at_unix_micros: Option<i64>,
    pub backlog: usize,
    pub blocker: Option<SessionTemporalRefreshBlocker>,
    pub retry_class: Option<SessionTemporalRefreshRetryClass>,
    pub unavailable_reason: Option<SessionTemporalRefreshUnavailableReason>,
}

impl SessionTemporalRefreshWorkerStatus {
    fn missing() -> Self {
        Self {
            last_progress_at_unix_micros: None,
            backlog: 0,
            blocker: Some(SessionTemporalRefreshBlocker::WorkerMissing),
            retry_class: None,
            unavailable_reason: Some(SessionTemporalRefreshUnavailableReason::Missing),
        }
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[hotpath::skip]
    pub const fn is_available(self) -> bool {
        self.unavailable_reason.is_none()
    }
}

struct SessionTemporalRefreshWorkerTelemetry {
    last_progress_at_unix_micros: Option<i64>,
    last_pass_made_progress: bool,
    queued_backlog: usize,
    durable_backlog: usize,
    blocker: Option<SessionTemporalRefreshBlocker>,
    retry_class: Option<SessionTemporalRefreshRetryClass>,
    unavailable_reason: Option<SessionTemporalRefreshUnavailableReason>,
    historical_state: SessionHistoricalServingState,
    depths_published: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SessionHistoricalServingState {
    Current,
    Pending,
    Retryable(String),
    Blocked(String),
}

impl Default for SessionTemporalRefreshWorkerTelemetry {
    fn default() -> Self {
        Self {
            last_progress_at_unix_micros: None,
            last_pass_made_progress: false,
            queued_backlog: 0,
            durable_backlog: 0,
            blocker: Some(SessionTemporalRefreshBlocker::WorkerStopped),
            retry_class: None,
            unavailable_reason: Some(SessionTemporalRefreshUnavailableReason::Stopped),
            historical_state: SessionHistoricalServingState::Current,
            depths_published: true,
        }
    }
}

macro_rules! define_wake_state {
    ($visibility:vis) => {
        $visibility struct SessionTemporalRefreshWakeState {
            pub(super) dirty: AtomicBool,
            pub(super) historical_dirty: AtomicBool,
            pub(super) requests: std::sync::Mutex<VecDeque<SessionRefreshBeginOrJoinRequestV1>>,
            pub(super) projection_discovery_after: std::sync::Mutex<Option<SessionId>>,
            pub(super) projection_discovery_active_turn: AtomicBool,
            pub(super) terminal_attempts: std::sync::Mutex<HashSet<String>>,
            pub(super) recovery_cycle_pending: std::sync::Mutex<VecDeque<String>>,
            pub(super) busy: AtomicBool,
            pub(super) history_retry_pending: AtomicBool,
            pub(super) pass_count: std::sync::atomic::AtomicUsize,
            pub(super) wake: tokio::sync::Notify,
            pub(super) idle: tokio::sync::Notify,
            pub(super) cancelled: AtomicBool,
            pub(super) cancellation: tokio::sync::Notify,
            completion_control: ExecutionControl,
            telemetry: std::sync::Mutex<SessionTemporalRefreshWorkerTelemetry>,
        }
    };
}

#[cfg(any(test, feature = "test-helpers"))]
define_wake_state!(pub);
#[cfg(not(any(test, feature = "test-helpers")))]
define_wake_state!(pub(crate));

impl Default for SessionTemporalRefreshWakeState {
    fn default() -> Self {
        Self {
            dirty: AtomicBool::new(false),
            historical_dirty: AtomicBool::new(false),
            requests: std::sync::Mutex::new(VecDeque::new()),
            projection_discovery_after: std::sync::Mutex::new(None),
            projection_discovery_active_turn: AtomicBool::new(true),
            terminal_attempts: std::sync::Mutex::new(HashSet::new()),
            recovery_cycle_pending: std::sync::Mutex::new(VecDeque::new()),
            busy: AtomicBool::new(false),
            history_retry_pending: AtomicBool::new(false),
            pass_count: std::sync::atomic::AtomicUsize::new(0),
            wake: tokio::sync::Notify::new(),
            idle: tokio::sync::Notify::new(),
            cancelled: AtomicBool::new(false),
            cancellation: tokio::sync::Notify::new(),
            completion_control: ExecutionControl::new(None),
            telemetry: std::sync::Mutex::new(SessionTemporalRefreshWorkerTelemetry::default()),
        }
    }
}

impl SessionTemporalRefreshWakeState {
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn queued_request_count(&self) -> usize {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn pending_recovery_operations(&self) -> Vec<String> {
        self.recovery_cycle_pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn handle(self: &Arc<Self>) -> SessionTemporalRefreshWake {
        let route = Arc::new(SessionTemporalRefreshWakeRoute {
            target: std::sync::RwLock::new(Arc::downgrade(self)),
        });
        SessionTemporalRefreshWake { route }
    }

    pub fn take_dirty(&self) -> bool {
        let dirty = self.dirty.swap(false, Ordering::AcqRel);
        if dirty {
            hotpath::gauge!("session_temporal_refresh_projection_dirty").inc(-1.0);
        }
        dirty
    }

    pub fn take_historical_dirty(&self) -> bool {
        let dirty = self.historical_dirty.swap(false, Ordering::AcqRel);
        if dirty {
            hotpath::gauge!("session_temporal_refresh_history_dirty").inc(-1.0);
        }
        dirty
    }

    pub fn take_requests(&self, limit: usize) -> Vec<SessionRefreshBeginOrJoinRequestV1> {
        let mut requests = self.requests.lock().unwrap_or_else(PoisonError::into_inner);
        let count = limit.min(requests.len());
        let drained = requests.drain(..count).collect();
        let remaining = requests.len();
        drop(requests);
        self.observe_queued_backlog(remaining);
        drained
    }

    pub fn projection_discovery_after(&self) -> Option<SessionId> {
        self.projection_discovery_after
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn projection_discovery_active_slots(&self, limit: usize) -> usize {
        match limit {
            0 => 0,
            1 => usize::from(
                self.projection_discovery_active_turn
                    .fetch_xor(true, Ordering::AcqRel),
            ),
            _ => 1,
        }
    }

    pub fn update_projection_discovery_cursor(&self, active_after: Option<SessionId>) {
        let mut cursor = self
            .projection_discovery_after
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *cursor = active_after;
    }

    pub fn requeue_request(&self, request: SessionRefreshBeginOrJoinRequestV1) {
        let mut requests = self.requests.lock().unwrap_or_else(PoisonError::into_inner);
        if !requests
            .iter()
            .any(|pending| pending.is_equivalent_to(&request))
        {
            requests.push_front(request);
        }
        let backlog = requests.len();
        drop(requests);
        self.observe_queued_backlog(backlog);
    }

    pub fn transfer_requests_to(&self, target: &Self) {
        let requests = {
            let mut requests = self.requests.lock().unwrap_or_else(PoisonError::into_inner);
            requests.drain(..).collect::<Vec<_>>()
        };
        for request in requests {
            target.requeue_request(request);
        }
        self.observe_queued_backlog(0);
        let projection_dirty = self.take_dirty();
        let historical_dirty = self.take_historical_dirty();
        if projection_dirty || target.has_requests() {
            target.wake();
        }
        if historical_dirty {
            target.wake_history();
        }
    }

    pub fn has_requests(&self) -> bool {
        !self
            .requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }

    pub fn claim_terminal_attempt(&self, recovery: &SessionRefreshRecoveryV1) -> bool {
        self.terminal_attempts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(recovery.operation_id().as_str().to_string())
    }

    pub fn release_terminal_attempt(&self, recovery: &SessionRefreshRecoveryV1) {
        self.terminal_attempts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(recovery.operation_id().as_str());
    }

    pub fn wake(&self) {
        self.requeue_projection();
        self.wake.notify_one();
    }

    pub fn requeue_projection(&self) {
        if !self.dirty.swap(true, Ordering::AcqRel) {
            hotpath::gauge!("session_temporal_refresh_projection_dirty").inc(1.0);
        }
    }

    pub fn wake_history(&self) {
        if !self.historical_dirty.swap(true, Ordering::AcqRel) {
            hotpath::gauge!("session_temporal_refresh_history_dirty").inc(1.0);
        }
        self.wake.notify_one();
    }

    pub fn has_pending_work(&self) -> bool {
        self.dirty.load(Ordering::Acquire) || self.historical_dirty.load(Ordering::Acquire)
    }

    pub fn mark_worker_busy(&self) {
        if !self.busy.swap(true, Ordering::AcqRel) {
            hotpath::gauge!("session_temporal_refresh_workers_busy").inc(1.0);
        }
    }

    pub fn mark_worker_idle(&self) {
        if self.busy.swap(false, Ordering::AcqRel) {
            hotpath::gauge!("session_temporal_refresh_workers_busy").inc(-1.0);
        }
    }

    pub fn clear_worker_instrumentation(&self) {
        self.clear_worker_activity_instrumentation();
        self.take_dirty();
        self.take_historical_dirty();
    }

    pub fn clear_worker_activity_instrumentation(&self) {
        self.mark_worker_idle();
        self.update_history_retry_state(false);
    }

    pub fn history_retry_pending(&self) -> bool {
        self.history_retry_pending.load(Ordering::Acquire)
    }

    pub fn update_history_retry_state(&self, pending: bool) {
        if self.history_retry_pending.swap(pending, Ordering::AcqRel) != pending {
            hotpath::gauge!("session_temporal_refresh_history_retrying").inc(if pending {
                1.0
            } else {
                -1.0
            });
        }
    }

    pub fn mark_running(&self) {
        let mut telemetry = self
            .telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        telemetry.blocker = None;
        telemetry.retry_class = None;
        telemetry.unavailable_reason = None;
    }

    pub fn begin_pass(&self) {
        self.telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last_pass_made_progress = false;
    }

    pub fn mark_history_pending(&self) {
        self.telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .historical_state = SessionHistoricalServingState::Pending;
    }

    pub fn record_history_outcome(&self, outcome: SessionHistoricalIngestOutcome) {
        match outcome {
            SessionHistoricalIngestOutcome::Complete => {
                hotpath::gauge!("session_temporal_refresh_history_complete").inc(1.0);
            }
            SessionHistoricalIngestOutcome::Pending { .. } => {
                hotpath::gauge!("session_temporal_refresh_history_pending").inc(1.0);
            }
            SessionHistoricalIngestOutcome::Retryable { .. } => {
                hotpath::gauge!("session_temporal_refresh_history_retryable").inc(1.0);
            }
            SessionHistoricalIngestOutcome::Blocked { .. } => {
                hotpath::gauge!("session_temporal_refresh_history_blocked").inc(1.0);
            }
            SessionHistoricalIngestOutcome::Cancelled => {
                hotpath::gauge!("session_temporal_refresh_history_cancelled").inc(1.0);
            }
        }
        let state = match outcome {
            SessionHistoricalIngestOutcome::Complete => SessionHistoricalServingState::Current,
            SessionHistoricalIngestOutcome::Pending { .. } => {
                SessionHistoricalServingState::Pending
            }
            SessionHistoricalIngestOutcome::Retryable { reason_code, .. } => {
                SessionHistoricalServingState::Retryable(reason_code.to_owned())
            }
            SessionHistoricalIngestOutcome::Blocked { reason_code, .. } => {
                SessionHistoricalServingState::Blocked(reason_code.to_owned())
            }
            SessionHistoricalIngestOutcome::Cancelled => return,
        };
        self.telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .historical_state = state;
    }

    pub fn recover_history_after_worker_panic(&self) {
        let should_retry = matches!(
            &self
                .telemetry
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .historical_state,
            SessionHistoricalServingState::Pending | SessionHistoricalServingState::Retryable(_)
        );
        if should_retry {
            self.wake_history();
        }
    }

    pub fn mark_recovering(
        &self,
        blocker: SessionTemporalRefreshBlocker,
        retry_class: SessionTemporalRefreshRetryClass,
    ) {
        let mut telemetry = self
            .telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        telemetry.blocker = Some(blocker);
        telemetry.retry_class = Some(retry_class);
        telemetry.unavailable_reason = Some(SessionTemporalRefreshUnavailableReason::Recovering);
    }

    pub fn mark_stopped(&self) {
        let mut telemetry = self
            .telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        telemetry.blocker = Some(SessionTemporalRefreshBlocker::WorkerStopped);
        telemetry.retry_class = None;
        telemetry.unavailable_reason = Some(SessionTemporalRefreshUnavailableReason::Stopped);
    }

    pub fn observe_durable_backlog(&self, backlog: usize) {
        let mut telemetry = self
            .telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if telemetry.depths_published {
            hotpath::gauge!("session_temporal_refresh_durable_depth")
                .inc(bounded_depth(backlog) - bounded_depth(telemetry.durable_backlog));
        }
        telemetry.durable_backlog = backlog;
    }

    pub fn record_pass(&self, durable_backlog: usize, made_progress: bool) {
        let mut telemetry = self
            .telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let previous = telemetry.durable_backlog;
        telemetry.durable_backlog = durable_backlog;
        telemetry.last_pass_made_progress = made_progress;
        if made_progress {
            let micros = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros()
                .min(i64::MAX as u128) as i64;
            telemetry.last_progress_at_unix_micros = Some(micros);
        }
        if telemetry.depths_published {
            hotpath::gauge!("session_temporal_refresh_durable_depth")
                .inc(bounded_depth(durable_backlog) - bounded_depth(previous));
        }
    }

    fn observe_queued_backlog(&self, backlog: usize) {
        let mut telemetry = self
            .telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if telemetry.depths_published {
            hotpath::gauge!("session_temporal_refresh_queue_depth")
                .inc(bounded_depth(backlog) - bounded_depth(telemetry.queued_backlog));
        }
        telemetry.queued_backlog = backlog;
    }

    fn status(&self) -> SessionTemporalRefreshWorkerStatus {
        let telemetry = self
            .telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let unavailable_reason = match (
            telemetry.unavailable_reason,
            telemetry.last_pass_made_progress,
        ) {
            (Some(SessionTemporalRefreshUnavailableReason::Recovering), false) => {
                Some(SessionTemporalRefreshUnavailableReason::Stalled)
            }
            (reason, _) => reason,
        };
        SessionTemporalRefreshWorkerStatus {
            last_progress_at_unix_micros: telemetry.last_progress_at_unix_micros,
            backlog: telemetry
                .queued_backlog
                .saturating_add(telemetry.durable_backlog),
            blocker: telemetry.blocker,
            retry_class: telemetry.retry_class,
            unavailable_reason,
        }
    }

    fn serving_status(&self) -> SessionProjectionServingStatus {
        let worker = self.status();
        let historical_state = self
            .telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .historical_state
            .clone();
        SessionProjectionServingStatus {
            state: match worker.unavailable_reason {
                Some(reason) => SessionProjectionServingState::Unavailable {
                    reason: match reason {
                        SessionTemporalRefreshUnavailableReason::Missing => {
                            SessionProjectionUnavailableReason::WorkerMissing
                        }
                        SessionTemporalRefreshUnavailableReason::Recovering => {
                            SessionProjectionUnavailableReason::WorkerRecovering
                        }
                        SessionTemporalRefreshUnavailableReason::Stalled => {
                            SessionProjectionUnavailableReason::WorkerStalled
                        }
                        SessionTemporalRefreshUnavailableReason::Stopped => {
                            SessionProjectionUnavailableReason::WorkerStopped
                        }
                    },
                },
                None => match historical_state {
                    SessionHistoricalServingState::Current => {
                        SessionProjectionServingState::Current
                    }
                    SessionHistoricalServingState::Pending => {
                        SessionProjectionServingState::Stale {
                            reason: SessionProjectionStaleReason::HistoricalConvergence,
                        }
                    }
                    SessionHistoricalServingState::Retryable(reason_code) => {
                        SessionProjectionServingState::Stale {
                            reason: SessionProjectionStaleReason::HistoricalRetry { reason_code },
                        }
                    }
                    SessionHistoricalServingState::Blocked(reason_code) => {
                        SessionProjectionServingState::Stale {
                            reason: SessionProjectionStaleReason::HistoricalBlocked { reason_code },
                        }
                    }
                },
            },
            last_progress_at_unix_micros: worker.last_progress_at_unix_micros,
            backlog: worker.backlog,
            blocker: worker.blocker.map(|blocker| match blocker {
                SessionTemporalRefreshBlocker::WorkerMissing => {
                    SessionProjectionWorkerBlocker::WorkerMissing
                }
                SessionTemporalRefreshBlocker::WorkerPanicked => {
                    SessionProjectionWorkerBlocker::WorkerPanicked
                }
                SessionTemporalRefreshBlocker::WorkerStopped => {
                    SessionProjectionWorkerBlocker::WorkerStopped
                }
                SessionTemporalRefreshBlocker::Storage => SessionProjectionWorkerBlocker::Storage,
                SessionTemporalRefreshBlocker::Projector => {
                    SessionProjectionWorkerBlocker::Projector
                }
                SessionTemporalRefreshBlocker::Deadline => SessionProjectionWorkerBlocker::Deadline,
            }),
            retry_class: worker.retry_class.map(|retry_class| match retry_class {
                SessionTemporalRefreshRetryClass::Storage => {
                    SessionProjectionWorkerRetryClass::Storage
                }
                SessionTemporalRefreshRetryClass::Projector => {
                    SessionProjectionWorkerRetryClass::Projector
                }
                SessionTemporalRefreshRetryClass::Deadline => {
                    SessionProjectionWorkerRetryClass::Deadline
                }
            }),
        }
    }

    pub fn cancel(&self) {
        let _requests = self.requests.lock().unwrap_or_else(PoisonError::into_inner);
        if !self.cancelled.swap(true, Ordering::AcqRel) {
            self.completion_control.cancel();
            self.clear_worker_instrumentation();
            self.clear_depth_instrumentation();
            self.mark_stopped();
            self.cancellation.notify_waiters();
            self.wake.notify_waiters();
        }
    }

    fn clear_depth_instrumentation(&self) {
        let mut telemetry = self
            .telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !telemetry.depths_published {
            return;
        }
        hotpath::gauge!("session_temporal_refresh_queue_depth")
            .inc(-bounded_depth(telemetry.queued_backlog));
        hotpath::gauge!("session_temporal_refresh_durable_depth")
            .inc(-bounded_depth(telemetry.durable_backlog));
        telemetry.depths_published = false;
    }

    pub fn completion_control(&self) -> ExecutionControl {
        self.completion_control.clone()
    }

    #[hotpath::skip]
    pub async fn wait_for_cancellation(&self) {
        loop {
            let notified = self.cancellation.notified();
            if self.cancelled.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn is_idle(&self) -> bool {
        !self.busy.load(Ordering::Acquire) && !self.has_pending_work()
    }
}

impl Drop for SessionTemporalRefreshWakeState {
    fn drop(&mut self) {
        self.clear_worker_instrumentation();
        self.clear_depth_instrumentation();
    }
}

fn bounded_depth(depth: usize) -> f64 {
    depth.min(u32::MAX as usize) as f64
}

pub(crate) struct TerminalAttemptGuard<'a> {
    state: &'a SessionTemporalRefreshWakeState,
    recovery: &'a SessionRefreshRecoveryV1,
    retain: bool,
}

impl<'a> TerminalAttemptGuard<'a> {
    pub(crate) fn new(
        state: &'a SessionTemporalRefreshWakeState,
        recovery: &'a SessionRefreshRecoveryV1,
    ) -> Self {
        Self {
            state,
            recovery,
            retain: false,
        }
    }

    pub(crate) fn retain(&mut self) {
        self.retain = true;
    }
}

impl Drop for TerminalAttemptGuard<'_> {
    fn drop(&mut self) {
        if !self.retain {
            self.state.release_terminal_attempt(self.recovery);
        }
    }
}

pub(crate) struct PendingBeginRequestGuard<'a> {
    state: &'a SessionTemporalRefreshWakeState,
    request: Option<SessionRefreshBeginOrJoinRequestV1>,
}

impl<'a> PendingBeginRequestGuard<'a> {
    pub(crate) fn new(
        state: &'a SessionTemporalRefreshWakeState,
        request: SessionRefreshBeginOrJoinRequestV1,
    ) -> Self {
        Self {
            state,
            request: Some(request),
        }
    }

    // Armed guards always hold a request; request() is only called before disarm().
    #[allow(clippy::expect_used)]
    pub(crate) fn request(&self) -> &SessionRefreshBeginOrJoinRequestV1 {
        self.request.as_ref().expect("pending request disarmed")
    }

    pub(crate) fn disarm(&mut self) {
        self.request = None;
    }
}

impl Drop for PendingBeginRequestGuard<'_> {
    fn drop(&mut self) {
        if let Some(request) = self.request.take() {
            self.state.requeue_request(request);
        }
    }
}

pub(crate) struct RecoverySelectionGuard<'a> {
    state: &'a SessionTemporalRefreshWakeState,
    pending: VecDeque<String>,
}

impl<'a> RecoverySelectionGuard<'a> {
    pub(crate) fn new(state: &'a SessionTemporalRefreshWakeState, pending: Vec<String>) -> Self {
        Self {
            state,
            pending: pending.into(),
        }
    }

    pub(crate) fn complete(&mut self, operation: &str) {
        // Resolve by identity so skipped/missing recoveries cannot desync the
        // local queue from the operations actually projected this pass.
        if let Some(index) = self.pending.iter().position(|item| item == operation) {
            self.pending.remove(index);
        }
    }
}

impl Drop for RecoverySelectionGuard<'_> {
    fn drop(&mut self) {
        let mut cycle = self
            .state
            .recovery_cycle_pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while let Some(operation) = self.pending.pop_back() {
            if !cycle.contains(&operation) {
                cycle.push_front(operation);
            }
        }
    }
}

struct SessionTemporalRefreshWakeRoute {
    target: std::sync::RwLock<std::sync::Weak<SessionTemporalRefreshWakeState>>,
}

fn enabled_idle_notification(
    state: &SessionTemporalRefreshWakeState,
) -> std::pin::Pin<Box<tokio::sync::futures::Notified<'_>>> {
    let mut idle = Box::pin(state.idle.notified());
    idle.as_mut().enable();
    idle
}

#[derive(Clone)]
pub struct SessionTemporalRefreshWake {
    route: Arc<SessionTemporalRefreshWakeRoute>,
}

impl SessionTemporalRefreshWake {
    pub fn unavailable() -> Self {
        Self {
            route: Arc::new(SessionTemporalRefreshWakeRoute {
                target: std::sync::RwLock::new(std::sync::Weak::new()),
            }),
        }
    }

    pub(crate) fn target(&self) -> Option<Arc<SessionTemporalRefreshWakeState>> {
        self.route
            .target
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .upgrade()
    }

    pub(crate) fn bind(&self, state: &Arc<SessionTemporalRefreshWakeState>) {
        *self
            .route
            .target
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Arc::downgrade(state);
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn same_route(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.route, &other.route)
    }

    /// Delivers a wake to the currently bound production worker.
    ///
    /// `false` means the route has no live target (including a retired owner)
    /// or the target is already cancelled. Callers crossing a durable effect
    /// boundary must preserve that distinction for reconciliation.
    pub fn wake(&self) -> bool {
        let Some(state) = self.target() else {
            return false;
        };
        if state.cancelled.load(Ordering::Acquire) {
            return false;
        }
        state.wake();
        true
    }

    /// Wake the refresh worker and wait until it finishes the resulting pass.
    ///
    /// Live LCM transcript projection writes must become temporally searchable
    /// before the mutating tool returns so Hermes `sync_turn` → `lcm_grep`
    /// stays available on the same route without inventing stores or weakening
    /// empty/unavailable semantics.
    #[hotpath::skip]
    pub async fn wake_and_wait_until_idle(&self, timeout: std::time::Duration) -> bool {
        let Some(state) = self.target() else {
            return false;
        };
        let before = state.pass_count.load(Ordering::Acquire);
        state.wake();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let idle = hotpath::future!(
                enabled_idle_notification(&state),
                label = "daemon.scheduler.session_temporal.idle_wait"
            );
            let pass_count = state.pass_count.load(Ordering::Acquire);
            let busy = state.busy.load(Ordering::Acquire);
            let dirty = state.dirty.load(Ordering::Acquire);
            let pending = state.has_requests();
            if pass_count > before && !busy && !dirty && !pending {
                return true;
            }
            if tokio::time::timeout_at(deadline, idle).await.is_err() {
                let pass_count = state.pass_count.load(Ordering::Acquire);
                return pass_count > before
                    && !state.busy.load(Ordering::Acquire)
                    && !state.dirty.load(Ordering::Acquire)
                    && !state.has_requests();
            }
        }
    }

    pub fn status(&self) -> SessionTemporalRefreshWorkerStatus {
        self.target()
            .map_or_else(SessionTemporalRefreshWorkerStatus::missing, |state| {
                state.status()
            })
    }
}

impl tracedecay_application::SessionTemporalRefreshWakePort for SessionTemporalRefreshWake {
    fn wake(&self) -> bool {
        SessionTemporalRefreshWake::wake(self)
    }

    fn is_unavailable(&self) -> bool {
        self.status().unavailable_reason.is_some()
    }

    fn wake_and_wait_until_idle(
        &self,
        timeout: std::time::Duration,
    ) -> tracedecay_application::SessionTemporalRefreshWakeFuture<'_> {
        Box::pin(SessionTemporalRefreshWake::wake_and_wait_until_idle(
            self, timeout,
        ))
    }
}

impl SessionProjectionServingStatusPort for SessionTemporalRefreshWake {
    fn serving_status(&self) -> SessionProjectionServingStatus {
        self.target().map_or_else(
            || SessionProjectionServingStatus {
                state: SessionProjectionServingState::Unavailable {
                    reason: SessionProjectionUnavailableReason::WorkerMissing,
                },
                last_progress_at_unix_micros: None,
                backlog: 0,
                blocker: Some(SessionProjectionWorkerBlocker::WorkerMissing),
                retry_class: None,
            },
            |state| state.serving_status(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn idle_waiter_is_registered_before_the_state_recheck() {
        let state = SessionTemporalRefreshWakeState::default();
        let idle = enabled_idle_notification(&state);

        state.idle.notify_waiters();

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), idle)
                .await
                .is_ok(),
            "a completion between waiter creation and await must not be lost"
        );
    }

    #[test]
    fn recovery_without_any_progress_is_reported_as_stalled() {
        let state = SessionTemporalRefreshWakeState::default();
        state.observe_durable_backlog(14);
        state.mark_recovering(
            SessionTemporalRefreshBlocker::Storage,
            SessionTemporalRefreshRetryClass::Storage,
        );

        let stalled = state.status();
        assert_eq!(
            stalled.unavailable_reason,
            Some(SessionTemporalRefreshUnavailableReason::Stalled)
        );
        assert_eq!(stalled.last_progress_at_unix_micros, None);
        assert_eq!(stalled.backlog, 14);
        assert_eq!(
            stalled.blocker,
            Some(SessionTemporalRefreshBlocker::Storage)
        );

        state.record_pass(13, true);
        state.mark_recovering(
            SessionTemporalRefreshBlocker::Storage,
            SessionTemporalRefreshRetryClass::Storage,
        );
        let progressing = state.status();
        assert_eq!(
            progressing.unavailable_reason,
            Some(SessionTemporalRefreshUnavailableReason::Recovering)
        );
        assert!(progressing.last_progress_at_unix_micros.is_some());

        state.record_pass(13, false);
        state.mark_recovering(
            SessionTemporalRefreshBlocker::Storage,
            SessionTemporalRefreshRetryClass::Storage,
        );
        let stalled_after_progress = state.status();
        assert_eq!(
            stalled_after_progress.unavailable_reason,
            Some(SessionTemporalRefreshUnavailableReason::Stalled)
        );
        assert!(
            stalled_after_progress
                .last_progress_at_unix_micros
                .is_some()
        );
    }

    #[test]
    fn cancellation_clears_pending_worker_instrumentation() {
        let state = SessionTemporalRefreshWakeState::default();
        state.requeue_projection();
        state.wake_history();
        state.mark_worker_busy();
        state.update_history_retry_state(true);

        state.cancel();

        assert!(!state.dirty.load(Ordering::Acquire));
        assert!(!state.historical_dirty.load(Ordering::Acquire));
        assert!(!state.busy.load(Ordering::Acquire));
        assert!(!state.history_retry_pending());
    }
}
