//! Cancellable daemon-owned project host-admission replay worker.
//!
//! Continues bounded [`McpServer::replay_host_admission`] passes while the
//! spool makes progress, applies retryable backoff, and is cancelled + joined
//! during project-server shutdown on every platform.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::task::JoinHandle;

use tracedecay_usecases::host_admission::{
    HostAdmissionOutcome, ReplayPassDecision, SharedHostAdmissionBroker, classify_replay_pass,
    replay_backoff,
};

const REPLAY_BACKOFF_SHIFT_CAP: u32 = 6;

type PassFn =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = HostAdmissionOutcome> + Send>> + Send + Sync>;

pub(super) struct ProjectHostAdmissionReplayTask {
    worker: Arc<ProjectHostAdmissionReplayWorker>,
    task: Option<JoinHandle<()>>,
}

pub(super) struct ProjectHostAdmissionReplayWorker {
    broker: SharedHostAdmissionBroker,
    pass: PassFn,
    cancel: AtomicBool,
    cancel_notify: Notify,
    dirty: AtomicBool,
    busy: AtomicBool,
    pass_count: AtomicUsize,
    backoff_count: AtomicUsize,
    idle: Notify,
}

impl ProjectHostAdmissionReplayTask {
    pub(super) fn start(broker: SharedHostAdmissionBroker, pass: PassFn) -> Self {
        let worker = Arc::new(ProjectHostAdmissionReplayWorker {
            broker,
            pass,
            cancel: AtomicBool::new(false),
            cancel_notify: Notify::new(),
            dirty: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            pass_count: AtomicUsize::new(0),
            backoff_count: AtomicUsize::new(0),
            idle: Notify::new(),
        });
        worker.kick();
        let task_worker = Arc::clone(&worker);
        let task = tokio::spawn(task_worker.run());
        Self {
            worker,
            task: Some(task),
        }
    }

    #[cfg(test)]
    pub(super) fn worker(&self) -> &Arc<ProjectHostAdmissionReplayWorker> {
        &self.worker
    }

    pub(super) async fn shutdown(mut self) {
        self.worker.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }

    #[cfg(test)]
    pub(super) fn pass_count(&self) -> usize {
        self.worker.pass_count()
    }

    #[cfg(test)]
    pub(super) fn backoff_count(&self) -> usize {
        self.worker.backoff_count()
    }
}

