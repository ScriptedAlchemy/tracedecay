//! Server-shaped lifecycle observation ports.
//!
//! The concrete daemon lifecycle lives in `tracedecay_daemon_service::shutdown`;
//! this module adapts it to the port the MCP connection loop observes drain
//! and request admission through.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_daemon_service::shutdown::DaemonLifecycle;

/// Request-activity guard retained while one MCP request is admitted.
///
/// Dropping the guard releases the underlying lifecycle seat. The boxed
/// retainee is the root-implemented activity token.
pub struct McpRequestActivity {
    _retain: Box<dyn Send>,
}

impl McpRequestActivity {
    pub fn retain<T: Send + 'static>(guard: T) -> Self {
        Self {
            _retain: Box::new(guard),
        }
    }
}

pub type McpLifecycleDrainFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// Observe daemon drain and admit one request seat without naming daemon types.
pub trait McpConnectionLifecyclePort: Send + Sync {
    fn accepting(&self) -> bool;
    fn try_enter(&self) -> Option<McpRequestActivity>;
    fn wait_for_draining(&self) -> McpLifecycleDrainFuture<'_>;
}

impl McpConnectionLifecyclePort for DaemonLifecycle {
    fn accepting(&self) -> bool {
        DaemonLifecycle::accepting(self)
    }

    fn try_enter(&self) -> Option<McpRequestActivity> {
        DaemonLifecycle::try_enter(self).map(McpRequestActivity::retain)
    }

    fn wait_for_draining(&self) -> McpLifecycleDrainFuture<'_> {
        Box::pin(DaemonLifecycle::wait_for_draining(self))
    }
}

/// Bound on the join failures retained from tasks reaped during normal
/// operation. Shutdown reports these alongside anything it drains itself; a
/// long-lived server keeps only this many panics rather than one per task.
const REAPED_FAILURE_CAPACITY: usize = 16;

/// Owns every background task one server spawns.
///
/// Admission and the live task set share one lock. Each admission first reaps
/// the tasks that have already finished, so the set holds only live work plus
/// whatever completed since the last spawn, and a panic surfaces on the next
/// admission instead of waiting for shutdown. Reaping never awaits a live
/// task: `try_join_next` only returns results that are already ready.
#[derive(Default)]
pub struct McpBackgroundTaskOwner {
    admission: std::sync::Mutex<McpBackgroundTaskAdmission>,
    shutdown_tasks: tokio::sync::Mutex<Option<tokio::task::JoinSet<()>>>,
}

#[derive(Default)]
struct McpBackgroundTaskAdmission {
    closed: bool,
    tasks: tokio::task::JoinSet<()>,
    /// Join failures observed while reaping, bounded by
    /// [`REAPED_FAILURE_CAPACITY`]; the oldest is dropped first.
    reaped_failures: std::collections::VecDeque<String>,
}

impl McpBackgroundTaskAdmission {
    /// Drains every already-finished task. A panic or abort is recorded once,
    /// here, and logged at the moment it is observed.
    fn reap_finished(&mut self) {
        while let Some(result) = self.tasks.try_join_next() {
            if let Err(error) = result
                && !error.is_cancelled()
            {
                let failure = error.to_string();
                tracing::error!(error = %failure, "MCP background task failed");
                if self.reaped_failures.len() == REAPED_FAILURE_CAPACITY {
                    self.reaped_failures.pop_front();
                }
                self.reaped_failures.push_back(failure);
            }
        }
    }
}

impl McpBackgroundTaskOwner {
    pub fn spawn<Task>(&self, task: Task) -> bool
    where
        Task: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut admission = self
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if admission.closed {
            return false;
        }
        admission.reap_finished();
        admission.tasks.spawn(task);
        true
    }

    /// Close admission without joining. Daemon shutdown uses this at TERM so
    /// a last read cannot start another reconcile while owners drain.
    pub fn close_admission(&self) {
        let mut admission = self
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        admission.closed = true;
        admission.reap_finished();
    }

    pub fn admits(&self) -> bool {
        !self
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closed
    }

    /// Tasks currently retained by the owner: live work plus completions not
    /// yet reaped by an admission.
    #[cfg(test)]
    fn retained_tasks(&self) -> usize {
        self.admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .tasks
            .len()
    }

    /// Join failures reaped during normal operation and not yet reported by
    /// shutdown.
    #[cfg(test)]
    fn reaped_failures(&self) -> Vec<String> {
        self.admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reaped_failures
            .iter()
            .cloned()
            .collect()
    }

    /// Closes admission, aborts and joins every retained task, and returns the
    /// join failures: those reaped earlier plus those observed by this drain.
    /// A caller cancelled mid-drain leaves the set retained, so a retry joins
    /// the same tasks; the failures reaped earlier are reported by whichever
    /// call completes the drain.
    #[hotpath::measure(label = "mcp.server.background_shutdown", future = true)]
    pub async fn shutdown(&self) -> Vec<String> {
        let mut retained = self.shutdown_tasks.lock().await;
        if retained.is_none() {
            let tasks = {
                let mut admission = self
                    .admission
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                admission.closed = true;
                std::mem::take(&mut admission.tasks)
            };
            *retained = Some(tasks);
        }
        let Some(tasks) = retained.as_mut() else {
            return Vec::new();
        };
        tasks.abort_all();
        let mut failures = Vec::new();
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result
                && !error.is_cancelled()
            {
                failures.push(error.to_string());
            }
        }
        retained.take();
        let mut reaped: Vec<String> = self
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reaped_failures
            .drain(..)
            .collect();
        reaped.append(&mut failures);
        reaped
    }
}

