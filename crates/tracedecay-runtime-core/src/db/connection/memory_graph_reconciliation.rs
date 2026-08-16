use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::task::JoinHandle;

use super::{Database, WeakDatabase};

const AUTOMATIC_RETRY_LIMIT: u32 = 3;
const AUTOMATIC_RETRY_BASE: Duration = Duration::from_millis(25);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MemoryGraphReconciliationTaskScheduleV1 {
    Scheduled,
    AlreadyScheduled,
    Retiring,
    Closed,
}

/// The reconciliation worker has not entered a state that can safely be
/// fenced for coordinated runtime retirement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryGraphReconciliationRetirementBlockerV1 {
    Pending,
    Running,
    RetainedJoinWork,
    Retiring,
    Closed,
}

/// The verified graph runtime disappeared after the coordinator was attached.
/// This remains typed so a retirement caller cannot mistake a vanished runtime
/// for a completed cancellation or close.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MemoryGraphReconciliationRuntimeErrorV1 {
    RuntimeUnavailable,
    CloseFailed(String),
}

/// A non-destructive fence over an idle reconciliation coordinator.
///
/// While held, new schedules receive [`MemoryGraphReconciliationTaskScheduleV1::Retiring`].
/// Dropping an uncommitted fence restores ordinary scheduling; [`Self::commit`]
/// is the only path that cancels and joins the coordinator.
pub struct MemoryGraphReconciliationRetirementReservationV1 {
    owner: MemoryGraphReconciliationTaskOwnerV1,
    armed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectMemoryReconciliationTelemetrySnapshotV1 {
    pub reconciliation_passes: u64,
    pub active_reconciliation_pass_count: u64,
    pub source_rows_loaded: u64,
    pub source_bytes_loaded: u64,
    pub publication_attempts: u64,
    pub retained_reconciliation_task_count: usize,
    pub retained_graph_owner_count: usize,
}

#[derive(Default)]
pub(crate) struct ProjectMemoryReconciliationTelemetryV1 {
    reconciliation_passes: AtomicU64,
    active_reconciliation_pass_count: AtomicU64,
    source_rows_loaded: AtomicU64,
    source_bytes_loaded: AtomicU64,
    publication_attempts: AtomicU64,
}

#[derive(Clone)]
pub struct ProjectMemoryReconciliationTelemetryObserverV1 {
    telemetry: Arc<ProjectMemoryReconciliationTelemetryV1>,
    database: Weak<super::registry::DatabaseInner>,
}

pub(crate) struct ProjectMemoryReconciliationPassLeaseV1 {
    telemetry: Arc<ProjectMemoryReconciliationTelemetryV1>,
}

impl ProjectMemoryReconciliationTelemetryObserverV1 {
    pub(super) fn new(
        telemetry: Arc<ProjectMemoryReconciliationTelemetryV1>,
        database: Weak<super::registry::DatabaseInner>,
    ) -> Self {
        Self {
            telemetry,
            database,
        }
    }

    pub fn snapshot(&self) -> ProjectMemoryReconciliationTelemetrySnapshotV1 {
        let (retained_reconciliation_task_count, retained_graph_owner_count) =
            self.database.upgrade().map_or((0, 0), |database| {
                (
                    database.memory_graph_reconciliation.retained_task_count(),
                    usize::from(database.memory_graph_runtime.get().is_some()),
                )
            });
        ProjectMemoryReconciliationTelemetrySnapshotV1 {
            reconciliation_passes: self.telemetry.reconciliation_passes.load(Ordering::SeqCst),
            active_reconciliation_pass_count: self
                .telemetry
                .active_reconciliation_pass_count
                .load(Ordering::SeqCst),
            source_rows_loaded: self.telemetry.source_rows_loaded.load(Ordering::SeqCst),
            source_bytes_loaded: self.telemetry.source_bytes_loaded.load(Ordering::SeqCst),
            publication_attempts: self.telemetry.publication_attempts.load(Ordering::SeqCst),
            retained_reconciliation_task_count,
            retained_graph_owner_count,
        }
    }
}

impl ProjectMemoryReconciliationTelemetryV1 {
    pub(crate) fn begin_reconciliation_pass(
        self: &Arc<Self>,
    ) -> Result<ProjectMemoryReconciliationPassLeaseV1, &'static str> {
        increment_counter(
            &self.active_reconciliation_pass_count,
            1,
            "active reconciliation passes",
        )?;
        if let Err(error) =
            increment_counter(&self.reconciliation_passes, 1, "reconciliation passes")
        {
            decrement_counter(
                &self.active_reconciliation_pass_count,
                "active reconciliation passes",
            )?;
            return Err(error);
        }
        Ok(ProjectMemoryReconciliationPassLeaseV1 {
            telemetry: Arc::clone(self),
        })
    }

