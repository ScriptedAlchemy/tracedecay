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
    InFlightWeakUpgrade,
    RetainedJoinWork,
    Retiring,
    Closed,
}

/// The verified graph runtime disappeared after the coordinator was attached.
/// This remains typed so a retirement caller cannot mistake a vanished runtime
/// for a completed cancellation and join.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum MemoryGraphReconciliationRuntimeErrorV1 {
    RuntimeUnavailable,
}

/// A direct cancellation denial from the coordinator's own lifecycle state.
/// A reservation remains drop-restorable until its commit linearizes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryGraphReconciliationCancelErrorV1 {
    RuntimeUnavailable,
    RetirementReserved,
}

/// Typed terminal truth for a reconciliation retirement after admission has
/// been irreversibly fenced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryGraphReconciliationRetirementTerminalV1 {
    CancelledAndJoined,
    RuntimeUnavailable,
    WorkerPanicked,
    RuntimeUnavailableAndWorkerPanicked,
    RetainedTaskAborted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryGraphReconciliationRetirementStartErrorV1 {
    RetirementReserved,
    TokioRuntimeUnavailable,
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

/// Receipt for reconciliation retirement after its admission fence became
/// irreversible. The coordinator owns the cancellation and join task, so a
/// requester dropping this receipt cannot strand reconciliation admission
/// without a terminal outcome.
#[must_use]
#[derive(Clone)]
pub(in crate::db) struct MemoryGraphReconciliationRetirementReceiptV1 {
    shared: Arc<MemoryGraphReconciliationSharedV1>,
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
                    usize::from(
                        database
                            .memory_graph_runtime
                            .get()
                            .and_then(std::sync::Weak::upgrade)
                            .is_some(),
                    ),
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
    retirement_task_running: bool,
    retirement_terminal: Option<MemoryGraphReconciliationRetirementTerminalV1>,
    pending: bool,
    running: bool,
    in_flight_weak_upgrades: usize,
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
}

impl MemoryGraphReconciliationCoordinatorV1 {
    pub(super) fn task_owner(
        &self,
        cancel_reconciliation: Arc<
            dyn Fn() -> Result<(), MemoryGraphReconciliationRuntimeErrorV1> + Send + Sync,
        >,
    ) -> MemoryGraphReconciliationTaskOwnerV1 {
        MemoryGraphReconciliationTaskOwnerV1 {
            shared: Arc::clone(&self.shared),
            cancel_reconciliation,
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
        usize::from(state.current.is_some())
            + state.retired.len()
            + state.joining_task_count
            + usize::from(state.retirement_task_running)
    }
}

/// Makes the worker's weak database upgrade interval observable to retirement.
///
/// Installation and removal both hold the coordinator state lock. In
/// particular, aborting or panicking a worker drops this lease and cannot
/// leave a stale weak-upgrade blocker behind. The lease keeps only a weak
/// coordinator reference, so it cannot make an active worker's join handle
/// self-retain the coordinator after its source drops.
struct WeakUpgradeLeaseV1 {
    shared: Weak<MemoryGraphReconciliationSharedV1>,
}

impl WeakUpgradeLeaseV1 {
    fn install(shared: &Arc<MemoryGraphReconciliationSharedV1>) -> Option<Self> {
        let mut state = shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.accepting || !state.running {
            return None;
        }
        state.in_flight_weak_upgrades = state.in_flight_weak_upgrades.saturating_add(1);
        Some(Self {
            shared: Arc::downgrade(shared),
        })
    }
}

impl Drop for WeakUpgradeLeaseV1 {
    fn drop(&mut self) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let mut state = shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.in_flight_weak_upgrades = state.in_flight_weak_upgrades.saturating_sub(1);
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
        if !should_run {
            drop(shared);
            notified.await;
            continue;
        }

        let Some(weak_upgrade) = WeakUpgradeLeaseV1::install(&shared) else {
            return;
        };
        drop(shared);
        let settled = operation(weak_database.clone()).await;
        drop(weak_upgrade);
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
            drop(shared);
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = notified => automatic_failures = 0,
            }
        } else {
            drop(shared);
            notified.await;
            automatic_failures = 0;
        }
    }
}

