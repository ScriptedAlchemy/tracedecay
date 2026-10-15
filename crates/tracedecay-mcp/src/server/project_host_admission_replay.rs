//! Cancellable daemon-owned project host-admission replay worker.
//!
//! Continues bounded project host-admission replay passes while the
//! spool makes progress, applies retryable backoff, and is cancelled + joined
//! during project-server shutdown on every platform.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::Notify;
use tokio::task::JoinHandle;

use tracedecay_host_admission::{
    REPLAY_BACKOFF_SHIFT_CAP, ReplayPassDecision, SharedHostAdmissionBroker, classify_replay_pass,
    replay_backoff,
};
use tracedecay_runtime_core::native_runtime;
use tracedecay_sessions::admission::HostAdmissionOutcome;

type PassFn =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = HostAdmissionOutcome> + Send>> + Send + Sync>;

pub struct ProjectHostAdmissionReplayTask {
    worker: Arc<ProjectHostAdmissionReplayWorker>,
    task: Option<JoinHandle<()>>,
}

pub struct ProjectHostAdmissionReplayWorker {
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
    pub fn start(broker: SharedHostAdmissionBroker, pass: PassFn) -> Self {
        let worker = Arc::new(ProjectHostAdmissionReplayWorker::new(broker, pass));
        worker.kick();
        let task_worker = Arc::clone(&worker);
        // The replay pass is not traced Tokio-free, so it runs on Tokio. A
        // miss leaves no worker running; shutdown then has nothing to join.
        let task = tracedecay_runtime_core::native_runtime::tokio_handle(
            "mcp.project_host_admission_replay",
        )
        .map(|runtime| runtime.spawn(task_worker.run()));
        Self { worker, task }
    }

    #[cfg(any(test, feature = "test-transport"))]
    pub fn worker(&self) -> &Arc<ProjectHostAdmissionReplayWorker> {
        &self.worker
    }

    pub async fn shutdown(mut self) {
        self.worker.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }

    #[cfg(any(test, feature = "test-transport"))]
    pub fn pass_count(&self) -> usize {
        self.worker.pass_count()
    }