    pub(crate) fn record_source_load(&self, rows: u64, bytes: u64) -> Result<(), &'static str> {
        increment_counter(&self.source_rows_loaded, rows, "source rows loaded")?;
        increment_counter(&self.source_bytes_loaded, bytes, "source bytes loaded")
    }

    pub(crate) fn record_publication_attempt(&self) -> Result<(), &'static str> {
        increment_counter(&self.publication_attempts, 1, "publication attempts")
    }
}

impl Drop for ProjectMemoryReconciliationPassLeaseV1 {
    fn drop(&mut self) {
        self.telemetry
            .active_reconciliation_pass_count
            .fetch_sub(1, Ordering::SeqCst);
    }
}

fn increment_counter(
    counter: &AtomicU64,
    increment: u64,
    counter_name: &'static str,
) -> Result<(), &'static str> {
    counter
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
            current.checked_add(increment)
        })
        .map(|_| ())
        .map_err(|_| counter_name)
}

fn decrement_counter(counter: &AtomicU64, counter_name: &'static str) -> Result<(), &'static str> {
    counter
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
            current.checked_sub(1)
        })
        .map(|_| ())
        .map_err(|_| counter_name)
}

#[derive(Default)]
struct MemoryGraphReconciliationTaskStateV1 {
    accepting: bool,
    retirement_reserved: bool,
    pending: bool,
    running: bool,
    current_identity: Option<Arc<()>>,
    current: Option<JoinHandle<()>>,
    retired: Vec<JoinHandle<()>>,
    joining: bool,
    joining_task_count: usize,
}

struct MemoryGraphReconciliationSharedV1 {
    state: Mutex<MemoryGraphReconciliationTaskStateV1>,
    wake: Arc<Notify>,
    joined: Arc<Notify>,
}

impl Default for MemoryGraphReconciliationSharedV1 {
    fn default() -> Self {
        Self {
            state: Mutex::new(MemoryGraphReconciliationTaskStateV1 {
                accepting: true,
                ..MemoryGraphReconciliationTaskStateV1::default()
            }),
            wake: Arc::new(Notify::new()),
            joined: Arc::new(Notify::new()),
        }
    }
}

impl Drop for MemoryGraphReconciliationSharedV1 {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(task) = state.current.as_ref() {
            task.abort();
        }
        for task in &state.retired {
            task.abort();
        }
    }
}

#[derive(Default)]
pub(super) struct MemoryGraphReconciliationCoordinatorV1 {
    shared: Arc<MemoryGraphReconciliationSharedV1>,
}

#[derive(Clone)]
pub struct MemoryGraphReconciliationTaskOwnerV1 {
    shared: Arc<MemoryGraphReconciliationSharedV1>,
    cancel_reconciliation:
        Arc<dyn Fn() -> Result<(), MemoryGraphReconciliationRuntimeErrorV1> + Send + Sync>,
    close_reconciliation:
        Arc<dyn Fn() -> Result<(), MemoryGraphReconciliationRuntimeErrorV1> + Send + Sync>,
}