impl MemoryGraphReconciliationTaskOwnerV1 {
    pub fn same_coordinator(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }

    pub fn cancel(&self) -> Result<(), MemoryGraphReconciliationCancelErrorV1> {
        self.close_admission()?;
        (self.cancel_reconciliation)()
            .map_err(|_| MemoryGraphReconciliationCancelErrorV1::RuntimeUnavailable)
    }

    fn close_admission(&self) -> Result<(), MemoryGraphReconciliationCancelErrorV1> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retirement_reserved {
            return Err(MemoryGraphReconciliationCancelErrorV1::RetirementReserved);
        }
        state.accepting = false;
        state.pending = false;
        self.shared.wake.notify_waiters();
        Ok(())
    }

    /// Fences an idle coordinator before a caller attempts the external graph
    /// and store-runtime retirement reservations. The fence does not cancel a
    /// task or close a graph; that remains deferred to the coordinated commit
    /// boundary.
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
        if state.in_flight_weak_upgrades != 0 {
            return Err(MemoryGraphReconciliationRetirementBlockerV1::InFlightWeakUpgrade);
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

    pub async fn shutdown(
        &self,
    ) -> std::result::Result<
        MemoryGraphReconciliationRetirementTerminalV1,
        MemoryGraphReconciliationRetirementStartErrorV1,
    > {
        Ok(self.start_cancel_and_join(false)?.wait().await)
    }

    fn start_cancel_and_join(
        &self,
        require_retirement_reservation: bool,
    ) -> std::result::Result<
        MemoryGraphReconciliationRetirementReceiptV1,
        MemoryGraphReconciliationRetirementStartErrorV1,
    > {
        let shared = Arc::clone(&self.shared);
        if !require_retirement_reservation
            && shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retirement_reserved
        {
            return Err(MemoryGraphReconciliationRetirementStartErrorV1::RetirementReserved);
        }
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| {
            MemoryGraphReconciliationRetirementStartErrorV1::TokioRuntimeUnavailable
        })?;
        {
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !require_retirement_reservation && state.retirement_reserved {
                return Err(MemoryGraphReconciliationRetirementStartErrorV1::RetirementReserved);
            }
            if state.retirement_task_running || state.retirement_terminal.is_some() {
                return Ok(MemoryGraphReconciliationRetirementReceiptV1 {
                    shared: Arc::clone(&shared),
                });
            }
            state.accepting = false;
            state.pending = false;
            state.retirement_reserved = false;
            state.retirement_task_running = true;
            state.retirement_terminal = None;
            shared.wake.notify_waiters();
        }
        let owner = self.clone();
        let finalizer =
            MemoryGraphReconciliationRetirementTaskFinalizerV1::new(Arc::clone(&shared));
        let task = runtime.spawn(async move {
            let mut finalizer = finalizer;
            finalizer.finish(owner.cancel_and_join().await);
        });
        drop(task);
        Ok(MemoryGraphReconciliationRetirementReceiptV1 { shared })
    }

    async fn cancel_and_join(&self) -> MemoryGraphReconciliationRetirementTerminalV1 {
        let cancellation_error = (self.cancel_reconciliation)().err();
        let joined = self.join_workers().await;
        match (cancellation_error, joined) {
            (None, terminal) => terminal,
            (
                Some(MemoryGraphReconciliationRuntimeErrorV1::RuntimeUnavailable),
                MemoryGraphReconciliationRetirementTerminalV1::CancelledAndJoined,
            ) => MemoryGraphReconciliationRetirementTerminalV1::RuntimeUnavailable,
            (
                Some(MemoryGraphReconciliationRuntimeErrorV1::RuntimeUnavailable),
                MemoryGraphReconciliationRetirementTerminalV1::WorkerPanicked,
            ) => MemoryGraphReconciliationRetirementTerminalV1::RuntimeUnavailableAndWorkerPanicked,
            (Some(MemoryGraphReconciliationRuntimeErrorV1::RuntimeUnavailable), terminal) => {
                terminal
            }
        }
    }

    async fn join_workers(&self) -> MemoryGraphReconciliationRetirementTerminalV1 {
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
            return self.join(tasks).await;
        }
    }

    async fn join(
        &self,
        tasks: Vec<JoinHandle<()>>,
    ) -> MemoryGraphReconciliationRetirementTerminalV1 {
        let mut lease = MemoryGraphReconciliationJoinLeaseV1 {
            shared: Arc::clone(&self.shared),
            tasks,
        };
        let mut worker_panicked = false;
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
                Err(_) => worker_panicked = true,
            }
        }
        drop(lease);
        if worker_panicked {
            MemoryGraphReconciliationRetirementTerminalV1::WorkerPanicked
        } else {
            MemoryGraphReconciliationRetirementTerminalV1::CancelledAndJoined
        }
    }

    pub fn pending(&self) -> bool {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
    }
}

