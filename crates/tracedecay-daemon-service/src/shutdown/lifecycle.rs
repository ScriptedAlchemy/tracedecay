//! Daemon lifecycle tracking: drain/idle coordination for graceful shutdown.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tokio::time::Duration;

pub use tracedecay_session_runtime::DAEMON_CLIENT_DRAIN_DEADLINE;

use super::orchestration::{DaemonShutdownFailures, DaemonShutdownReceipt};

pub const DAEMON_TASK_ABORT_DEADLINE: Duration = crate::TASK_ABORT_DEADLINE;

/// Per-phase shutdown budgets.
///
/// A single global deadline shared by every phase lets one stuck phase spend
/// the whole budget, so the phases behind it never run at all and their
/// receipts degrade to an anonymous `shutdown_coordinator` timeout. These
/// caps bound each phase *individually*; they do not shorten the total.
/// Unspent budget still flows forward, because each phase deadline is
/// recomputed from the clock at that phase's start (see
/// `DaemonShutdownBudget`), so a phase that drains quickly donates the
/// remainder to the phases after it.
///
/// The store-close reserve is the load-bearing one: closing the graph
/// runtimes is the only phase whose completion is a *durability* obligation,
/// so it is guaranteed a slice that no earlier phase can consume.
pub const DAEMON_BACKGROUND_DRAIN_DEADLINE: Duration = Duration::from_secs(15);
pub const DAEMON_PROJECT_SERVER_DRAIN_DEADLINE: Duration = Duration::from_secs(12);
pub const DAEMON_STORE_CLOSE_RESERVE: Duration = Duration::from_secs(12);

#[derive(Clone)]
pub struct DaemonLifecycle {
    inner: Arc<DaemonLifecycleInner>,
}

struct DaemonLifecycleInner {
    draining: AtomicBool,
    active: AtomicUsize,
    idle: tokio::sync::Notify,
    draining_notify: tokio::sync::Notify,
    shutdown: std::sync::Mutex<DaemonShutdownCoordinator>,
}

pub struct DaemonActivity {
    inner: Arc<DaemonLifecycleInner>,
}

#[derive(Default)]
struct DaemonShutdownCoordinator {
    in_flight: Option<Arc<DaemonShutdownAttempt>>,
    coordinator_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    coordinator_completed: Arc<tokio::sync::Notify>,
    terminal: Option<Arc<DaemonShutdownReceipt>>,
    failures: DaemonShutdownFailures,
}

struct DaemonShutdownCoordinatorCompletion(Arc<tokio::sync::Notify>);

impl Drop for DaemonShutdownCoordinatorCompletion {
    fn drop(&mut self) {
        self.0.notify_waiters();
    }
}

pub struct DaemonShutdownAttempt {
    receipt: tokio::sync::watch::Sender<Option<Arc<DaemonShutdownReceipt>>>,
}

pub enum DaemonShutdownClaim {
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
    pub fn accepting(&self) -> bool {
        !self.inner.draining.load(Ordering::Acquire)
    }

