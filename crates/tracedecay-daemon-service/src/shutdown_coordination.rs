//! Cancellation-safe daemon shutdown coordination.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_runtime_core::native_runtime::{self, NativeTask, NativeTaskOutcome};
use tracedecay_store_runtime::ShutdownStatus;

pub struct ShutdownCoordinatorV1 {
    state: Arc<ShutdownCoordinatorState>,
}

#[derive(Default)]
struct ShutdownCoordinatorState {
    running: AtomicBool,
    terminal: std::sync::Mutex<Option<ShutdownStatus>>,
    coordinator_task: tokio::sync::Mutex<Option<NativeTask<()>>>,
    changed: tokio::sync::Notify,
}

struct ShutdownCoordinatorCompletion(Arc<ShutdownCoordinatorState>);

impl Drop for ShutdownCoordinatorCompletion {
    fn drop(&mut self) {
        self.0.changed.notify_waiters();
    }
}

impl Default for ShutdownCoordinatorV1 {
    fn default() -> Self {
        Self {
            state: Arc::new(ShutdownCoordinatorState::default()),
        }
    }
}

impl ShutdownCoordinatorV1 {
    pub async fn coordinate_until<Work>(
        &self,
        deadline: impl Into<std::time::Instant>,
        work: Work,
    ) -> ShutdownStatus
    where
        Work: std::future::Future<Output = ShutdownStatus> + Send + 'static,
    {
        let deadline = deadline.into();
        let mut work = Some(work);
        loop {
            self.reap_completed_coordinator().await;
            if !self.state.running.load(Ordering::Acquire) {
                self.join_coordinator().await;
            }
            if let Some(status) = self.terminal_status() {
                self.join_coordinator().await;
                return status;
            }

            let mut coordinator_task = self.state.coordinator_task.lock().await;
            if coordinator_task.is_some() {
                let running = self.state.running.load(Ordering::Acquire);
                drop(coordinator_task);
                if running {
                    return self.wait_for_terminal_status_until(deadline).await;
                }
                self.join_coordinator().await;
                continue;
            }
            if self
                .state
                .running
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                drop(coordinator_task);
                return self.wait_for_terminal_status_until(deadline).await;
            }
            if let Some(status) = self.terminal_status() {
                self.state.running.store(false, Ordering::Release);
                drop(coordinator_task);
                self.join_coordinator().await;
                return status;
            }

            // The MCP server's shutdown work still joins Tokio-owned tasks, so
            // it runs on Tokio while the coordinator that owns it does not.
            let started = work
                .take()
                .ok_or_else(|| "MCP shutdown coordinator lost its work future".to_owned())
                .and_then(|work| {
                    let runtime =
                        native_runtime::tokio_handle("daemon-service.shutdown_coordination.runner")
                            .ok_or_else(|| "MCP shutdown work found no Tokio runtime".to_owned())?;
                    let state = Arc::clone(&self.state);
                    NativeTask::spawn(async move {
                        let _completion = ShutdownCoordinatorCompletion(Arc::clone(&state));
                        let status = match runtime.spawn(work).await {
                            Ok(status) => status,
                            Err(error) => ShutdownStatus::Failed(error.to_string()),
                        };
                        state.finish(status);
                    })
                    .map_err(|error| format!("MCP shutdown coordinator was not started: {error}"))
                });
            match started {
                Ok(task) => *coordinator_task = Some(task),
                Err(error) => {
                    self.state.finish(ShutdownStatus::Failed(error.clone()));
                    return ShutdownStatus::Failed(error);
                }
            }
            drop(coordinator_task);
            return self.wait_for_terminal_status_until(deadline).await;
        }
    }

    /// Reaps a coordinator task that has already completed, without waiting for
    /// one that has not.
    ///
    /// Callers reach this while still inside their own deadline and fall through
    /// to a bounded wait, so a running coordinator must not block them.
    async fn reap_completed_coordinator(&self) {
        let result = {
            let mut coordinator_task = self.state.coordinator_task.lock().await;
            let Some(result) = coordinator_task.as_ref().and_then(NativeTask::try_finished) else {
                return;
            };
            coordinator_task.take();
            result
        };
        self.observe_coordinator_result(result);
    }

    /// Waits for the coordinator task to finish, then reaps it.
    ///
    /// This awaits the task rather than re-checking `is_finished()` behind
    /// `changed`, because no notification ever follows the task becoming
    /// finished. Both notifications a coordinator sends, `finish` and
    /// `ShutdownCoordinatorCompletion::drop`, run while its future is still
    /// being polled, and the runtime marks the task complete only after that
    /// poll returns. A waiter that registered on `changed` after the last
    /// notification and observed `is_finished() == false` therefore had nothing
    /// left to wake it, and this wait has no deadline of its own to escape on.
    ///
    /// Every caller reaches this only once the coordinator has published a
    /// terminal receipt or cleared `running`, so the work is already complete
    /// and holding the handle's lock across the join costs only task teardown.
    async fn join_coordinator(&self) {
        let result = {
            let mut coordinator_task = self.state.coordinator_task.lock().await;
            let Some(task) = coordinator_task.as_ref() else {
                return;
            };
            let result = task.wait().await;
            coordinator_task.take();
            result
        };
        self.observe_coordinator_result(result);
    }

    fn observe_coordinator_result(&self, outcome: NativeTaskOutcome<()>) {
        if !matches!(outcome, NativeTaskOutcome::Completed(())) {
            tracing::error!(
                ?outcome,
                "MCP shutdown coordinator task failed after receipt"
            );
            self.state
                .finish(ShutdownStatus::Failed(format!("{outcome:?}")));
        }
    }

    fn terminal_status(&self) -> Option<ShutdownStatus> {
        self.state
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    async fn wait_for_terminal_status_until(&self, deadline: std::time::Instant) -> ShutdownStatus {
        loop {
            if let Some(status) = self.terminal_status() {
                self.join_coordinator().await;
                return status;
            }
            if !self.state.running.load(Ordering::Acquire) {
                self.join_coordinator().await;
                return ShutdownStatus::TimedOut;
            }
            let notified = self.state.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(status) = self.terminal_status() {
                self.join_coordinator().await;
                return status;
            }
            if !self.state.running.load(Ordering::Acquire) {
                self.join_coordinator().await;
                return ShutdownStatus::TimedOut;
            }
            // An unavailable timer reads as elapsed.
            if tracedecay_runtime_core::native_runtime::timeout_at(deadline, notified)
                .await
                .is_err()
            {
                return ShutdownStatus::TimedOut;
            }
        }
    }
}