impl MemoryGraphReconciliationCoordinatorV1 {
    pub(super) fn task_owner(
        &self,
        cancel_reconciliation: Arc<
            dyn Fn() -> Result<(), MemoryGraphReconciliationRuntimeErrorV1> + Send + Sync,
        >,
        close_reconciliation: Arc<
            dyn Fn() -> Result<(), MemoryGraphReconciliationRuntimeErrorV1> + Send + Sync,
        >,
    ) -> MemoryGraphReconciliationTaskOwnerV1 {
        MemoryGraphReconciliationTaskOwnerV1 {
            shared: Arc::clone(&self.shared),
            cancel_reconciliation,
            close_reconciliation,
        }
    }

    pub(super) fn schedule<Operation, OperationFuture>(
        &self,
        database: &Database,
        operation: Operation,
    ) -> MemoryGraphReconciliationTaskScheduleV1
    where
        Operation: Fn(WeakDatabase) -> OperationFuture + Send + 'static,
        OperationFuture: Future<Output = bool> + Send + 'static,
    {
        self.schedule_weak(database.downgrade(), operation)
    }

    fn schedule_weak<Operation, OperationFuture>(
        &self,
        weak_database: WeakDatabase,
        operation: Operation,
    ) -> MemoryGraphReconciliationTaskScheduleV1
    where
        Operation: Fn(WeakDatabase) -> OperationFuture + Send + 'static,
        OperationFuture: Future<Output = bool> + Send + 'static,
    {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.accepting {
            return MemoryGraphReconciliationTaskScheduleV1::Closed;
        }
        if state.retirement_reserved {
            return MemoryGraphReconciliationTaskScheduleV1::Retiring;
        }
        state.pending = true;
        if state
            .current
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            self.shared.wake.notify_one();
            return MemoryGraphReconciliationTaskScheduleV1::AlreadyScheduled;
        }
        if let Some(finished) = state.current.take() {
            state.retired.push(finished);
        }
        state.current_identity = None;
        let worker_identity = Arc::new(());
        let weak_shared = Arc::downgrade(&self.shared);
        let wake = Arc::clone(&self.shared.wake);
        let task_identity = Arc::clone(&worker_identity);
        let task = tokio::spawn(async move {
            run_memory_graph_reconciliation_worker(
                weak_shared,
                task_identity,
                wake,
                weak_database,
                operation,
            )
            .await;
        });
        // The worker needs this same state lock before its first pass, so the
        // join handle is retained before the task can complete or be replaced.
        state.current_identity = Some(worker_identity);
        state.current = Some(task);
        self.shared.wake.notify_one();
        MemoryGraphReconciliationTaskScheduleV1::Scheduled
    }

    pub(super) fn pending(&self) -> bool {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
    }

    fn retained_task_count(&self) -> usize {
        let state = match self.shared.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        usize::from(state.current.is_some()) + state.retired.len() + state.joining_task_count
    }
}

struct MemoryGraphReconciliationWorkerGuardV1 {
    shared: Weak<MemoryGraphReconciliationSharedV1>,
    identity: Arc<()>,
}

impl Drop for MemoryGraphReconciliationWorkerGuardV1 {
    fn drop(&mut self) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let mut state = shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .current_identity
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &self.identity))
        {
            state.running = false;
            if !state.accepting {
                state.pending = false;
            }
        }
    }
}