    pub fn try_enter(&self) -> Option<DaemonActivity> {
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

    pub fn begin_draining(&self) {
        if !self.inner.draining.swap(true, Ordering::AcqRel) {
            self.inner.draining_notify.notify_waiters();
        }
    }

    #[hotpath::measure(label = "daemon.engine.lifecycle.wait_draining", future = true)]
    pub async fn wait_for_draining(&self) {
        loop {
            let notified = self.inner.draining_notify.notified();
            if !self.accepting() {
                return;
            }
            notified.await;
        }
    }

    #[hotpath::measure(label = "daemon.engine.lifecycle.wait_idle", future = true)]
    pub async fn wait_for_idle(&self) {
        loop {
            let notified = self.inner.idle.notified();
            if self.inner.active.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }

    pub fn claim_shutdown_coordination(&self) -> DaemonShutdownClaim {
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

    /// Retain the coordinator independently of any caller that is merely
    /// waiting for its receipt. The task is joined after it publishes that
    /// receipt, so cancelling a first waiter cannot detach shutdown work.
    pub fn spawn_shutdown_coordinator<Task>(
        &self,
        attempt: &Arc<DaemonShutdownAttempt>,
        task: Task,
    ) -> bool
    where
        Task: std::future::Future<Output = ()> + Send + 'static,
    {
        let coordinator_task = {
            let shutdown = self
                .inner
                .shutdown
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Arc::clone(&shutdown.coordinator_task)
        };
        let mut coordinator_task = match coordinator_task.try_lock() {
            Ok(task) => task,
            Err(_) => return false,
        };
        let shutdown = self
            .inner
            .shutdown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !shutdown
            .in_flight
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, attempt))
            || coordinator_task.is_some()
        {
            return false;
        }
        let completed = Arc::clone(&shutdown.coordinator_completed);
        drop(shutdown);
        *coordinator_task = Some(tokio::spawn(hotpath::future!(
            async move {
                let _completion = DaemonShutdownCoordinatorCompletion(completed);
                task.await;
            },
            label = "daemon.engine.shutdown.coordinator"
        )));
        true
    }

    /// A receipt is sent immediately before the coordinator returns. Await
    /// task completion while leaving its handle in lifecycle ownership, so a
    /// cancelled waiter cannot detach the final coordinator exit.
    #[hotpath::measure(label = "daemon.engine.shutdown.coordinator.wait", future = true)]
    pub async fn wait_for_finished_shutdown_coordinator(&self) {
        loop {
            let completed = {
                let shutdown = self
                    .inner
                    .shutdown
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                Arc::clone(&shutdown.coordinator_completed)
            };
            let notified = completed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let coordinator_task = {
                let shutdown = self
                    .inner
                    .shutdown
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                Arc::clone(&shutdown.coordinator_task)
            };
            let finished = coordinator_task
                .lock()
                .await
                .as_ref()
                .is_none_or(tokio::task::JoinHandle::is_finished);
            if finished {
                return;
            }
            // The completion guard notifies from inside the coordinator task,
            // one scheduler step before Tokio marks its JoinHandle finished.
            // Recheck periodically so consuming that notification in the
            // intervening window cannot wait forever for a second one.
            tokio::select! {
                () = notified => {}
                () = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
        }
    }

    /// Reap only an already-finished coordinator. Its join cannot suspend,
    /// which keeps cancellation from taking ownership out of lifecycle state.
    #[hotpath::measure(label = "daemon.engine.shutdown.coordinator.join", future = true)]
    pub async fn join_finished_shutdown_coordinator(&self) {
        let coordinator_task = {
            let shutdown = self
                .inner
                .shutdown
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Arc::clone(&shutdown.coordinator_task)
        };
        let task = {
            let mut coordinator_task = coordinator_task.lock().await;
            if coordinator_task
                .as_ref()
                .is_some_and(tokio::task::JoinHandle::is_finished)
            {
                coordinator_task.take()
            } else {
                None
            }
        };
        if let Some(task) = task
            && let Err(error) = task.await
        {
            tracing::error!(%error, "daemon shutdown coordinator task failed after receipt");
        }
    }

    pub fn finish_shutdown_attempt(
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
    #[hotpath::measure(label = "daemon.engine.shutdown.wait_receipt", future = true)]
    pub async fn wait_for_receipt(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn coordinator_completion_notification_cannot_strand_its_waiter() {
        let lifecycle = DaemonLifecycle::default();
        let allow_completion = Arc::new(tokio::sync::Notify::new());
        let (coordinator_task, completed) = {
            let shutdown = lifecycle
                .inner
                .shutdown
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                Arc::clone(&shutdown.coordinator_task),
                Arc::clone(&shutdown.coordinator_completed),
            )
        };
        let task = tokio::spawn({
            let allow_completion = Arc::clone(&allow_completion);
            async move {
                allow_completion.notified().await;
                completed.notify_waiters();
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
        let mut retained_task = coordinator_task.lock().await;
        *retained_task = Some(task);
        let waiter = tokio::spawn({
            let lifecycle = lifecycle.clone();
            async move {
                lifecycle.wait_for_finished_shutdown_coordinator().await;
            }
        });

        allow_completion.notify_one();
        drop(retained_task);
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("coordinator completion notification must not be lost")
            .expect("coordinator waiter remains joinable");
        lifecycle.join_finished_shutdown_coordinator().await;
        assert!(coordinator_task.lock().await.is_none());
    }
}
