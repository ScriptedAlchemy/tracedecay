//! Daemon lifecycle tracking: drain/idle coordination for graceful shutdown.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tokio::time::Duration;

use super::shutdown_orchestration::{DaemonShutdownFailures, DaemonShutdownReceipt};

/// Upper bound on graceful-shutdown persistence work (per-server token
/// persistence and WAL checkpoints). Must stay comfortably below systemd's
/// stop timeout (90s by default) so the daemon exits cleanly instead of
/// being killed with `SIGKILL` mid-checkpoint.
pub(crate) const DAEMON_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(45);
pub(crate) const DAEMON_CLIENT_DRAIN_DEADLINE: Duration = Duration::from_secs(2);
pub(crate) const DAEMON_TASK_ABORT_DEADLINE: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub(crate) struct DaemonLifecycle {
    inner: Arc<DaemonLifecycleInner>,
}

struct DaemonLifecycleInner {
    draining: AtomicBool,
    active: AtomicUsize,
    idle: tokio::sync::Notify,
    draining_notify: tokio::sync::Notify,
    shutdown: std::sync::Mutex<DaemonShutdownCoordinator>,
}

pub(crate) struct DaemonActivity {
    inner: Arc<DaemonLifecycleInner>,
}

#[derive(Default)]
struct DaemonShutdownCoordinator {
    in_flight: Option<Arc<DaemonShutdownAttempt>>,
    terminal: Option<Arc<DaemonShutdownReceipt>>,
    failures: DaemonShutdownFailures,
}

pub(super) struct DaemonShutdownAttempt {
    receipt: tokio::sync::watch::Sender<Option<Arc<DaemonShutdownReceipt>>>,
}

pub(super) enum DaemonShutdownClaim {
    Run {
        attempt: Arc<DaemonShutdownAttempt>,
        failures: DaemonShutdownFailures,
    },
    Wait(Arc<DaemonShutdownAttempt>),
    Terminal(Arc<DaemonShutdownReceipt>),
}

impl Default for DaemonLifecycle {
    fn default() -> Self {
        Self {
            inner: Arc::new(DaemonLifecycleInner {
                draining: AtomicBool::new(false),
                active: AtomicUsize::new(0),
                idle: tokio::sync::Notify::new(),
                draining_notify: tokio::sync::Notify::new(),
                shutdown: std::sync::Mutex::new(DaemonShutdownCoordinator::default()),
            }),
        }
    }
}

impl DaemonLifecycle {
    pub(crate) fn accepting(&self) -> bool {
        !self.inner.draining.load(Ordering::Acquire)
    }

    pub(crate) fn try_enter(&self) -> Option<DaemonActivity> {
        if !self.accepting() {
            return None;
        }
        self.inner.active.fetch_add(1, Ordering::AcqRel);
        if self.accepting() {
            Some(DaemonActivity {
                inner: Arc::clone(&self.inner),
            })
        } else {
            if self.inner.active.fetch_sub(1, Ordering::AcqRel) == 1 {
                self.inner.idle.notify_waiters();
            }
            None
        }
    }

    pub(crate) fn begin_draining(&self) {
        if !self.inner.draining.swap(true, Ordering::AcqRel) {
            self.inner.draining_notify.notify_waiters();
        }
    }

    pub(crate) async fn wait_for_draining(&self) {
        loop {
            let notified = self.inner.draining_notify.notified();
            if !self.accepting() {
                return;
            }
            notified.await;
        }
    }

    pub(crate) async fn wait_for_idle(&self) {
        loop {
            let notified = self.inner.idle.notified();
            if self.inner.active.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }

    pub(super) fn claim_shutdown_coordination(&self) -> DaemonShutdownClaim {
        let mut shutdown = self
            .inner
            .shutdown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(receipt) = &shutdown.terminal {
            return DaemonShutdownClaim::Terminal(Arc::clone(receipt));
        }
        if let Some(attempt) = &shutdown.in_flight {
            return DaemonShutdownClaim::Wait(Arc::clone(attempt));
        }
        let (receipt, _) = tokio::sync::watch::channel(None);
        let attempt = Arc::new(DaemonShutdownAttempt { receipt });
        shutdown.in_flight = Some(Arc::clone(&attempt));
        DaemonShutdownClaim::Run {
            attempt,
            failures: shutdown.failures.clone(),
        }
    }

    pub(super) fn finish_shutdown_attempt(
        &self,
        attempt: &Arc<DaemonShutdownAttempt>,
        receipt: Arc<DaemonShutdownReceipt>,
        failures: DaemonShutdownFailures,
    ) {
        let mut shutdown = self
            .inner
            .shutdown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if shutdown
            .in_flight
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, attempt))
        {
            shutdown.in_flight = None;
            shutdown.failures = failures;
            if !receipt.is_retryable() {
                shutdown.terminal = Some(Arc::clone(&receipt));
            }
        }
        drop(shutdown);
        attempt.receipt.send_replace(Some(receipt));
    }
}

impl DaemonShutdownAttempt {
    pub(super) async fn wait_for_receipt(
        &self,
    ) -> std::result::Result<Arc<DaemonShutdownReceipt>, String> {
        let mut receipt = self.receipt.subscribe();
        loop {
            if let Some(receipt) = receipt.borrow_and_update().clone() {
                return Ok(receipt);
            }
            receipt
                .changed()
                .await
                .map_err(|error| format!("shutdown receipt channel closed: {error}"))?;
        }
    }
}

impl Drop for DaemonActivity {
    fn drop(&mut self) {
        if self.inner.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.inner.idle.notify_waiters();
        }
    }
}