impl MemoryGraphReconciliationRetirementReceiptV1 {
    pub(in crate::db) async fn wait(self) -> MemoryGraphReconciliationRetirementTerminalV1 {
        loop {
            let settled = self.shared.joined.notified();
            let terminal = self
                .shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retirement_terminal;
            if let Some(terminal) = terminal {
                return terminal;
            }
            settled.await;
        }
    }
}

/// Guarantees a terminal receipt even when Tokio aborts the detached
/// cancellation-and-join task during runtime teardown.
struct MemoryGraphReconciliationRetirementTaskFinalizerV1 {
    shared: Arc<MemoryGraphReconciliationSharedV1>,
    finished: bool,
}

impl MemoryGraphReconciliationRetirementTaskFinalizerV1 {
    fn new(shared: Arc<MemoryGraphReconciliationSharedV1>) -> Self {
        Self {
            shared,
            finished: false,
        }
    }

    fn finish(&mut self, terminal: MemoryGraphReconciliationRetirementTerminalV1) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.retirement_task_running = false;
        state.retirement_terminal = Some(terminal);
        self.finished = true;
        self.shared.joined.notify_waiters();
    }
}

impl Drop for MemoryGraphReconciliationRetirementTaskFinalizerV1 {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.retirement_task_running = false;
        state.retirement_terminal =
            Some(MemoryGraphReconciliationRetirementTerminalV1::RetainedTaskAborted);
        self.shared.joined.notify_waiters();
    }
}

impl MemoryGraphReconciliationRetirementReservationV1 {
    /// Linearizes the reconciliation fence and transfers cancellation/joining
    /// into a retained task. Graph retirement remains the graph registry's
    /// sole close authority.
    pub(in crate::db) fn commit(
        mut self,
    ) -> std::result::Result<
        MemoryGraphReconciliationRetirementReceiptV1,
        MemoryGraphReconciliationRetirementStartErrorV1,
    > {
        let receipt = self.owner.start_cancel_and_join(true)?;
        self.armed = false;
        Ok(receipt)
    }