impl ShutdownCoordinatorState {
    fn finish(&self, status: ShutdownStatus) {
        if status != ShutdownStatus::TimedOut {
            *self
                .terminal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(status);
        }
        self.running.store(false, Ordering::Release);
        self.changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// The bounded wait times out on the native clock, so a waiter driven by
    /// no Tokio runtime still returns at its deadline, and not before it.
    #[test]
    fn a_running_coordinator_wait_times_out_without_a_tokio_runtime() {
        let coordinator = ShutdownCoordinatorV1::default();
        coordinator.state.running.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_millis(50);

        let status = native_runtime::block_on(native_runtime::timeout(
            Duration::from_secs(10),
            coordinator.wait_for_terminal_status_until(deadline),
        ))
        .expect("native runtime");

        assert_eq!(status, Ok(ShutdownStatus::TimedOut));
        assert!(
            Instant::now() >= deadline,
            "the wait ended before its deadline"
        );
    }

    /// The coordinator runs on the native runtime; the MCP shutdown work it
    /// owns still needs Tokio, so a caller without one gets a typed failure.
    #[test]
    fn coordinating_without_a_tokio_runtime_fails_with_a_typed_status() {
        const SITE: &str = "daemon-service.shutdown_coordination.runner";
        let coordinator = ShutdownCoordinatorV1::default();
        let missing = native_runtime::tokio_runtime_missing_count(SITE);
        let status = native_runtime::block_on(async {
            assert!(tokio::runtime::Handle::try_current().is_err());
            coordinator
                .coordinate_until(Instant::now() + Duration::from_secs(5), async {
                    ShutdownStatus::Clean
                })
                .await
        })
        .expect("native runtime");

        assert_eq!(
            status,
            ShutdownStatus::Failed("MCP shutdown work found no Tokio runtime".to_owned())
        );
        assert_eq!(
            native_runtime::tokio_runtime_missing_count(SITE) - missing,
            1
        );
    }

    /// The ordering that stalled the daemon's RMCP shutdown for a full test
    /// bound: a waiter begins waiting for the coordinator task after the
    /// coordinator's last notification has already fired, and the task is only
    /// marked complete afterwards.
    ///
    /// A real coordinator always passes through this state, because both of its
    /// notifications run while its future is still being polled and the runtime
    /// marks the task complete only once that poll returns. Installing the task
    /// directly makes the window a fact instead of a race: the waiter is polled
    /// once and observes an unfinished task before `release` lets it finish.
    /// Waiting on `changed` for a finished task sleeps forever here; awaiting
    /// the task cannot.
    #[test]
    fn a_coordinator_that_sends_no_further_notification_is_still_joined() {
        let coordinator = ShutdownCoordinatorV1::default();
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        native_runtime::block_on(async {
            *coordinator.state.coordinator_task.lock().await = Some(
                NativeTask::spawn(async move {
                    let _ = released.await;
                })
                .expect("native coordinator task"),
            );
            coordinator.state.finish(ShutdownStatus::Clean);

            let mut join = std::pin::pin!(coordinator.join_coordinator());
            std::future::poll_fn(|cx| {
                assert!(
                    join.as_mut().poll(cx).is_pending(),
                    "the coordinator task is still running"
                );
                std::task::Poll::Ready(())
            })
            .await;
            release.send(()).expect("release the coordinator task");
            native_runtime::timeout(Duration::from_secs(30), join)
                .await
                .expect("a coordinator that sends no further notification must still be joined");
            assert!(
                coordinator.state.coordinator_task.lock().await.is_none(),
                "the joined coordinator task must be reaped"
            );
        })
        .expect("native runtime");
    }
}
