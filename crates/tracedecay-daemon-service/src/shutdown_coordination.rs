//! Cancellation-safe daemon shutdown coordination.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_store_runtime::ShutdownStatus;

pub struct ShutdownCoordinatorV1 {
    state: Arc<ShutdownCoordinatorState>,
}

#[derive(Default)]
struct ShutdownCoordinatorState {
    running: AtomicBool,
    terminal: std::sync::Mutex<Option<ShutdownStatus>>,
    coordinator_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
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
    #[hotpath::skip]
    pub async fn coordinate_until<Work>(
        &self,
        deadline: tokio::time::Instant,
        work: Work,
    ) -> ShutdownStatus
    where
        Work: std::future::Future<Output = ShutdownStatus> + Send + 'static,
    {
        let mut work = Some(work);
        loop {
            self.join_finished_coordinator().await;
            if !self.state.running.load(Ordering::Acquire) {
                self.wait_for_finished_coordinator().await;
                self.join_finished_coordinator().await;
            }
            if let Some(status) = self.terminal_status() {
                self.wait_for_finished_coordinator().await;
                self.join_finished_coordinator().await;
                return status;
            }

            let mut coordinator_task = self.state.coordinator_task.lock().await;
            if coordinator_task.is_some() {
                let running = self.state.running.load(Ordering::Acquire);
                drop(coordinator_task);
                if running {
                    return self.wait_for_terminal_status_until(deadline).await;
                }
                self.wait_for_finished_coordinator().await;
                self.join_finished_coordinator().await;
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
                self.wait_for_finished_coordinator().await;
                self.join_finished_coordinator().await;
                return status;
            }

            let Some(work) = work.take() else {
                self.state.finish(ShutdownStatus::Failed(
                    "MCP shutdown coordinator lost its work future".to_owned(),
                ));
                drop(coordinator_task);
                return ShutdownStatus::Failed(
                    "MCP shutdown coordinator lost its work future".to_owned(),
                );
            };
            let state = Arc::clone(&self.state);
            let task = tokio::spawn(async move {
                let _completion = ShutdownCoordinatorCompletion(Arc::clone(&state));
                let runner = tokio::spawn(work);
                let status = match runner.await {
                    Ok(status) => status,
                    Err(error) => ShutdownStatus::Failed(error.to_string()),
                };
                state.finish(status);
            });
            *coordinator_task = Some(task);
            drop(coordinator_task);
            return self.wait_for_terminal_status_until(deadline).await;
        }
    }

    #[hotpath::skip]
    async fn join_finished_coordinator(&self) {
        let result = {
            let mut coordinator_task = self.state.coordinator_task.lock().await;
            let Some(task) = coordinator_task.as_mut() else {
                return;
            };
            if !task.is_finished() {
                return;
            }
            let result = task.await;
            coordinator_task.take();
            result
        };
        if let Err(error) = result {
            tracing::error!(%error, "MCP shutdown coordinator task failed after receipt");
            self.state.finish(ShutdownStatus::Failed(error.to_string()));
        }
    }

    #[hotpath::skip]
    async fn wait_for_finished_coordinator(&self) {
        loop {
            let notified = self.state.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let finished = self
                .state
                .coordinator_task
                .lock()
                .await
                .as_ref()
                .is_none_or(tokio::task::JoinHandle::is_finished);
            if finished {
                return;
            }
            notified.as_mut().await;
        }
    }

    fn terminal_status(&self) -> Option<ShutdownStatus> {
        self.state
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[hotpath::skip]
    async fn wait_for_terminal_status_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> ShutdownStatus {
        loop {
            if let Some(status) = self.terminal_status() {
                self.wait_for_finished_coordinator().await;
                self.join_finished_coordinator().await;
                return status;
            }
            if !self.state.running.load(Ordering::Acquire) {
                self.wait_for_finished_coordinator().await;
                self.join_finished_coordinator().await;
                return ShutdownStatus::TimedOut;
            }
            let notified = self.state.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(status) = self.terminal_status() {
                self.wait_for_finished_coordinator().await;
                self.join_finished_coordinator().await;
                return status;
            }
            if !self.state.running.load(Ordering::Acquire) {
                self.wait_for_finished_coordinator().await;
                self.join_finished_coordinator().await;
                return ShutdownStatus::TimedOut;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
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