async fn run_memory_graph_reconciliation_worker<Operation, OperationFuture>(
    weak_shared: Weak<MemoryGraphReconciliationSharedV1>,
    identity: Arc<()>,
    wake: Arc<Notify>,
    weak_database: WeakDatabase,
    operation: Operation,
) where
    Operation: Fn(WeakDatabase) -> OperationFuture + Send + 'static,
    OperationFuture: Future<Output = bool> + Send + 'static,
{
    let _worker_guard = MemoryGraphReconciliationWorkerGuardV1 {
        shared: weak_shared.clone(),
        identity,
    };
    let mut automatic_failures = 0_u32;
    loop {
        let notified = wake.notified();
        let Some(shared) = weak_shared.upgrade() else {
            return;
        };
        let should_run = {
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !state.accepting {
                return;
            }
            if state.pending {
                state.pending = false;
                state.running = true;
                true
            } else {
                false
            }
        };
        drop(shared);
        if !should_run {
            notified.await;
            continue;
        }

        let settled = operation(weak_database.clone()).await;
        let Some(shared) = weak_shared.upgrade() else {
            return;
        };
        if settled {
            automatic_failures = 0;
        }
        let Some((continue_now, retry_delay)) = ({
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.running = false;
            if !state.accepting {
                state.pending = false;
                None
            } else if !settled && !state.pending {
                state.pending = true;
                let retry_delay = (automatic_failures < AUTOMATIC_RETRY_LIMIT).then(|| {
                    let delay = AUTOMATIC_RETRY_BASE.saturating_mul(1 << automatic_failures);
                    automatic_failures += 1;
                    delay
                });
                Some((false, retry_delay))
            } else {
                if state.pending {
                    automatic_failures = 0;
                }
                Some((state.pending, None))
            }
        }) else {
            return;
        };
        if continue_now {
            continue;
        }
        if let Some(delay) = retry_delay {
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = notified => automatic_failures = 0,
            }
        } else {
            notified.await;
            automatic_failures = 0;
        }
    }
}

impl MemoryGraphReconciliationTaskOwnerV1 {
    pub fn same_coordinator(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }

    pub fn cancel(&self) -> Result<(), MemoryGraphReconciliationRuntimeErrorV1> {
        let cancellation = (self.cancel_reconciliation)();
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.accepting = false;
        state.pending = false;
        self.shared.wake.notify_waiters();
        cancellation
    }

    /// Fences an idle coordinator before a caller attempts the external graph
    /// and store-runtime retirement reservations. The fence does not cancel a
    /// task or close a graph; that remains deferred to [`Self::shutdown`] at
    /// the coordinated commit boundary.
    pub fn reserve_retirement(
        &self,
    ) -> Result<
        MemoryGraphReconciliationRetirementReservationV1,
        MemoryGraphReconciliationRetirementBlockerV1,
    > {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.accepting {
            return Err(MemoryGraphReconciliationRetirementBlockerV1::Closed);
        }
        if state.retirement_reserved {
            return Err(MemoryGraphReconciliationRetirementBlockerV1::Retiring);
        }
        if state.pending {
            return Err(MemoryGraphReconciliationRetirementBlockerV1::Pending);
        }
        if state.running {
            return Err(MemoryGraphReconciliationRetirementBlockerV1::Running);
        }
        if state.joining || !state.retired.is_empty() || state.joining_task_count != 0 {
            return Err(MemoryGraphReconciliationRetirementBlockerV1::RetainedJoinWork);
        }
        state.retirement_reserved = true;
        Ok(MemoryGraphReconciliationRetirementReservationV1 {
            owner: self.clone(),
            armed: true,
        })
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        let cancellation_error = self.cancel().err();
        loop {
            let joined = self.shared.joined.notified();
            let tasks = {
                let mut state = self
                    .shared
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.joining {
                    None
                } else {
                    state.joining = true;
                    let mut tasks = std::mem::take(&mut state.retired);
                    if let Some(current) = state.current.take() {
                        tasks.push(current);
                    }
                    state.joining_task_count = tasks.len();
                    Some(tasks)
                }
            };
            let Some(tasks) = tasks else {
                joined.await;
                continue;
            };
            let close = self.join_and_close(tasks).await;
            return match (cancellation_error, close) {
                (None, result) => result,
                (Some(error), Ok(())) => Err(format!("cancel reconciliation: {error:?}")),
                (Some(error), Err(close_error)) => Err(format!(
                    "cancel reconciliation: {error:?}; close reconciliation: {close_error}"
                )),
            };
        }
    }