    #[cfg(any(test, feature = "test-transport"))]
    pub fn backoff_count(&self) -> usize {
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
    fn new(broker: SharedHostAdmissionBroker, pass: PassFn) -> Self {
        Self {
            broker,
            pass,
            cancel: AtomicBool::new(false),
            cancel_notify: Notify::new(),
            dirty: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            pass_count: AtomicUsize::new(0),
            backoff_count: AtomicUsize::new(0),
            idle: Notify::new(),
        }
    }

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

    #[cfg(any(test, feature = "test-transport"))]
    pub async fn wait_idle(&self) {
        loop {
            // `notify_waiters` stores no permit, so both waits are armed before
            // the state is read; an idle transition in between still wakes us.
            let idle = self.idle.notified();
            let cancelled = self.cancel_notify.notified();
            tokio::pin!(idle, cancelled);
            idle.as_mut().enable();
            cancelled.as_mut().enable();
            if self.cancel.load(Ordering::Acquire) {
                return;
            }
            if !self.busy.load(Ordering::Acquire)
                && !self.dirty.load(Ordering::Acquire)
                && !self.broker.has_pending_replay().await
            {
                return;
            }
            tokio::select! {
                () = idle => {}
                () = cancelled => return,
            }
        }
    }

    #[cfg(any(test, feature = "test-transport"))]
    pub fn pass_count(&self) -> usize {
        self.pass_count.load(Ordering::Acquire)
    }

    #[cfg(any(test, feature = "test-transport"))]
    pub fn backoff_count(&self) -> usize {
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
                            slept = native_runtime::sleep_until(Instant::now() + backoff) => {
                                // Retrying without the delay would spin; the
                                // record stays dirty for the next kick.
                                if let Err(error) = slept {
                                    tracing::warn!(
                                        event = "project_host_admission_replay_timer_unavailable",
                                        %error,
                                    );
                                    break;
                                }
                            }
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
                    ReplayPassDecision::TerminalNoop => {
                        consecutive_retryable = 0;
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

pub fn project_replay_backoff(attempt: u32) -> Duration {
    replay_backoff(attempt, REPLAY_BACKOFF_SHIFT_CAP)
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "tests run on an ambient Tokio runtime"
)]
mod tests {
    use super::*;

    #[test]
    fn project_replay_backoff_grows_then_caps() {
        assert_eq!(project_replay_backoff(1), Duration::from_millis(25));
        assert_eq!(project_replay_backoff(2), Duration::from_millis(50));
        assert_eq!(project_replay_backoff(3), Duration::from_millis(100));
        // the shared schedule saturates at the 2s absolute ceiling
        assert_eq!(project_replay_backoff(20), Duration::from_secs(2));
    }

    #[test]
    fn retryable_passes_back_off_on_native_timers_with_no_tokio_runtime_entered() {
        assert!(tokio::runtime::Handle::try_current().is_err());
        let temp = tempfile::TempDir::new().unwrap();
        let (runtime, _) = tracedecay_host_admission::HostAdmissionRuntime::open(
            temp.path(),
            tracedecay_host_admission::SpoolBounds::default(),
        )
        .unwrap();
        let broker = Arc::new(tracedecay_host_admission::HostAdmissionBroker::new(runtime));
        let passes = Arc::new(AtomicUsize::new(0));
        let third_pass = Arc::new(Notify::new());
        let pass: PassFn = {
            let passes = Arc::clone(&passes);
            let third_pass = Arc::clone(&third_pass);
            Arc::new(move || {
                let passes = Arc::clone(&passes);
                let third_pass = Arc::clone(&third_pass);
                Box::pin(async move {
                    if passes.fetch_add(1, Ordering::AcqRel) == 2 {
                        third_pass.notify_one();
                    }
                    HostAdmissionOutcome::retained_unavailable("test_retryable")
                })
            })
        };
        let worker = Arc::new(ProjectHostAdmissionReplayWorker::new(broker, pass));
        worker.kick();

        let started = Instant::now();
        let reached_third_pass = native_runtime::block_on(async {
            tokio::select! {
                biased;
                () = third_pass.notified() => true,
                () = Arc::clone(&worker).run() => false,
                _ = native_runtime::sleep_until(started + Duration::from_secs(10)) => false,
            }
        })
        .expect("native runtime");
        let elapsed = started.elapsed();

        assert!(reached_third_pass, "retryable passes must keep retrying");
        assert_eq!(passes.load(Ordering::Acquire), 3);
        let floor = project_replay_backoff(1) + project_replay_backoff(2);
        assert!(
            elapsed >= floor,
            "two backoffs must wait before the third pass; elapsed={elapsed:?} floor={floor:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn non_retryable_pending_record_stops_until_an_external_kick() {
        let temp = tempfile::TempDir::new().unwrap();
        let (runtime, _) = tracedecay_host_admission::HostAdmissionRuntime::open(
            temp.path(),
            tracedecay_host_admission::SpoolBounds::default(),
        )
        .unwrap();
        let broker = Arc::new(tracedecay_host_admission::HostAdmissionBroker::new(runtime));
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
    async fn not_applicable_pending_record_stops_without_backoff() {
        let temp = tempfile::TempDir::new().unwrap();
        let (runtime, _) = tracedecay_host_admission::HostAdmissionRuntime::open(
            temp.path(),
            tracedecay_host_admission::SpoolBounds::default(),
        )
        .unwrap();
        let broker = Arc::new(tracedecay_host_admission::HostAdmissionBroker::new(runtime));
        broker.admit("test:pending", b"pending").await.unwrap();
        let passes = Arc::new(AtomicUsize::new(0));
        let passes_for_run = Arc::clone(&passes);
        let pass: PassFn = Arc::new(move || {
            let passes = Arc::clone(&passes_for_run);
            Box::pin(async move {
                passes.fetch_add(1, Ordering::AcqRel);
                HostAdmissionOutcome::not_applicable("code_index_not_applicable")
            })
        });
        let task = ProjectHostAdmissionReplayTask::start(broker, pass);

        tokio::time::timeout(Duration::from_secs(1), async {
            while task.pass_count() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("project replay must attempt the not-applicable record");
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert_eq!(passes.load(Ordering::Acquire), 1);
        assert_eq!(task.pass_count(), 1);
        assert_eq!(task.backoff_count(), 0);
        task.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_task_aborts_an_in_flight_pass_without_an_arc_cycle() {
        let temp = tempfile::TempDir::new().unwrap();
        let (runtime, _) = tracedecay_host_admission::HostAdmissionRuntime::open(
            temp.path(),
            tracedecay_host_admission::SpoolBounds::default(),
        )
        .unwrap();
        let broker = Arc::new(tracedecay_host_admission::HostAdmissionBroker::new(runtime));
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
