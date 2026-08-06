//! Resumable MCP shutdown ownership and persistence.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::time::Duration;

use super::*;

pub(in crate::mcp::server) struct McpShutdownCompletion {
    state: Arc<McpShutdownState>,
}

#[derive(Default)]
struct McpShutdownState {
    running: AtomicBool,
    done: AtomicBool,
    terminal: std::sync::Mutex<Option<crate::daemon::ShutdownStatus>>,
    coordinator_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    changed: tokio::sync::Notify,
}

struct McpShutdownCoordinatorCompletion(Arc<McpShutdownState>);

impl Drop for McpShutdownCoordinatorCompletion {
    fn drop(&mut self) {
        self.0.changed.notify_waiters();
    }
}

impl Default for McpShutdownCompletion {
    fn default() -> Self {
        Self {
            state: Arc::new(McpShutdownState::default()),
        }
    }
}

impl McpShutdownCompletion {
    pub(super) async fn coordinate_until<Work>(
        &self,
        deadline: tokio::time::Instant,
        work: Work,
    ) -> crate::daemon::ShutdownStatus
    where
        Work: Future<Output = crate::daemon::ShutdownStatus> + Send + 'static,
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
                self.state.finish(crate::daemon::ShutdownStatus::Failed(
                    "MCP shutdown coordinator lost its work future".to_owned(),
                ));
                drop(coordinator_task);
                return crate::daemon::ShutdownStatus::Failed(
                    "MCP shutdown coordinator lost its work future".to_owned(),
                );
            };
            let state = Arc::clone(&self.state);
            let task = tokio::spawn(async move {
                let _completion = McpShutdownCoordinatorCompletion(Arc::clone(&state));
                let runner = tokio::spawn(work);
                let status = match runner.await {
                    Ok(status) => status,
                    Err(error) => crate::daemon::ShutdownStatus::Failed(error.to_string()),
                };
                state.finish(status);
            });
            *coordinator_task = Some(task);
            drop(coordinator_task);
            return self.wait_for_terminal_status_until(deadline).await;
        }
    }

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
            self.state
                .finish(crate::daemon::ShutdownStatus::Failed(error.to_string()));
        }
    }

    async fn wait_for_finished_coordinator(&self) {
        loop {
            let notified = self.state.changed.notified();
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
            notified.await;
        }
    }

    fn terminal_status(&self) -> Option<crate::daemon::ShutdownStatus> {
        self.state
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    async fn wait_for_terminal_status_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> crate::daemon::ShutdownStatus {
        loop {
            if let Some(status) = self.terminal_status() {
                self.wait_for_finished_coordinator().await;
                self.join_finished_coordinator().await;
                return status;
            }
            if !self.state.running.load(Ordering::Acquire) {
                self.wait_for_finished_coordinator().await;
                self.join_finished_coordinator().await;
                return crate::daemon::ShutdownStatus::TimedOut;
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
                return crate::daemon::ShutdownStatus::TimedOut;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return crate::daemon::ShutdownStatus::TimedOut;
            }
        }
    }

    #[cfg(test)]
    fn is_done(&self) -> bool {
        self.state.done.load(Ordering::Acquire)
    }
}

impl McpShutdownState {
    fn finish(&self, status: crate::daemon::ShutdownStatus) {
        if status != crate::daemon::ShutdownStatus::TimedOut {
            *self
                .terminal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(status);
            self.done.store(true, Ordering::Release);
        }
        self.running.store(false, Ordering::Release);
        self.changed.notify_waiters();
    }
}

impl McpServer {
    pub(crate) async fn shutdown_if(self: &Arc<Self>, enabled: bool) {
        if enabled {
            self.shutdown().await;
        }
    }

    /// Persists the tokens-saved counter, flushes pending tokens to the
    /// worldwide counter, checkpoints the WAL, and logs a session summary.
    ///
    /// Idempotent — safe to call multiple times. `run` invokes it once when
    /// its main loop exits; callers (e.g. `main.rs`, tests) may invoke it
    /// explicitly afterwards without re-running the persistence logic.
    pub async fn shutdown(self: &Arc<Self>) {
        let deadline = tokio::time::Instant::now() + crate::daemon::DAEMON_SHUTDOWN_DEADLINE;
        let status = self.shutdown_until(deadline).await;
        if !status.is_clean() {
            tracing::warn!(?status, "MCP server shutdown did not complete cleanly");
        }
    }

