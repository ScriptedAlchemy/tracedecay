//! Daemon lifecycle tracking: drain/idle coordination for graceful shutdown.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tokio::time::Duration;

use super::shutdown_orchestration::DaemonShutdownReceipt;

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
    shutdown_claimed: AtomicBool,
    shutdown_receipt: tokio::sync::watch::Sender<Option<Arc<DaemonShutdownReceipt>>>,
}

pub(crate) struct DaemonActivity {
    inner: Arc<DaemonLifecycleInner>,
}

impl Default for DaemonLifecycle {
    fn default() -> Self {
        let (shutdown_receipt, _) = tokio::sync::watch::channel(None);
        Self {
            inner: Arc::new(DaemonLifecycleInner {
                draining: AtomicBool::new(false),
                active: AtomicUsize::new(0),
                idle: tokio::sync::Notify::new(),
                draining_notify: tokio::sync::Notify::new(),
                shutdown_claimed: AtomicBool::new(false),
                shutdown_receipt,
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

    pub(super) fn claim_shutdown_coordination(&self) -> bool {
        self.inner
            .shutdown_claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(super) fn publish_shutdown_receipt(&self, receipt: Arc<DaemonShutdownReceipt>) {
        self.inner.shutdown_receipt.send_replace(Some(receipt));
    }

    pub(super) async fn wait_for_shutdown_receipt(
        &self,
    ) -> std::result::Result<Arc<DaemonShutdownReceipt>, String> {
        let mut receipt = self.inner.shutdown_receipt.subscribe();
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