    async fn join_and_close(&self, tasks: Vec<JoinHandle<()>>) -> Result<(), String> {
        let mut lease = MemoryGraphReconciliationJoinLeaseV1 {
            shared: Arc::clone(&self.shared),
            tasks,
        };
        let mut failures = Vec::new();
        while !lease.tasks.is_empty() {
            let result = {
                let Some(task) = lease.tasks.last_mut() else {
                    break;
                };
                task.await
            };
            lease.remove_completed_task();
            match result {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {}
                Err(error) => failures.push(error.to_string()),
            }
        }
        if let Err(error) = (self.close_reconciliation)() {
            failures.push(format!("{error:?}"));
        }
        drop(lease);
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    pub fn pending(&self) -> bool {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
    }

    pub fn running(&self) -> bool {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .running
    }
}

impl MemoryGraphReconciliationRetirementReservationV1 {
    /// Cancels, joins, and closes the reconciler after every external runtime
    /// reservation has entered its own irreversible commit phase.
    pub async fn commit(mut self) -> Result<(), String> {
        if !self.armed {
            return Err(
                "memory graph reconciliation retirement reservation was consumed".to_owned(),
            );
        }
        self.armed = false;
        self.owner.shutdown().await
    }
}

impl Drop for MemoryGraphReconciliationRetirementReservationV1 {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut state = self
            .owner
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.accepting {
            state.retirement_reserved = false;
            self.owner.shared.wake.notify_waiters();
        }
    }
}

struct MemoryGraphReconciliationJoinLeaseV1 {
    shared: Arc<MemoryGraphReconciliationSharedV1>,
    tasks: Vec<JoinHandle<()>>,
}

impl MemoryGraphReconciliationJoinLeaseV1 {
    fn remove_completed_task(&mut self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(self.tasks.pop());
        state.joining_task_count = self.tasks.len();
    }
}