/// Retained startup reconciliation-admission task, joined or aborted before the code graph
/// authority is released.
#[derive(Default)]
struct StartupCatchUpTasksV1 {
    sync: Option<tokio::task::JoinHandle<()>>,
}

/// The startup catch-up lifecycle as one linear machine.
///
/// Dispatch and readiness live in the same state machine. The hazard that
/// motivated it: the old completion flag defaulted to
/// `true` so a server with no catch-up reported "settled", which forced the
/// dispatch site to pre-clear them in a separate store *before* spawning —
/// an ordering that was documented rather than enforced. Here, dispatch
/// *is* the transition into [`Self::Syncing`], so no window exists in which
/// a dispatched catch-up still reads as settled.
enum StartupCatchUpStateV1 {
    /// No catch-up was ever dispatched (session-start sync disabled, or a
    /// construction path that opts out). Terminal, and *ready*: waiters must
    /// not block on work that will never run.
    NotStarted,
    /// Reconciliation admission is running.
    Syncing { tasks: StartupCatchUpTasksV1 },
    /// Reconciliation admission settled, including failure paths.
    Settled { tasks: StartupCatchUpTasksV1 },
    /// Shutdown tore the machine down.
    Cancelled,
}

impl StartupCatchUpStateV1 {
    #[hotpath::skip]
    const fn settled(&self) -> bool {
        !matches!(self, Self::Syncing { .. })
    }

    fn tasks_mut(&mut self) -> Option<&mut StartupCatchUpTasksV1> {
        match self {
            Self::Syncing { tasks } | Self::Settled { tasks } => Some(tasks),
            Self::NotStarted | Self::Cancelled => None,
        }
    }

    fn take_tasks(&mut self) -> StartupCatchUpTasksV1 {
        self.tasks_mut().map(std::mem::take).unwrap_or_default()
    }
}

/// Owns the startup reconciliation-admission state.
///
/// Held behind an `Arc` on the server so the spawned sync task can signal
/// completion through the same lock the waiters read.
/// The lock is a `std::sync::Mutex` on purpose: every critical section is a
/// phase swap or a handle take, and joins always happen *outside* it, so the
/// sync readiness accessors stay callable from non-async code.
pub struct StartupCatchUpMachineV1 {
    state: std::sync::Mutex<StartupCatchUpStateV1>,
    /// Set once the first dispatch claims the machine. Kept distinct from
    /// the phase so a completed catch-up still refuses a second dispatch.
    dispatched: std::sync::atomic::AtomicBool,
}