impl Drop for ProjectHostAdmissionReplayTask {
    fn drop(&mut self) {
        self.worker.cancel();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl ProjectHostAdmissionReplayWorker {
    fn kick(&self) {
        self.dirty.store(true, Ordering::Release);
        self.broker.request_replay();
    }

    fn cancel(&self) {
        if !self.cancel.swap(true, Ordering::AcqRel) {
            self.cancel_notify.notify_waiters();
            self.broker.request_replay();
        }
    }

    #[cfg(test)]
    pub(super) async fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.cancel.load(Ordering::Acquire) {
                return true;
            }
            if !self.busy.load(Ordering::Acquire)
                && !self.dirty.load(Ordering::Acquire)
                && !self.broker.has_pending_replay().await
            {
                return true;
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            tokio::select! {
                () = self.idle.notified() => {}
                () = self.cancel_notify.notified() => return true,
                () = tokio::time::sleep(remaining) => return false,
            }
        }
    }

    #[cfg(test)]
    pub(super) fn pass_count(&self) -> usize {
        self.pass_count.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(super) fn backoff_count(&self) -> usize {
        self.backoff_count.load(Ordering::Acquire)
    }

    async fn run(self: Arc<Self>) {
        let mut consecutive_retryable = 0u32;
        loop {
            if self.cancel.load(Ordering::Acquire) {
                break;
            }
            tokio::select! {
                () = self.cancel_notify.notified() => break,
                () = self.broker.wait_for_replay_request() => {}
            }
            if self.cancel.load(Ordering::Acquire) {
                break;
            }
            loop {
                if self.cancel.load(Ordering::Acquire) {
                    break;
                }
                if !self.dirty.swap(false, Ordering::AcqRel)
                    && !self.broker.has_pending_replay().await
                {
                    self.busy.store(false, Ordering::Release);
                    self.idle.notify_waiters();
                    break;
                }
                self.busy.store(true, Ordering::Release);
                self.pass_count.fetch_add(1, Ordering::AcqRel);
                let pending_before = self.broker.pending_replay_count().await.unwrap_or(0);
                let outcome = tokio::select! {
                    () = self.cancel_notify.notified() => break,
                    outcome = (self.pass)() => outcome,
                };
                let pending_after = self.broker.pending_replay_count().await.unwrap_or(0);
                if self.cancel.load(Ordering::Acquire) {
                    break;
                }
                match classify_replay_pass(pending_before, pending_after, &outcome) {
                    ReplayPassDecision::ProgressPending => {
                        consecutive_retryable = 0;
                        tokio::task::yield_now().await;
                    }
                    ReplayPassDecision::Backoff => {
                        consecutive_retryable = consecutive_retryable.saturating_add(1);
                        self.backoff_count.fetch_add(1, Ordering::AcqRel);
                        self.dirty.store(true, Ordering::Release);
                        let backoff = project_replay_backoff(consecutive_retryable);
                        tokio::select! {
                            () = self.cancel_notify.notified() => break,
                            () = tokio::time::sleep(backoff) => {}
                        }
                    }
                    ReplayPassDecision::Stop => {
                        consecutive_retryable = 0;
                        tracing::warn!(
                            reason_code =
                                outcome.reason_code.unwrap_or("host_admission_unavailable"),
                            "project host admission replay stopped"
                        );
                        break;
                    }
                    ReplayPassDecision::Requeue => {
                        consecutive_retryable = 0;
                        if self.dirty.load(Ordering::Acquire) || pending_after > 0 {
                            continue;
                        }
                        self.busy.store(false, Ordering::Release);
                        self.idle.notify_waiters();
                        break;
                    }
                }
            }
            self.busy.store(false, Ordering::Release);
            self.idle.notify_waiters();
        }
        self.busy.store(false, Ordering::Release);
        self.idle.notify_waiters();
    }
}

pub(super) fn project_replay_backoff(attempt: u32) -> Duration {
    replay_backoff(attempt, REPLAY_BACKOFF_SHIFT_CAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_replay_backoff_grows_then_caps() {
        assert_eq!(project_replay_backoff(1), Duration::from_millis(25));
        assert_eq!(project_replay_backoff(2), Duration::from_millis(50));
        assert_eq!(project_replay_backoff(3), Duration::from_millis(100));
        // shift caps at 6 → 25ms * 64 = 1.6s (below the 2s absolute ceiling)
        assert_eq!(project_replay_backoff(20), Duration::from_millis(1_600));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn non_retryable_pending_record_stops_until_an_external_kick() {
        let temp = tempfile::TempDir::new().unwrap();
        let (runtime, _) = tracedecay_usecases::host_admission::HostAdmissionRuntime::open(
            temp.path(),
            tracedecay_usecases::host_admission::SpoolBounds::default(),
        )
        .unwrap();
        let broker =
            Arc::new(tracedecay_usecases::host_admission::HostAdmissionBroker::new(runtime));
        broker.admit("test:pending", b"pending").await.unwrap();
        let passes = Arc::new(AtomicUsize::new(0));
        let passes_for_run = Arc::clone(&passes);
        let pass: PassFn = Arc::new(move || {
            let passes = Arc::clone(&passes_for_run);
            Box::pin(async move {
                passes.fetch_add(1, Ordering::AcqRel);
                HostAdmissionOutcome::spool_corrupted()
            })
        });
        let task = ProjectHostAdmissionReplayTask::start(broker, pass);

        tokio::time::timeout(Duration::from_secs(1), async {
            while task.pass_count() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("project replay must attempt the pending record");
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert_eq!(passes.load(Ordering::Acquire), 1);
        assert_eq!(task.pass_count(), 1);
        assert_eq!(task.backoff_count(), 0);
        task.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_task_aborts_an_in_flight_pass_without_an_arc_cycle() {
        let temp = tempfile::TempDir::new().unwrap();
        let (runtime, _) = tracedecay_usecases::host_admission::HostAdmissionRuntime::open(
            temp.path(),
            tracedecay_usecases::host_admission::SpoolBounds::default(),
        )
        .unwrap();
        let broker =
            Arc::new(tracedecay_usecases::host_admission::HostAdmissionBroker::new(runtime));
        let started = Arc::new(Notify::new());
        let started_for_run = Arc::clone(&started);
        let pass: PassFn = Arc::new(move || {
            let started = Arc::clone(&started_for_run);
            Box::pin(async move {
                started.notify_one();
                std::future::pending::<HostAdmissionOutcome>().await
            })
        });
        let task = ProjectHostAdmissionReplayTask::start(broker, pass);
        let worker = Arc::downgrade(task.worker());
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("project replay pass must start");

        drop(task);

        tokio::time::timeout(Duration::from_secs(1), async {
            while worker.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dropping the task must release its worker");
    }
}