    pub(crate) async fn shutdown_until(
        self: &Arc<Self>,
        deadline: tokio::time::Instant,
    ) -> crate::daemon::ShutdownStatus {
        self.shutdown
            .coordinate_until(deadline, Arc::clone(self).run_shutdown(deadline))
            .await
    }

    async fn run_shutdown(
        self: Arc<Self>,
        deadline: tokio::time::Instant,
    ) -> crate::daemon::ShutdownStatus {
        let mut failures = self.shutdown_background_tasks_until(deadline).await;

        let uptime = self.stats.started_at.elapsed();
        let tool_calls = self.stats.tool_calls.load(Ordering::Relaxed);
        let tokens_saved = self.tokens_saved.load(Ordering::Relaxed);

        let cg = self.cg_snapshot().await;
        // Persist final tokens-saved value
        if let Err(e) = cg.set_tokens_saved(tokens_saved).await {
            tracing::warn!(error = %e, "failed to persist tokens saved during shutdown");
            failures.push(format!("persist tokens saved: {e}"));
        }

        if let Some(ref gdb) = self.accounting_db {
            gdb.upsert(cg.project_root(), tokens_saved).await;
            gdb.checkpoint().await;
        } else if let Some(ref gdb) = self.global_db {
            gdb.upsert(cg.project_root(), tokens_saved).await;
            gdb.checkpoint().await;
        }

        // Flush remaining delta to worldwide counter (what periodic flushes missed)
        let last_flushed = self.last_flushed_tokens.load(Ordering::Relaxed);
        if (self.accounting_db.is_some() || self.global_db.is_some()) && tokens_saved > last_flushed
        {
            let delta = tokens_saved - last_flushed;
            let mut config = crate::user_config::UserConfig::load();
            config.pending_upload += delta;
            if config.upload_enabled
                && let Some(_total) = crate::cloud::flush_pending(config.pending_upload)
            {
                config.pending_upload = 0;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                config.last_upload_at = now;
            }
            if let Err(err) = config.save() {
                tracing::warn!(error = %err, "could not save upload config during shutdown");
            }
        }

        // Checkpoint WAL to merge it into the main database file
        if let Err(e) = cg.checkpoint().await {
            tracing::warn!(error = %e, "failed to checkpoint WAL during shutdown");
            failures.push(format!("code graph checkpoint: {e}"));
        }

        if failures.is_empty() {
            tracing::info!(
                tool_calls,
                tokens_saved,
                uptime_secs = uptime.as_secs(),
                "MCP server shutdown complete"
            );
            crate::daemon::ShutdownStatus::Clean
        } else {
            crate::daemon::ShutdownStatus::Failed(failures.join("; "))
        }
    }

    pub(crate) async fn shutdown_background_tasks(&self) -> Vec<String> {
        self.shutdown_background_tasks_until(
            tokio::time::Instant::now() + crate::daemon::DAEMON_SHUTDOWN_DEADLINE,
        )
        .await
    }

