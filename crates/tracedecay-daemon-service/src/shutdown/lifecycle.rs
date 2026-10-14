//! Daemon lifecycle tracking: drain/idle coordination for graceful shutdown.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use std::time::Duration;

use tracedecay_runtime_core::native_runtime::{NativeTask, NativeTaskOutcome};

pub use tracedecay_runtime_core::DAEMON_TASK_ABORT_DEADLINE;
pub use tracedecay_session_runtime::DAEMON_CLIENT_DRAIN_DEADLINE;

use super::orchestration::{DaemonShutdownFailures, DaemonShutdownReceipt};

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
    coordinator_task: Arc<tokio::sync::Mutex<Option<ShutdownCoordinatorTask>>>,
    coordinator_completed: Arc<tokio::sync::Notify>,
    terminal: Option<Arc<DaemonShutdownReceipt>>,
    failures: DaemonShutdownFailures,
}

struct ShutdownCoordinatorTask {
    handle: NativeTask<()>,
    exited: Arc<AtomicBool>,
}

/// Moved into the coordinator task, so it drops when the task body returns,
/// unwinds, or is dropped unpolled. The exit flag is set before the
/// notification, so a waiter that misses the notification still reads it.
struct DaemonShutdownCoordinatorCompletion {
    exited: Arc<AtomicBool>,
    completed: Arc<tokio::sync::Notify>,
}

impl Drop for DaemonShutdownCoordinatorCompletion {
    fn drop(&mut self) {
        self.exited.store(true, Ordering::Release);
        self.completed.notify_waiters();
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

    #[tracing::instrument(
        name = "daemon.engine.lifecycle.wait_draining",
        level = "trace",
        skip_all
    )]
    pub async fn wait_for_draining(&self) {
        loop {
            let notified = self.inner.draining_notify.notified();
            if !self.accepting() {
                return;
            }
            notified.await;
        }
    }

    #[tracing::instrument(name = "daemon.engine.lifecycle.wait_idle", level = "trace", skip_all)]
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
    /// The error says why no coordinator now owns `attempt`.
    pub fn spawn_shutdown_coordinator<Task>(
        &self,
        attempt: &Arc<DaemonShutdownAttempt>,
        task: Task,
    ) -> Result<(), String>
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
        let lost = || "daemon shutdown coordinator ownership was lost".to_owned();
        let mut coordinator_task = coordinator_task.try_lock().map_err(|_| lost())?;
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
            return Err(lost());
        }
        let exited = Arc::new(AtomicBool::new(false));
        let completion = DaemonShutdownCoordinatorCompletion {
            exited: Arc::clone(&exited),
            completed: Arc::clone(&shutdown.coordinator_completed),
        };
        drop(shutdown);
        let handle = NativeTask::spawn(tracing::Instrument::instrument(
            async move {
                let _completion = completion;
                task.await;
            },
            tracing::trace_span!("daemon.engine.shutdown.coordinator"),
        ))
        .map_err(|error| format!("daemon shutdown coordinator was not started: {error}"))?;
        *coordinator_task = Some(ShutdownCoordinatorTask { handle, exited });
        Ok(())
    }

    /// A receipt is sent immediately before the coordinator returns. Await
    /// the coordinator body's exit while leaving its handle in lifecycle
    /// ownership, so a cancelled waiter cannot detach the final coordinator
    /// exit.
    #[tracing::instrument(
        name = "daemon.engine.shutdown.coordinator.wait",
        level = "trace",
        skip_all
    )]
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
            if coordinator_task
                .lock()
                .await
                .as_ref()
                .is_none_or(ShutdownCoordinatorTask::exited)
            {
                return;
            }
            notified.await;
        }
    }

    /// Reap only a coordinator whose body has exited. Its join waits only for
    /// the runtime to record that exit, so cancellation cannot take a live
    /// coordinator out of lifecycle state.
    #[tracing::instrument(
        name = "daemon.engine.shutdown.coordinator.join",
        level = "trace",
        skip_all
    )]
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
                .is_some_and(ShutdownCoordinatorTask::exited)
            {
                coordinator_task.take()
            } else {
                None
            }
        };
        if let Some(task) = task {
            let outcome = task.handle.wait().await;
            if !matches!(outcome, NativeTaskOutcome::Completed(())) {
                tracing::error!(
                    ?outcome,
                    "daemon shutdown coordinator task failed after receipt"
                );
            }
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

impl ShutdownCoordinatorTask {
    fn exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }
}

impl DaemonShutdownAttempt {
    #[tracing::instrument(
        name = "daemon.engine.shutdown.wait_receipt",
        level = "trace",
        skip_all
    )]
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

    #[tokio::test]
    async fn the_shutdown_coordinator_runs_off_tokio() {
        let lifecycle = DaemonLifecycle::default();
        let DaemonShutdownClaim::Run { attempt, .. } = lifecycle.claim_shutdown_coordination()
        else {
            panic!("the first claim runs the shutdown");
        };
        let (on_tokio, observed) = tokio::sync::oneshot::channel();
        lifecycle
            .spawn_shutdown_coordinator(&attempt, async move {
                let _ = on_tokio.send(tokio::runtime::Handle::try_current().is_ok());
            })
            .expect("the claimed attempt starts its coordinator");

        assert!(
            !observed.await.expect("the coordinator ran"),
            "the coordinator task runs on the native runtime"
        );
        lifecycle.wait_for_finished_shutdown_coordinator().await;
        lifecycle.join_finished_shutdown_coordinator().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn coordinator_completion_notification_cannot_strand_its_waiter() {
        let lifecycle = DaemonLifecycle::default();
        let DaemonShutdownClaim::Run { attempt, .. } = lifecycle.claim_shutdown_coordination()
        else {
            panic!("the first claim runs the shutdown");
        };
        let release = Arc::new(tokio::sync::Notify::new());
        let coordinator_release = Arc::clone(&release);
        lifecycle
            .spawn_shutdown_coordinator(&attempt, async move {
                coordinator_release.notified().await;
            })
            .expect("the claimed attempt starts its coordinator");
        let waiter = tokio::spawn({
            let lifecycle = lifecycle.clone();
            async move {
                lifecycle.wait_for_finished_shutdown_coordinator().await;
            }
        });

        release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("coordinator completion notification must not be lost")
            .expect("coordinator waiter remains joinable");
        lifecycle.join_finished_shutdown_coordinator().await;
        let coordinator_task = {
            let shutdown = lifecycle
                .inner
                .shutdown
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Arc::clone(&shutdown.coordinator_task)
        };
        assert!(
            coordinator_task.lock().await.is_none(),
            "an exited coordinator is reaped"
        );
    }
}
