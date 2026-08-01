//! Daemon lifecycle tracking: drain/idle coordination for graceful shutdown.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tokio::time::Duration;

/// Upper bound on graceful-shutdown persistence work (per-server token
/// persistence and WAL checkpoints). Must stay comfortably below systemd's
/// stop timeout (90s by default) so the daemon exits cleanly instead of
/// being killed with `SIGKILL` mid-checkpoint.
#[cfg(unix)]
pub(crate) const DAEMON_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(45);
#[cfg(unix)]
pub(crate) const DAEMON_SERVER_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(3);
pub(crate) const DAEMON_CLIENT_DRAIN_DEADLINE: Duration = Duration::from_secs(2);
pub(crate) const DAEMON_TASK_ABORT_DEADLINE: Duration = Duration::from_secs(2);

#[derive(Clone, Default)]
pub(crate) struct DaemonLifecycle {
    inner: Arc<DaemonLifecycleInner>,
}

#[derive(Default)]
struct DaemonLifecycleInner {
    draining: AtomicBool,
    active: AtomicUsize,
    idle: tokio::sync::Notify,
    draining_notify: tokio::sync::Notify,
}

pub(crate) struct DaemonActivity {
    inner: Arc<DaemonLifecycleInner>,
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
}

impl Drop for DaemonActivity {
    fn drop(&mut self) {
        if self.inner.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.inner.idle.notify_waiters();
        }
    }
}