impl Drop for MemoryGraphReconciliationJoinLeaseV1 {
    fn drop(&mut self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.retired.append(&mut self.tasks);
        state.joining = false;
        state.joining_task_count = 0;
        if state.retired.is_empty() {
            state.running = false;
            state.current_identity = None;
        }
        self.shared.joined.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Weak;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use tokio::sync::Notify;

    use super::*;
    use crate::db::connection::registry::DatabaseInner;

    fn closed_database() -> WeakDatabase {
        WeakDatabase {
            inner: Weak::<DatabaseInner>::new(),
        }
    }

    fn task_owner(
        coordinator: &MemoryGraphReconciliationCoordinatorV1,
    ) -> (MemoryGraphReconciliationTaskOwnerV1, Arc<AtomicBool>) {
        let cancelled = Arc::new(AtomicBool::new(false));
        let task_cancelled = Arc::clone(&cancelled);
        let owner = coordinator.task_owner(
            Arc::new(move || {
                task_cancelled.store(true, Ordering::Release);
                Ok(())
            }),
            Arc::new(|| Ok(())),
        );
        (owner, cancelled)
    }

    async fn wait_until(mut predicate: impl FnMut() -> bool) {
        for _ in 0..256 {
            if predicate() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("memory graph reconciliation task did not reach expected state");
    }

    #[tokio::test]
    async fn cancellation_before_start_refuses_task_admission() {
        let coordinator = MemoryGraphReconciliationCoordinatorV1::default();
        let (owner, cancelled) = task_owner(&coordinator);
        let calls = Arc::new(AtomicUsize::new(0));
        owner.cancel().expect("cancel reconciliation");
        assert!(cancelled.load(Ordering::Acquire));

        let task_calls = Arc::clone(&calls);
        assert_eq!(
            coordinator.schedule_weak(closed_database(), move |_| {
                let task_calls = Arc::clone(&task_calls);
                async move {
                    task_calls.fetch_add(1, Ordering::SeqCst);
                    true
                }
            }),
            MemoryGraphReconciliationTaskScheduleV1::Closed
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        owner.shutdown().await.expect("shutdown closed coordinator");
    }

    #[tokio::test]
    async fn failed_pass_and_concurrent_trigger_have_no_lost_wakeup() {
        let coordinator = MemoryGraphReconciliationCoordinatorV1::default();
        let (owner, _) = task_owner(&coordinator);
        let calls = Arc::new(AtomicUsize::new(0));
        let first_started = Arc::new(Notify::new());
        let release_first = Arc::new(Notify::new());
        let task_calls = Arc::clone(&calls);
        let task_started = Arc::clone(&first_started);
        let task_release = Arc::clone(&release_first);

        assert_eq!(
            coordinator.schedule_weak(closed_database(), move |_| {
                let call = task_calls.fetch_add(1, Ordering::SeqCst);
                let task_started = Arc::clone(&task_started);
                let task_release = Arc::clone(&task_release);
                async move {
                    if call == 0 {
                        task_started.notify_one();
                        task_release.notified().await;
                        return false;
                    }
                    true
                }
            }),
            MemoryGraphReconciliationTaskScheduleV1::Scheduled
        );
        first_started.notified().await;
        assert_eq!(
            coordinator.schedule_weak(closed_database(), |_| async { true }),
            MemoryGraphReconciliationTaskScheduleV1::AlreadyScheduled
        );
        release_first.notify_one();
        wait_until(|| calls.load(Ordering::SeqCst) == 2 && !owner.running()).await;
        assert!(!owner.pending());
        owner.shutdown().await.expect("join reconciler worker");
    }

    #[tokio::test]
    async fn shutdown_observation_counts_join_lease_tasks_until_join_completes() {
        let coordinator = MemoryGraphReconciliationCoordinatorV1::default();
        let (owner, _) = task_owner(&coordinator);
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let task_started = Arc::clone(&started);
        let task_release = Arc::clone(&release);

        assert_eq!(
            coordinator.schedule_weak(closed_database(), move |_| {
                let task_started = Arc::clone(&task_started);
                let task_release = Arc::clone(&task_release);
                async move {
                    task_started.notify_one();
                    task_release.notified().await;
                    true
                }
            }),
            MemoryGraphReconciliationTaskScheduleV1::Scheduled
        );
        started.notified().await;

        let shutdown_owner = owner.clone();
        let shutdown = tokio::spawn(async move { shutdown_owner.shutdown().await });
        wait_until(|| {
            let state = coordinator
                .shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.joining && state.joining_task_count == 1
        })
        .await;
        assert_eq!(coordinator.retained_task_count(), 1);

        release.notify_one();
        shutdown
            .await
            .expect("shutdown task must not panic")
            .expect("shutdown must join retained reconciliation task");
        assert_eq!(coordinator.retained_task_count(), 0);
    }

    #[tokio::test]
    async fn transient_failure_retries_without_a_later_write_trigger() {
        let coordinator = MemoryGraphReconciliationCoordinatorV1::default();
        let (owner, _) = task_owner(&coordinator);
        let calls = Arc::new(AtomicUsize::new(0));
        let succeeded = Arc::new(Notify::new());
        let task_calls = Arc::clone(&calls);
        let task_succeeded = Arc::clone(&succeeded);

        assert_eq!(
            coordinator.schedule_weak(closed_database(), move |_| {
                let call = task_calls.fetch_add(1, Ordering::SeqCst);
                let task_succeeded = Arc::clone(&task_succeeded);
                async move {
                    if call == 0 {
                        false
                    } else {
                        task_succeeded.notify_one();
                        true
                    }
                }
            }),
            MemoryGraphReconciliationTaskScheduleV1::Scheduled
        );
        tokio::time::timeout(Duration::from_secs(1), succeeded.notified())
            .await
            .expect("lifecycle retry must settle without another write");
        wait_until(|| !owner.running()).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(!owner.pending());
        owner.shutdown().await.expect("join reconciler worker");
    }

    #[tokio::test]
    async fn panicked_worker_clears_matching_running_identity() {
        let coordinator = MemoryGraphReconciliationCoordinatorV1::default();
        let (owner, _) = task_owner(&coordinator);
        let operation_started = Arc::new(Notify::new());
        let task_started = Arc::clone(&operation_started);
        assert_eq!(
            coordinator.schedule_weak(closed_database(), move |_| {
                let task_started = Arc::clone(&task_started);
                async move {
                    task_started.notify_one();
                    panic!("forced reconciliation worker failure");
                }
            }),
            MemoryGraphReconciliationTaskScheduleV1::Scheduled
        );
        operation_started.notified().await;
        wait_until(|| !owner.running()).await;
        assert!(owner.shutdown().await.is_err());
        assert!(!owner.running());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn installed_handle_remains_current_across_idle_reschedule_and_shutdown() {
        let coordinator = MemoryGraphReconciliationCoordinatorV1::default();
        let (owner, cancelled) = task_owner(&coordinator);
        let calls = Arc::new(AtomicUsize::new(0));
        let first_started = Arc::new(Notify::new());
        let second_started = Arc::new(Notify::new());
        let block_second = Arc::new(Notify::new());
        let task_calls = Arc::clone(&calls);
        let task_first_started = Arc::clone(&first_started);
        let task_second_started = Arc::clone(&second_started);
        let task_block = Arc::clone(&block_second);

        assert_eq!(
            coordinator.schedule_weak(closed_database(), move |_| {
                let call = task_calls.fetch_add(1, Ordering::SeqCst);
                let task_first_started = Arc::clone(&task_first_started);
                let task_second_started = Arc::clone(&task_second_started);
                let task_block = Arc::clone(&task_block);
                async move {
                    if call == 0 {
                        task_first_started.notify_one();
                    } else if call == 1 {
                        task_second_started.notify_one();
                        task_block.notified().await;
                    }
                    true
                }
            }),
            MemoryGraphReconciliationTaskScheduleV1::Scheduled
        );
        first_started.notified().await;
        wait_until(|| calls.load(Ordering::SeqCst) == 1 && !owner.running()).await;
        assert_eq!(
            coordinator.schedule_weak(closed_database(), |_| async { true }),
            MemoryGraphReconciliationTaskScheduleV1::AlreadyScheduled
        );
        second_started.notified().await;

        owner.cancel().expect("cancel reconciliation");
        assert!(cancelled.load(Ordering::Acquire));
        let shutdown_owner = owner.clone();
        let shutdown = tokio::spawn(async move { shutdown_owner.shutdown().await });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        block_second.notify_one();
        shutdown
            .await
            .expect("join shutdown task")
            .expect("cancel and join current persistent worker");
        assert!(!owner.running());
    }

    #[tokio::test]
    async fn idle_retirement_reservation_fences_new_schedules_and_drop_restores_them() {
        let coordinator = MemoryGraphReconciliationCoordinatorV1::default();
        let (owner, _) = task_owner(&coordinator);

        let reservation = owner
            .reserve_retirement()
            .expect("an idle reconciler can reserve retirement");
        assert_eq!(
            coordinator.schedule_weak(closed_database(), |_| async { true }),
            MemoryGraphReconciliationTaskScheduleV1::Retiring
        );

        drop(reservation);
        assert_eq!(
            coordinator.schedule_weak(closed_database(), |_| async { true }),
            MemoryGraphReconciliationTaskScheduleV1::Scheduled
        );
        owner.shutdown().await.expect("join reconciler worker");
    }

    #[tokio::test]
    async fn active_reconciliation_refuses_retirement_without_mutating_admission() {
        let coordinator = MemoryGraphReconciliationCoordinatorV1::default();
        let (owner, _) = task_owner(&coordinator);
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let task_started = Arc::clone(&started);
        let task_release = Arc::clone(&release);

        assert_eq!(
            coordinator.schedule_weak(closed_database(), move |_| {
                let task_started = Arc::clone(&task_started);
                let task_release = Arc::clone(&task_release);
                async move {
                    task_started.notify_one();
                    task_release.notified().await;
                    true
                }
            }),
            MemoryGraphReconciliationTaskScheduleV1::Scheduled
        );
        started.notified().await;

        assert_eq!(
            owner.reserve_retirement(),
            Err(MemoryGraphReconciliationRetirementBlockerV1::Running)
        );
        assert_eq!(
            coordinator.schedule_weak(closed_database(), |_| async { true }),
            MemoryGraphReconciliationTaskScheduleV1::AlreadyScheduled
        );

        release.notify_one();
        owner.shutdown().await.expect("join reconciler worker");
    }
}
