use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_store::SessionRefreshBeginOrJoinRequestV1;

use super::MAX_PENDING_REFRESH_REQUESTS;
use crate::store::SessionRefreshRecoveryV1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionTemporalRefreshRetryClass {
    Storage,
    Projector,
    Deadline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionTemporalRefreshUnavailableReason {
    Missing,
    Recovering,
    Stalled,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionTemporalRefreshBlocker {
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
pub(crate) struct SessionTemporalRefreshWorkerStatus {
    pub(crate) last_progress_at_unix_micros: Option<i64>,
    pub(crate) backlog: usize,
    pub(crate) blocker: Option<SessionTemporalRefreshBlocker>,
    pub(crate) retry_class: Option<SessionTemporalRefreshRetryClass>,
    pub(crate) unavailable_reason: Option<SessionTemporalRefreshUnavailableReason>,
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

    #[cfg(test)]
    pub(crate) const fn is_available(self) -> bool {
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
        }
    }
}

pub(super) struct SessionTemporalRefreshWakeState {
    pub(super) dirty: AtomicBool,
    pub(super) requests: std::sync::Mutex<VecDeque<SessionRefreshBeginOrJoinRequestV1>>,
    pub(super) terminal_attempts: std::sync::Mutex<HashSet<String>>,
    pub(super) recovery_cycle_pending: std::sync::Mutex<VecDeque<String>>,
    pub(super) busy: AtomicBool,
    pub(super) pass_count: std::sync::atomic::AtomicUsize,
    pub(super) wake: tokio::sync::Notify,
    pub(super) idle: tokio::sync::Notify,
    pub(super) cancelled: AtomicBool,
    pub(super) cancellation: tokio::sync::Notify,
    telemetry: std::sync::Mutex<SessionTemporalRefreshWorkerTelemetry>,
}

impl Default for SessionTemporalRefreshWakeState {
    fn default() -> Self {
        Self {
            dirty: AtomicBool::new(false),
            requests: std::sync::Mutex::new(VecDeque::new()),
            terminal_attempts: std::sync::Mutex::new(HashSet::new()),
            recovery_cycle_pending: std::sync::Mutex::new(VecDeque::new()),
            busy: AtomicBool::new(false),
            pass_count: std::sync::atomic::AtomicUsize::new(0),
            wake: tokio::sync::Notify::new(),
            idle: tokio::sync::Notify::new(),
            cancelled: AtomicBool::new(false),
            cancellation: tokio::sync::Notify::new(),
            telemetry: std::sync::Mutex::new(SessionTemporalRefreshWorkerTelemetry::default()),
        }
    }
}

impl SessionTemporalRefreshWakeState {
    pub(super) fn handle(self: &Arc<Self>) -> SessionTemporalRefreshWake {
        let route = Arc::new(SessionTemporalRefreshWakeRoute {
            target: std::sync::RwLock::new(Arc::downgrade(self)),
        });
        SessionTemporalRefreshWake { route }
    }

    pub(super) fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    pub(super) fn take_requests(&self, limit: usize) -> Vec<SessionRefreshBeginOrJoinRequestV1> {
        let mut requests = self.requests.lock().unwrap_or_else(PoisonError::into_inner);
        let count = limit.min(requests.len());
        let drained = requests.drain(..count).collect();
        let remaining = requests.len();
        drop(requests);
        self.observe_queued_backlog(remaining);
        drained
    }

    pub(super) fn requeue_request(&self, request: SessionRefreshBeginOrJoinRequestV1) {
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

    pub(super) fn transfer_requests_to(&self, target: &Self) {
        let requests = {
            let mut requests = self.requests.lock().unwrap_or_else(PoisonError::into_inner);
            requests.drain(..).collect::<Vec<_>>()
        };
        for request in requests {
            target.requeue_request(request);
        }
        self.observe_queued_backlog(0);
        if self.take_dirty() || target.has_requests() {
            target.wake();
        }
    }

    pub(super) fn has_requests(&self) -> bool {
        !self
            .requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }

    pub(super) fn claim_terminal_attempt(&self, recovery: &SessionRefreshRecoveryV1) -> bool {
        self.terminal_attempts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(recovery.operation_id().as_str().to_string())
    }

    pub(super) fn release_terminal_attempt(&self, recovery: &SessionRefreshRecoveryV1) {
        self.terminal_attempts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(recovery.operation_id().as_str());
    }

    pub(super) fn wake(&self) {
        self.dirty.store(true, Ordering::Release);
        self.wake.notify_one();
    }

    pub(super) fn mark_running(&self) {
        let mut telemetry = self
            .telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        telemetry.blocker = None;
        telemetry.retry_class = None;
        telemetry.unavailable_reason = None;
    }

    pub(super) fn begin_pass(&self) {
        self.telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last_pass_made_progress = false;
    }

    pub(super) fn mark_recovering(
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

    pub(super) fn mark_stopped(&self) {
        let mut telemetry = self
            .telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        telemetry.blocker = Some(SessionTemporalRefreshBlocker::WorkerStopped);
        telemetry.retry_class = None;
        telemetry.unavailable_reason = Some(SessionTemporalRefreshUnavailableReason::Stopped);
    }

    pub(super) fn observe_durable_backlog(&self, backlog: usize) {
        self.telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .durable_backlog = backlog;
    }

    pub(super) fn record_pass(&self, durable_backlog: usize, made_progress: bool) {
        let mut telemetry = self
            .telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
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
    }

    fn observe_queued_backlog(&self, backlog: usize) {
        self.telemetry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .queued_backlog = backlog;
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

    pub(super) fn cancel(&self) {
        let _requests = self.requests.lock().unwrap_or_else(PoisonError::into_inner);
        if !self.cancelled.swap(true, Ordering::AcqRel) {
            self.mark_stopped();
            self.cancellation.notify_waiters();
            self.wake.notify_waiters();
        }
    }

    pub(super) async fn wait_for_cancellation(&self) {
        loop {
            let notified = self.cancellation.notified();
            if self.cancelled.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    #[cfg(test)]
    pub(super) fn is_idle(&self) -> bool {
        !self.busy.load(Ordering::Acquire) && !self.dirty.load(Ordering::Acquire)
    }
}

pub(super) struct TerminalAttemptGuard<'a> {
    state: &'a SessionTemporalRefreshWakeState,
    recovery: &'a SessionRefreshRecoveryV1,
    retain: bool,
}

impl<'a> TerminalAttemptGuard<'a> {
    pub(super) fn new(
        state: &'a SessionTemporalRefreshWakeState,
        recovery: &'a SessionRefreshRecoveryV1,
    ) -> Self {
        Self {
            state,
            recovery,
            retain: false,
        }
    }

    pub(super) fn retain(&mut self) {
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

pub(super) struct PendingBeginRequestGuard<'a> {
    state: &'a SessionTemporalRefreshWakeState,
    request: Option<SessionRefreshBeginOrJoinRequestV1>,
}

impl<'a> PendingBeginRequestGuard<'a> {
    pub(super) fn new(
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
    pub(super) fn request(&self) -> &SessionRefreshBeginOrJoinRequestV1 {
        self.request.as_ref().expect("pending request disarmed")
    }

    pub(super) fn disarm(&mut self) {
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

pub(super) struct RecoverySelectionGuard<'a> {
    state: &'a SessionTemporalRefreshWakeState,
    pending: VecDeque<String>,
}

impl<'a> RecoverySelectionGuard<'a> {
    pub(super) fn new(state: &'a SessionTemporalRefreshWakeState, pending: Vec<String>) -> Self {
        Self {
            state,
            pending: pending.into(),
        }
    }

    pub(super) fn complete(&mut self, operation: &str) {
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
pub(crate) struct SessionTemporalRefreshWake {
    route: Arc<SessionTemporalRefreshWakeRoute>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // Slice 3 consumes the queued-request disposition.
pub(crate) enum SessionTemporalRefreshWakeDisposition {
    Enqueued,
    Coalesced,
    Saturated,
}

impl SessionTemporalRefreshWake {
    pub(crate) fn unavailable() -> Self {
        Self {
            route: Arc::new(SessionTemporalRefreshWakeRoute {
                target: std::sync::RwLock::new(std::sync::Weak::new()),
            }),
        }
    }

    pub(super) fn target(&self) -> Option<Arc<SessionTemporalRefreshWakeState>> {
        self.route
            .target
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .upgrade()
    }

    pub(super) fn bind(&self, state: &Arc<SessionTemporalRefreshWakeState>) {
        *self
            .route
            .target
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Arc::downgrade(state);
    }

    #[cfg(test)]
    pub(super) fn same_route(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.route, &other.route)
    }

    pub(crate) fn wake(&self) {
        if let Some(state) = self.target() {
            state.wake();
        }
    }

    /// Wake the refresh worker and wait until it finishes the resulting pass.
    ///
    /// Live LCM transcript projection writes must become temporally searchable
    /// before the mutating tool returns so Hermes `sync_turn` → `lcm_grep`
    /// stays available on the same route without inventing stores or weakening
    /// empty/unavailable semantics.
    pub(crate) async fn wake_and_wait_until_idle(&self, timeout: std::time::Duration) -> bool {
        let Some(state) = self.target() else {
            return false;
        };
        let before = state.pass_count.load(Ordering::Acquire);
        state.wake();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let idle = enabled_idle_notification(&state);
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

    pub(crate) fn status(&self) -> SessionTemporalRefreshWorkerStatus {
        self.target()
            .map_or_else(SessionTemporalRefreshWorkerStatus::missing, |state| {
                state.status()
            })
    }

    #[allow(dead_code)] // Slice 3 maps admitted source frontiers into begin requests.
    pub(crate) fn request(
        &self,
        request: SessionRefreshBeginOrJoinRequestV1,
    ) -> SessionTemporalRefreshWakeDisposition {
        let Some(state) = self.target() else {
            return SessionTemporalRefreshWakeDisposition::Saturated;
        };
        let (disposition, backlog) = {
            let mut requests = state
                .requests
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let disposition = if state.cancelled.load(Ordering::Acquire) {
                SessionTemporalRefreshWakeDisposition::Saturated
            } else if requests
                .iter()
                .any(|pending| pending.is_equivalent_to(&request))
            {
                SessionTemporalRefreshWakeDisposition::Coalesced
            } else if requests.len() >= MAX_PENDING_REFRESH_REQUESTS {
                SessionTemporalRefreshWakeDisposition::Saturated
            } else {
                requests.push_back(request);
                SessionTemporalRefreshWakeDisposition::Enqueued
            };
            (disposition, requests.len())
        };
        if disposition != SessionTemporalRefreshWakeDisposition::Saturated {
            state.observe_queued_backlog(backlog);
            state.wake();
        }
        disposition
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
}