    /// Consumes the reconciliation fence and waits for its typed terminal
    /// state. This is the only public completion boundary; callers cannot
    /// accidentally retain a detached receipt while proceeding to graph or
    /// Store close.
    pub async fn commit_and_wait(
        self,
    ) -> std::result::Result<
        MemoryGraphReconciliationRetirementTerminalV1,
        MemoryGraphReconciliationRetirementStartErrorV1,
    > {
        Ok(self.commit()?.wait().await)
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
    use crate::db::connection::registry::{DatabaseClientLeaseV1, DatabaseInner};

    fn closed_database() -> WeakDatabase {
        WeakDatabase {
            inner: Weak::<DatabaseInner>::new(),
            client: Weak::<DatabaseClientLeaseV1>::new(),
        }
    }

    fn task_owner(
        coordinator: &MemoryGraphReconciliationCoordinatorV1,
    ) -> (MemoryGraphReconciliationTaskOwnerV1, Arc<AtomicBool>) {
        let cancelled = Arc::new(AtomicBool::new(false));
        let task_cancelled = Arc::clone(&cancelled);
        let owner = coordinator.task_owner(Arc::new(move || {
            task_cancelled.store(true, Ordering::Release);
            Ok(())
        }));
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

    fn reconciliation_retirement_is_admissible(
        owner: &MemoryGraphReconciliationTaskOwnerV1,
    ) -> bool {
        match owner.reserve_retirement() {
            Ok(reservation) => {
                drop(reservation);
                true
            }
            Err(_) => false,
        }
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
        assert_eq!(
            owner.shutdown().await.expect("shutdown closed coordinator"),
            MemoryGraphReconciliationRetirementTerminalV1::CancelledAndJoined
        );
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
        wait_until(|| {
            calls.load(Ordering::SeqCst) == 2 && reconciliation_retirement_is_admissible(&owner)
        })
        .await;
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
        assert_eq!(coordinator.retained_task_count(), 2);

        release.notify_one();
        let terminal = shutdown
            .await
            .expect("shutdown task must not panic")
            .expect("shutdown must join retained reconciliation task");
        assert_eq!(
            terminal,
            MemoryGraphReconciliationRetirementTerminalV1::CancelledAndJoined
        );
        assert_eq!(coordinator.retained_task_count(), 0);
    }

    #[tokio::test(start_paused = true)]
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
                    if call < 2 {
                        false
                    } else {
                        task_succeeded.notify_one();
                        true
                    }
                }
            }),
            MemoryGraphReconciliationTaskScheduleV1::Scheduled
        );
        // The first failed pass retries immediately through the wake permit
        // its own schedule stored; the second failure has no permit left, so
        // the worker re-arms itself and parks on its backoff timer. The
        // worker publishes that re-armed state and registers its timer
        // within one poll, so on this single-threaded paused runtime the
        // explicit advances below race with nothing.
        wait_until(|| {
            let state = coordinator
                .shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            calls.load(Ordering::SeqCst) == 2 && state.pending && !state.running
        })
        .await;

        tokio::time::advance(AUTOMATIC_RETRY_BASE - Duration::from_millis(1)).await;
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "automatic retry must wait out its full base backoff"
        );

        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::time::timeout(Duration::from_secs(1), succeeded.notified())
            .await
            .expect("lifecycle retry must settle without another write");
        wait_until(|| reconciliation_retirement_is_admissible(&owner)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 3);
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
        wait_until(|| reconciliation_retirement_is_admissible(&owner)).await;
        assert_eq!(
            owner
                .shutdown()
                .await
                .expect("start typed reconciliation shutdown"),
            MemoryGraphReconciliationRetirementTerminalV1::WorkerPanicked
        );
        assert!(matches!(
            owner.reserve_retirement(),
            Err(MemoryGraphReconciliationRetirementBlockerV1::Closed)
        ));
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
        wait_until(|| {
            calls.load(Ordering::SeqCst) == 1 && reconciliation_retirement_is_admissible(&owner)
        })
        .await;
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
        let terminal = shutdown
            .await
            .expect("join shutdown task")
            .expect("cancel and join current persistent worker");
        assert_eq!(
            terminal,
            MemoryGraphReconciliationRetirementTerminalV1::CancelledAndJoined
        );
        assert!(matches!(
            owner.reserve_retirement(),
            Err(MemoryGraphReconciliationRetirementBlockerV1::Closed)
        ));
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
    async fn reservation_refuses_cancel_and_drop_restores_admission() {
        let coordinator = MemoryGraphReconciliationCoordinatorV1::default();
        let (owner, cancelled) = task_owner(&coordinator);
        let reservation = owner
            .reserve_retirement()
            .expect("idle reconciler retirement reservation");

        assert_eq!(
            owner.cancel(),
            Err(MemoryGraphReconciliationCancelErrorV1::RetirementReserved)
        );
        assert!(
            !cancelled.load(Ordering::Acquire),
            "denied cancellation must not invoke the bound runtime"
        );

        drop(reservation);
        assert_eq!(
            coordinator.schedule_weak(closed_database(), |_| async { true }),
            MemoryGraphReconciliationTaskScheduleV1::Scheduled
        );
        assert_eq!(
            owner
                .shutdown()
                .await
                .expect("shutdown restored coordinator"),
            MemoryGraphReconciliationRetirementTerminalV1::CancelledAndJoined
        );
    }

    #[tokio::test]
    async fn reservation_refuses_shutdown_and_drop_restores_admission() {
        let coordinator = MemoryGraphReconciliationCoordinatorV1::default();
        let (owner, cancelled) = task_owner(&coordinator);
        let reservation = owner
            .reserve_retirement()
            .expect("idle reconciler retirement reservation");

        assert_eq!(
            owner.shutdown().await,
            Err(MemoryGraphReconciliationRetirementStartErrorV1::RetirementReserved)
        );
        assert!(
            !cancelled.load(Ordering::Acquire),
            "denied shutdown must not invoke the bound runtime"
        );

        drop(reservation);
        assert_eq!(
            coordinator.schedule_weak(closed_database(), |_| async { true }),
            MemoryGraphReconciliationTaskScheduleV1::Scheduled
        );
        assert_eq!(
            owner
                .shutdown()
                .await
                .expect("shutdown restored coordinator"),
            MemoryGraphReconciliationRetirementTerminalV1::CancelledAndJoined
        );
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

        assert!(matches!(
            owner.reserve_retirement(),
            Err(MemoryGraphReconciliationRetirementBlockerV1::Running)
        ));
        assert_eq!(
            coordinator.schedule_weak(closed_database(), |_| async { true }),
            MemoryGraphReconciliationTaskScheduleV1::AlreadyScheduled
        );

        release.notify_one();
        owner.shutdown().await.expect("join reconciler worker");
    }

    #[tokio::test]
    async fn aborting_a_worker_releases_its_weak_upgrade_fence() {
        let coordinator = MemoryGraphReconciliationCoordinatorV1::default();
        let (owner, _) = task_owner(&coordinator);
        let started = Arc::new(Notify::new());
        let never = Arc::new(Notify::new());
        let task_started = Arc::clone(&started);
        let task_never = Arc::clone(&never);

        assert_eq!(
            coordinator.schedule_weak(closed_database(), move |_| {
                let task_started = Arc::clone(&task_started);
                let task_never = Arc::clone(&task_never);
                async move {
                    task_started.notify_one();
                    task_never.notified().await;
                    true
                }
            }),
            MemoryGraphReconciliationTaskScheduleV1::Scheduled
        );
        started.notified().await;
        {
            let state = coordinator
                .shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert_eq!(state.in_flight_weak_upgrades, 1);
            state
                .current
                .as_ref()
                .expect("scheduled reconciliation worker")
                .abort();
        }
        wait_until(|| {
            let state = coordinator
                .shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            !state.running && state.in_flight_weak_upgrades == 0
        })
        .await;
        owner
            .shutdown()
            .await
            .expect("join aborted reconciliation worker");
    }

    #[tokio::test]
    async fn idle_worker_does_not_retain_its_coordinator_after_the_source_drops() {
        let coordinator = MemoryGraphReconciliationCoordinatorV1::default();
        let (owner, _) = task_owner(&coordinator);
        let weak_shared = Arc::downgrade(&coordinator.shared);
        let completed = Arc::new(Notify::new());
        let task_completed = Arc::clone(&completed);
        assert_eq!(
            coordinator.schedule_weak(closed_database(), move |_| {
                let task_completed = Arc::clone(&task_completed);
                async move {
                    task_completed.notify_one();
                    true
                }
            }),
            MemoryGraphReconciliationTaskScheduleV1::Scheduled
        );
        completed.notified().await;
        wait_until(|| reconciliation_retirement_is_admissible(&owner)).await;

        drop(owner);
        drop(coordinator);
        wait_until(|| weak_shared.upgrade().is_none()).await;
    }

    #[tokio::test]
    async fn active_weak_upgrade_does_not_self_retain_its_coordinator() {
        let coordinator = MemoryGraphReconciliationCoordinatorV1::default();
        let weak_shared = Arc::downgrade(&coordinator.shared);
        let started = Arc::new(Notify::new());
        let never = Arc::new(Notify::new());
        let task_started = Arc::clone(&started);
        let task_never = Arc::clone(&never);
        assert_eq!(
            coordinator.schedule_weak(closed_database(), move |_| {
                let task_started = Arc::clone(&task_started);
                let task_never = Arc::clone(&task_never);
                async move {
                    task_started.notify_one();
                    task_never.notified().await;
                    true
                }
            }),
            MemoryGraphReconciliationTaskScheduleV1::Scheduled
        );
        started.notified().await;

        drop(coordinator);
        wait_until(|| weak_shared.upgrade().is_none()).await;
    }

    #[tokio::test]
    async fn retry_wait_does_not_retain_its_coordinator_after_the_source_drops() {
        let coordinator = MemoryGraphReconciliationCoordinatorV1::default();
        let weak_shared = Arc::downgrade(&coordinator.shared);
        let completed = Arc::new(Notify::new());
        let task_completed = Arc::clone(&completed);
        assert_eq!(
            coordinator.schedule_weak(closed_database(), move |_| {
                let task_completed = Arc::clone(&task_completed);
                async move {
                    task_completed.notify_one();
                    false
                }
            }),
            MemoryGraphReconciliationTaskScheduleV1::Scheduled
        );
        completed.notified().await;
        wait_until(|| {
            let state = coordinator
                .shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.pending && !state.running
        })
        .await;

        drop(coordinator);
        wait_until(|| weak_shared.upgrade().is_none()).await;
    }

    #[tokio::test]
    async fn committed_retirement_settles_after_the_requester_drops_its_receipt() {
        let coordinator = MemoryGraphReconciliationCoordinatorV1::default();
        let (owner, cancelled) = task_owner(&coordinator);
        let reservation = owner
            .reserve_retirement()
            .expect("idle reconciler retirement reservation");
        let receipt = reservation
            .commit()
            .expect("linearize reconciliation retirement");
        let retained_receipt = receipt.clone();
        drop(receipt);

        assert_eq!(
            retained_receipt.wait().await,
            MemoryGraphReconciliationRetirementTerminalV1::CancelledAndJoined
        );
        assert!(cancelled.load(Ordering::Acquire));
        assert_eq!(
            coordinator.schedule_weak(closed_database(), |_| async { true }),
            MemoryGraphReconciliationTaskScheduleV1::Closed
        );
    }

    #[tokio::test]
    async fn retirement_commit_and_wait_returns_the_terminal_state() {
        let coordinator = MemoryGraphReconciliationCoordinatorV1::default();
        let (owner, cancelled) = task_owner(&coordinator);

        assert_eq!(
            owner
                .reserve_retirement()
                .expect("idle reconciler retirement reservation")
                .commit_and_wait()
                .await
                .expect("commit and wait for reconciliation retirement"),
            MemoryGraphReconciliationRetirementTerminalV1::CancelledAndJoined
        );
        assert!(cancelled.load(Ordering::Acquire));
        assert_eq!(
            coordinator.schedule_weak(closed_database(), |_| async { true }),
            MemoryGraphReconciliationTaskScheduleV1::Closed
        );
    }

    #[tokio::test]
    async fn retirement_receipt_reports_runtime_unavailable_without_erasing_it() {
        let coordinator = MemoryGraphReconciliationCoordinatorV1::default();
        let owner = coordinator.task_owner(Arc::new(|| {
            Err(MemoryGraphReconciliationRuntimeErrorV1::RuntimeUnavailable)
        }));
        let receipt = owner
            .reserve_retirement()
            .expect("idle reconciler retirement reservation")
            .commit()
            .expect("linearize reconciliation retirement");

        assert_eq!(
            receipt.wait().await,
            MemoryGraphReconciliationRetirementTerminalV1::RuntimeUnavailable
        );
    }

    #[test]
    fn dropping_an_undriven_runtime_records_a_typed_retirement_terminal() {
        let coordinator = MemoryGraphReconciliationCoordinatorV1::default();
        let (owner, _) = task_owner(&coordinator);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build undriven test runtime");
        let receipt = {
            let _entered = runtime.enter();
            owner
                .reserve_retirement()
                .expect("idle reconciler retirement reservation")
                .commit()
                .expect("linearize reconciliation retirement")
        };
        drop(runtime);
        let observer = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build terminal observer runtime");

        assert_eq!(
            observer.block_on(receipt.wait()),
            MemoryGraphReconciliationRetirementTerminalV1::RetainedTaskAborted
        );
    }
}