impl Default for StartupCatchUpMachineV1 {
    fn default() -> Self {
        Self {
            state: std::sync::Mutex::new(StartupCatchUpStateV1::NotStarted),
            dispatched: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl StartupCatchUpMachineV1 {
    fn state(&self) -> std::sync::MutexGuard<'_, StartupCatchUpStateV1> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// One-shot dispatch claim. The first caller wins and the machine enters
    /// [`StartupCatchUpStateV1::Syncing`] in the same critical section, so
    /// there is no interval in which a dispatched catch-up reads as settled.
    pub fn try_claim_dispatch(&self) -> bool {
        if self
            .dispatched
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_err()
        {
            return false;
        }
        let mut state = self.state();
        if matches!(*state, StartupCatchUpStateV1::Cancelled) {
            return false;
        }
        let tasks = state.take_tasks();
        *state = StartupCatchUpStateV1::Syncing { tasks };
        true
    }

    /// Enters the synchronous phase for a direct
    /// server's startup catch-up call. Idempotent for the
    /// dispatched path, which is already `Syncing`. A cancelled machine
    /// stays cancelled: shutdown has already released what this phase needs.
    pub fn begin_sync(&self) {
        let mut state = self.state();
        if matches!(*state, StartupCatchUpStateV1::Cancelled) {
            return;
        }
        let tasks = state.take_tasks();
        *state = StartupCatchUpStateV1::Syncing { tasks };
    }

    /// The reconciliation-admission phase is done.
    pub fn settle(&self) {
        let mut state = self.state();
        if matches!(*state, StartupCatchUpStateV1::Cancelled) {
            return;
        }
        let tasks = state.take_tasks();
        *state = StartupCatchUpStateV1::Settled { tasks };
    }

    pub fn install_sync_task(&self, task: tokio::task::JoinHandle<()>) {
        let mut state = self.state();
        match state.tasks_mut() {
            Some(tasks) => tasks.sync = Some(task),
            // Shutdown won the race; nothing will ever join this handle.
            None => task.abort(),
        }
    }

    pub fn take_sync_task(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.state().tasks_mut().and_then(|tasks| tasks.sync.take())
    }

    /// Terminal shutdown state.
    pub fn mark_cancelled(&self) {
        *self.state() = StartupCatchUpStateV1::Cancelled;
    }

    pub fn settled(&self) -> bool {
        self.state().settled()
    }
}

/// Owns response admission, revocation, and forced cancellation for one
/// daemon-retained project server.
#[derive(Clone)]
pub struct ProjectServerResponseLifecycle {
    response_gate: Arc<tokio::sync::RwLock<()>>,
    response_revoked: tracedecay_session_memory::context::CancellationToken,
    request_abort: tracedecay_session_memory::context::CancellationToken,
}

impl Default for ProjectServerResponseLifecycle {
    fn default() -> Self {
        Self {
            response_gate: Arc::new(tokio::sync::RwLock::new(())),
            response_revoked: tracedecay_session_memory::context::CancellationToken::new(),
            request_abort: tracedecay_session_memory::context::CancellationToken::new(),
        }
    }
}

impl ProjectServerResponseLifecycle {
    pub fn revoke(&self) {
        self.response_revoked.cancel();
    }

    /// Close response admission without invalidating an already-admitted reply.
    /// Tokio's write-preferring lock prevents later readers from overtaking the
    /// queued retirement writer, so cancellation is published at the cutover.
    #[hotpath::measure(label = "mcp.server.revoke_drain", future = true)]
    pub async fn revoke_after_request_drain(&self) {
        let _guard = self.response_gate.write().await;
        self.response_revoked.cancel();
    }

    pub async fn wait_for_request_drain(&self) {
        let _guard = self.response_gate.write().await;
    }

    pub fn abort_requests(&self) {
        self.request_abort.cancel();
    }

    pub fn response_gate(&self) -> &Arc<tokio::sync::RwLock<()>> {
        &self.response_gate
    }

    pub fn response_revoked(&self) -> &tracedecay_session_memory::context::CancellationToken {
        &self.response_revoked
    }
}

#[cfg(test)]
mod response_lifecycle_tests {
    use super::*;

    #[tokio::test]
    async fn response_revocation_waits_for_admitted_response_lease_to_drain() {
        let lifecycle = ProjectServerResponseLifecycle::default();
        let admitted_response = Arc::clone(lifecycle.response_gate()).read_owned().await;
        let mut retirement = Box::pin(lifecycle.revoke_after_request_drain());
        std::future::poll_fn(|context| {
            let retirement_poll = std::future::Future::poll(retirement.as_mut(), context);
            assert!(
                retirement_poll.is_pending(),
                "retirement bypassed an admitted response"
            );
            std::task::Poll::Ready(())
        })
        .await;

        assert!(
            !lifecycle.response_revoked().is_cancelled(),
            "retirement must not revoke an already-admitted response"
        );
        drop(admitted_response);
        retirement.await;
        assert!(lifecycle.response_revoked().is_cancelled());
    }
}

#[cfg(test)]
mod background_task_owner_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct DropSignal(Arc<AtomicBool>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    struct DropCount(Arc<std::sync::atomic::AtomicUsize>);

    impl Drop for DropCount {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Yields until `settled` is set: the task that owns it has run to
    /// completion (or unwound) on the test runtime.
    async fn wait_until_set(settled: &AtomicBool) {
        for _ in 0..1_000 {
            if settled.load(Ordering::Acquire) {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("background task did not settle on the test runtime");
    }

    #[tokio::test]
    async fn shutdown_aborts_joins_and_closes_background_task_admission() {
        let owner = McpBackgroundTaskOwner::default();
        let dropped = Arc::new(AtomicBool::new(false));
        let task_dropped = Arc::clone(&dropped);
        assert!(owner.spawn(async move {
            let _signal = DropSignal(task_dropped);
            std::future::pending::<()>().await;
        }));
        tokio::task::yield_now().await;

        assert!(owner.shutdown().await.is_empty());
        assert!(dropped.load(Ordering::Acquire));
        assert!(!owner.spawn(async {}));
    }

    #[tokio::test]
    async fn close_admission_refuses_new_background_work_without_joining() {
        let owner = McpBackgroundTaskOwner::default();
        let dropped = Arc::new(AtomicBool::new(false));
        let task_dropped = Arc::clone(&dropped);
        assert!(owner.spawn(async move {
            let _signal = DropSignal(task_dropped);
            std::future::pending::<()>().await;
        }));
        tokio::task::yield_now().await;

        owner.close_admission();
        assert!(!owner.spawn(async {}));
        assert!(
            !dropped.load(Ordering::Acquire),
            "close_admission must not join or abort live tasks"
        );
        assert!(owner.shutdown().await.is_empty());
        assert!(dropped.load(Ordering::Acquire));
    }

    /// A sustained run of short tasks must not accumulate in the owner: once
    /// they finish, the next admission reaps them without shutdown and
    /// without waiting on the live task that is still running.
    #[tokio::test]
    async fn completed_tasks_are_reaped_by_the_next_admission_before_shutdown() {
        let owner = McpBackgroundTaskOwner::default();
        let finished = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let live_started = Arc::new(AtomicBool::new(false));
        let task_live_started = Arc::clone(&live_started);
        assert!(owner.spawn(async move {
            task_live_started.store(true, Ordering::Release);
            std::future::pending::<()>().await;
        }));
        const COMPLETED: usize = 32;
        for _ in 0..COMPLETED {
            let finished = Arc::clone(&finished);
            assert!(owner.spawn(async move {
                finished.fetch_add(1, Ordering::AcqRel);
            }));
        }
        wait_until_set(&live_started).await;
        for _ in 0..1_000 {
            if finished.load(Ordering::Acquire) == COMPLETED {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(finished.load(Ordering::Acquire), COMPLETED);
        assert!(
            owner.retained_tasks() > 1,
            "completed tasks stay retained until an admission reaps them"
        );

        let admitted = Arc::new(AtomicBool::new(false));
        let task_admitted = Arc::clone(&admitted);
        assert!(owner.spawn(async move {
            task_admitted.store(true, Ordering::Release);
        }));

        assert_eq!(
            owner.retained_tasks(),
            2,
            "after reaping, the owner retains only the live task and the task just admitted"
        );
        assert!(owner.reaped_failures().is_empty());
        wait_until_set(&admitted).await;
        assert!(owner.shutdown().await.is_empty());
    }

    /// A panicked task is observed by the next admission, once, and shutdown
    /// reports it exactly once rather than discovering it for the first time.
    #[tokio::test]
    async fn panicked_task_evidence_surfaces_on_the_next_admission() {
        let owner = McpBackgroundTaskOwner::default();
        let unwound = Arc::new(AtomicBool::new(false));
        let task_unwound = Arc::clone(&unwound);
        assert!(owner.spawn(async move {
            let _signal = DropSignal(task_unwound);
            panic!("background task failed on purpose");
        }));
        wait_until_set(&unwound).await;
        assert!(
            owner.reaped_failures().is_empty(),
            "a finished task is reaped by an admission, not by completing"
        );

        assert!(owner.spawn(async {}));

        let reaped = owner.reaped_failures();
        assert_eq!(reaped.len(), 1, "one panic yields one failure record");
        assert!(
            reaped[0].contains("panicked"),
            "the join failure names the panic: {}",
            reaped[0]
        );
        assert_eq!(
            owner.retained_tasks(),
            1,
            "the panicked task is no longer retained; only the new task is"
        );

        assert!(owner.spawn(async {}));
        assert_eq!(
            owner.reaped_failures().len(),
            1,
            "reaping again must not re-observe the same panic"
        );

        let failures = owner.shutdown().await;
        assert_eq!(failures.len(), 1, "shutdown reports the reaped panic once");
        assert!(failures[0].contains("panicked"));
        assert!(
            owner.shutdown().await.is_empty(),
            "a second shutdown has nothing left to report"
        );
    }

    /// The reaped-failure record is bounded: a server that outlives many
    /// panics keeps the newest ones, not one entry per failed task.
    #[tokio::test]
    async fn reaped_failures_keep_only_the_newest_bounded_records() {
        let owner = McpBackgroundTaskOwner::default();
        let total = REAPED_FAILURE_CAPACITY + 4;
        let unwound = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for ordinal in 0..total {
            let unwound = Arc::clone(&unwound);
            assert!(owner.spawn(async move {
                let _count = DropCount(unwound);
                panic!("bounded failure {ordinal}");
            }));
        }
        for _ in 0..1_000 {
            if unwound.load(Ordering::Acquire) == total {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(unwound.load(Ordering::Acquire), total);

        assert!(owner.spawn(async {}));

        let reaped = owner.reaped_failures();
        assert_eq!(reaped.len(), REAPED_FAILURE_CAPACITY);
        assert_eq!(owner.shutdown().await.len(), REAPED_FAILURE_CAPACITY);
    }
}
