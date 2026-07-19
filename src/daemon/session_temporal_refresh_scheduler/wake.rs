use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_store::SessionRefreshBeginOrJoinRequestV1;

use super::MAX_PENDING_REFRESH_REQUESTS;
use crate::store::SessionRefreshRecoveryV1;

#[derive(Default)]
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
        let mut requests = self
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let count = limit.min(requests.len());
        requests.drain(..count).collect()
    }

    pub(super) fn requeue_request(&self, request: SessionRefreshBeginOrJoinRequestV1) {
        let mut requests = self
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !requests
            .iter()
            .any(|pending| pending.is_equivalent_to(&request))
        {
            requests.push_front(request);
        }
    }

    pub(super) fn transfer_requests_to(&self, target: &Self) {
        let requests = {
            let mut requests = self
                .requests
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            requests.drain(..).collect::<Vec<_>>()
        };
        for request in requests {
            target.requeue_request(request);
        }
        if self.take_dirty() || target.has_requests() {
            target.wake();
        }
    }

    pub(super) fn has_requests(&self) -> bool {
        !self
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
    }

    pub(super) fn claim_terminal_attempt(&self, recovery: &SessionRefreshRecoveryV1) -> bool {
        self.terminal_attempts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(recovery.operation_id().as_str().to_string())
    }

    pub(super) fn release_terminal_attempt(&self, recovery: &SessionRefreshRecoveryV1) {
        self.terminal_attempts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(recovery.operation_id().as_str());
    }

    pub(super) fn wake(&self) {
        self.dirty.store(true, Ordering::Release);
        self.wake.notify_one();
    }

    pub(super) fn cancel(&self) {
        let _requests = self
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !self.cancelled.swap(true, Ordering::AcqRel) {
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
            .unwrap_or_else(|error| error.into_inner());
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
    pub(super) fn target(&self) -> Option<Arc<SessionTemporalRefreshWakeState>> {
        self.route
            .target
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .upgrade()
    }

    pub(super) fn bind(&self, state: &Arc<SessionTemporalRefreshWakeState>) {
        *self
            .route
            .target
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Arc::downgrade(state);
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

    #[allow(dead_code)] // Slice 3 maps admitted source frontiers into begin requests.
    pub(crate) fn request(
        &self,
        request: SessionRefreshBeginOrJoinRequestV1,
    ) -> SessionTemporalRefreshWakeDisposition {
        let Some(state) = self.target() else {
            return SessionTemporalRefreshWakeDisposition::Saturated;
        };
        let disposition = {
            let mut requests = state
                .requests
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if state.cancelled.load(Ordering::Acquire) {
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
            }
        };
        if disposition != SessionTemporalRefreshWakeDisposition::Saturated {
            state.wake();
        }
        disposition
    }
}