    async fn shutdown_background_tasks_until(&self, deadline: tokio::time::Instant) -> Vec<String> {
        let mut failures = Vec::new();
        if let Err(error) =
            crate::mcp::tools::handlers::dashboard::shutdown_dashboard_until(deadline).await
        {
            failures.push(format!("dashboard shutdown: {error}"));
        }
        failures.extend(self.background_tasks.shutdown().await);
        if let Some(worker) = self.project_host_admission_replay.lock().await.take() {
            worker.shutdown().await;
        }
        // Same ordering as before the state machine landed: the index-sync
        // task is aborted and joined first, then the ingest is cancelled,
        // joined, and the machine marked cancelled.
        self.shutdown_startup_catch_up_sync().await;
        self.shutdown_startup_transcript_ingest().await;
        failures
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    struct RetainedShutdownOwner(Arc<AtomicBool>);

    impl Drop for RetainedShutdownOwner {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[tokio::test]
    async fn cancelled_shutdown_waiter_does_not_cancel_the_owned_work() {
        let completion = Arc::new(McpShutdownCompletion::default());
        let attempts = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(tokio::sync::Notify::new());
        let (release, released) = tokio::sync::oneshot::channel();

        let first_completion = Arc::clone(&completion);
        let first_attempts = Arc::clone(&attempts);
        let first_entered = Arc::clone(&entered);
        let first = tokio::spawn(async move {
            first_completion
                .coordinate_until(
                    tokio::time::Instant::now() + Duration::from_secs(5),
                    async move {
                        first_attempts.fetch_add(1, Ordering::AcqRel);
                        first_entered.notify_one();
                        let _ = released.await;
                        crate::daemon::ShutdownStatus::Clean
                    },
                )
                .await
        });
        entered.notified().await;
        first.abort();
        assert!(
            first
                .await
                .expect_err("cancel first shutdown waiter")
                .is_cancelled()
        );
        assert!(!completion.is_done());

        release.send(()).expect("release retained shutdown work");
        let retry_attempts = Arc::clone(&attempts);
        let retry = completion
            .coordinate_until(
                tokio::time::Instant::now() + Duration::from_secs(1),
                async move {
                    retry_attempts.fetch_add(1, Ordering::AcqRel);
                    panic!("retry must await the retained shutdown work");
                },
            )
            .await;

        assert_eq!(retry, crate::daemon::ShutdownStatus::Clean);
        assert_eq!(attempts.load(Ordering::Acquire), 1);
        assert!(completion.is_done());
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_shutdown_retains_owned_work_until_a_retry_observes_its_terminal_status() {
        let completion = Arc::new(McpShutdownCompletion::default());
        let attempts = Arc::new(AtomicUsize::new(0));
        let owner_dropped = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(tokio::sync::Notify::new());
        let (release, released) = tokio::sync::oneshot::channel();

        let first_completion = Arc::clone(&completion);
        let first_attempts = Arc::clone(&attempts);
        let first_entered = Arc::clone(&entered);
        let first_owner_dropped = Arc::clone(&owner_dropped);
        let first = tokio::spawn(async move {
            first_completion
                .coordinate_until(
                    tokio::time::Instant::now() + Duration::from_secs(1),
                    async move {
                        let _owner = RetainedShutdownOwner(first_owner_dropped);
                        first_attempts.fetch_add(1, Ordering::AcqRel);
                        first_entered.notify_one();
                        let _ = released.await;
                        crate::daemon::ShutdownStatus::Clean
                    },
                )
                .await
        });
        entered.notified().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(
            first.await.expect("first timed-out shutdown"),
            crate::daemon::ShutdownStatus::TimedOut
        );
        assert!(
            !owner_dropped.load(Ordering::Acquire),
            "the timed-out attempt must retain its owner for a retry"
        );

        let retry_completion = Arc::clone(&completion);
        let retry_attempts = Arc::clone(&attempts);
        let retry = tokio::spawn(async move {
            retry_completion
                .coordinate_until(
                    tokio::time::Instant::now() + Duration::from_secs(1),
                    async move {
                        retry_attempts.fetch_add(1, Ordering::AcqRel);
                        panic!("retry must await the retained shutdown owner");
                    },
                )
                .await
        });
        release.send(()).expect("release retained shutdown owner");

        assert_eq!(
            retry.await.expect("retry shutdown"),
            crate::daemon::ShutdownStatus::Clean
        );
        let duplicate_attempts = Arc::clone(&attempts);
        let duplicate = completion
            .coordinate_until(
                tokio::time::Instant::now() + Duration::from_secs(1),
                async move {
                    duplicate_attempts.fetch_add(1, Ordering::AcqRel);
                    panic!("terminal shutdown status must be retained");
                },
            )
            .await;

        assert_eq!(duplicate, crate::daemon::ShutdownStatus::Clean);
        assert_eq!(attempts.load(Ordering::Acquire), 1);
        assert!(owner_dropped.load(Ordering::Acquire));
        assert!(completion.is_done());
    }
}
