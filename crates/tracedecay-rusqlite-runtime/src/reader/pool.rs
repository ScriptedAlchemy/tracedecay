use std::{
    collections::{BTreeMap, VecDeque},
    error::Error,
    fmt,
    sync::{
        Arc, Condvar, Mutex,
        mpsc::{Receiver, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use tracedecay_store::{
    OperationPriorityV1, ReaderBudgetV1, RuntimeInterruptionV1, RuntimeReadOutcomeV1,
    RuntimeReadRequestV1, RuntimeRequestProbeV1, SaturationScopeV1, StorageRuntimeContractErrorV1,
    StoreRuntimeBindingV1, UnavailableReasonV1,
};

use super::{
    ExistingReaderLocator, ReaderQueryExecutor, ReaderStartError, ReaderWorkerError,
    unavailable_read, worker,
};

const ACQUISITION_POLL_QUANTUM: Duration = Duration::from_millis(5);
const SNAPSHOT_END_GRACE: Duration = Duration::from_millis(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReaderLane {
    General,
    ReservedHealth,
}

impl ReaderLane {
    fn for_priority(priority: OperationPriorityV1) -> Self {
        match priority {
            OperationPriorityV1::Health => Self::ReservedHealth,
            OperationPriorityV1::Foreground | OperationPriorityV1::Background => Self::General,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReaderPoolState {
    Ready,
    Draining,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReaderPoolSnapshot {
    pub state: ReaderPoolState,
    pub general_workers: u16,
    pub available_general: u16,
    pub health_workers: u16,
    pub available_health: u16,
    pub leased_general: u16,
    pub leased_health: u16,
}

#[derive(Debug)]
pub enum ReaderAcquireError {
    InvalidRequest(StorageRuntimeContractErrorV1),
    ProbeBindingMismatch { field: &'static str },
    BindingMismatch,
    Interrupted { reason: UnavailableReasonV1 },
    Saturated { scope: SaturationScopeV1 },
    WorkerStart(ReaderStartError),
    Worker(ReaderWorkerError),
}

impl fmt::Display for ReaderAcquireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(error) => write!(f, "invalid reader request: {error}"),
            Self::ProbeBindingMismatch { field } => {
                write!(f, "reader probe does not match {field}")
            }
            Self::BindingMismatch => f.write_str("reader request does not bind to this pool"),
            Self::Interrupted { reason } => write!(f, "reader acquisition interrupted: {reason:?}"),
            Self::Saturated { scope } => write!(f, "reader acquisition saturated: {scope:?}"),
            Self::WorkerStart(error) => write!(f, "reader burst worker failed to start: {error}"),
            Self::Worker(error) => write!(f, "reader worker failed: {error}"),
        }
    }
}

impl Error for ReaderAcquireError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidRequest(error) => Some(error),
            Self::WorkerStart(error) => Some(error),
            Self::Worker(error) => Some(error),
            _ => None,
        }
    }
}

struct WorkerRecord {
    client: worker::WorkerClient,
    join: Option<JoinHandle<()>>,
    lane: ReaderLane,
}

#[derive(Clone)]
struct AvailableWorker {
    id: u64,
    client: worker::WorkerClient,
    idle_since: Instant,
}

struct PoolState {
    lifecycle: ReaderPoolState,
    next_id: u64,
    opening_general: u16,
    records: BTreeMap<u64, WorkerRecord>,
    general: VecDeque<AvailableWorker>,
    health: VecDeque<AvailableWorker>,
    leased_general: u16,
    leased_health: u16,
}

impl PoolState {
    fn workers(&self, lane: ReaderLane) -> u16 {
        self.records
            .values()
            .filter(|record| record.lane == lane)
            .count() as u16
    }

    fn available(&mut self, lane: ReaderLane) -> &mut VecDeque<AvailableWorker> {
        match lane {
            ReaderLane::General => &mut self.general,
            ReaderLane::ReservedHealth => &mut self.health,
        }
    }
}

struct PoolInner<E: ReaderQueryExecutor> {
    binding: StoreRuntimeBindingV1,
    locator: ExistingReaderLocator,
    budget: ReaderBudgetV1,
    idle_burst_retire: Duration,
    executor: E,
    state: Mutex<PoolState>,
    capacity_changed: Condvar,
}

impl<E: ReaderQueryExecutor> Drop for PoolInner<E> {
    fn drop(&mut self) {
        let records = {
            let state = self
                .state
                .get_mut()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut state.records)
        };
        for record in records.values() {
            record.client.shutdown();
        }
        for mut record in records.into_values() {
            if let Some(join) = record.join.take() {
                let _ = join.join();
            }
        }
    }
}

/// Per-hot-shard reader façade. General readers scale from the contract's 2-8
/// budget; one separately-accounted health worker remains available even when
/// every general reader is leased.
pub struct ReaderPool<E: ReaderQueryExecutor> {
    inner: Arc<PoolInner<E>>,
}

impl<E: ReaderQueryExecutor> Clone for ReaderPool<E> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<E: ReaderQueryExecutor> ReaderPool<E> {
    pub fn start(
        locator: ExistingReaderLocator,
        budget: ReaderBudgetV1,
        executor: E,
    ) -> Result<Self, ReaderStartError> {
        budget
            .validate()
            .map_err(ReaderStartError::InvalidReaderBudget)?;
        let inner = Arc::new(PoolInner {
            binding: locator.binding().clone(),
            locator,
            idle_burst_retire: Duration::from_millis(budget.idle_burst_retire_ms),
            budget,
            executor,
            state: Mutex::new(PoolState {
                lifecycle: ReaderPoolState::Ready,
                next_id: 1,
                opening_general: 0,
                records: BTreeMap::new(),
                general: VecDeque::new(),
                health: VecDeque::new(),
                leased_general: 0,
                leased_health: 0,
            }),
            capacity_changed: Condvar::new(),
        });
        let pool = Self { inner };
        for _ in 0..pool.inner.budget.min_per_hot_shard {
            pool.add_idle_worker(ReaderLane::General)?;
        }
        pool.add_idle_worker(ReaderLane::ReservedHealth)?;
        Ok(pool)
    }

    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.inner.binding
    }

    pub fn snapshot(&self) -> ReaderPoolSnapshot {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ReaderPoolSnapshot {
            state: state.lifecycle,
            general_workers: state.workers(ReaderLane::General),
            available_general: state.general.len() as u16,
            health_workers: state.workers(ReaderLane::ReservedHealth),
            available_health: state.health.len() as u16,
            leased_general: state.leased_general,
            leased_health: state.leased_health,
        }
    }

    /// Stop general admission and wake every waiter. Already-leased workers
    /// continue until their RAII lease ends. The independently-accounted
    /// health lane remains available for drain/health policy checks.
    pub fn begin_drain(&self) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.lifecycle == ReaderPoolState::Ready {
            state.lifecycle = ReaderPoolState::Draining;
            drop(state);
            self.inner.capacity_changed.notify_all();
        }
    }

    /// Acquire within a caller-selected bound. The caller-owned probe remains
    /// the sole cancellation/deadline authority and is checked before every
    /// capacity decision and bounded condvar wait.
    pub fn acquire(
        &self,
        request: &RuntimeReadRequestV1,
        probe: &dyn RuntimeRequestProbeV1,
        max_wait: Duration,
    ) -> Result<ReaderLease<E>, ReaderAcquireError> {
        request
            .validate()
            .map_err(ReaderAcquireError::InvalidRequest)?;
        if request.binding() != &self.inner.binding {
            return Err(ReaderAcquireError::BindingMismatch);
        }
        validate_probe(request, probe)?;
        let lane = ReaderLane::for_priority(request.priority());
        let started = Instant::now();

        loop {
            if let Some(reason) = interruption(probe) {
                return Err(ReaderAcquireError::Interrupted { reason });
            }
            self.retire_idle_at(Instant::now());
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.lifecycle == ReaderPoolState::Draining && lane == ReaderLane::General {
                return Err(ReaderAcquireError::Interrupted {
                    reason: UnavailableReasonV1::Draining,
                });
            }
            if let Some(worker) = state.available(lane).pop_front() {
                match lane {
                    ReaderLane::General => state.leased_general += 1,
                    ReaderLane::ReservedHealth => state.leased_health += 1,
                }
                drop(state);
                return Ok(ReaderLease {
                    checkout: Checkout {
                        inner: Arc::clone(&self.inner),
                        lane,
                        worker,
                        deferred_end: None,
                        retire: false,
                    },
                    snapshot_active: false,
                });
            }
            if lane == ReaderLane::General
                && state.workers(lane).saturating_add(state.opening_general)
                    < self.inner.budget.max_per_hot_shard
            {
                state.opening_general += 1;
                drop(state);
                let spawned =
                    worker::spawn(self.inner.locator.clone(), self.inner.executor.clone());
                let mut state = self
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.opening_general -= 1;
                let spawned = spawned.map_err(ReaderAcquireError::WorkerStart)?;
                let id = state.next_id;
                state.next_id += 1;
                let client = spawned.client.clone();
                state.records.insert(
                    id,
                    WorkerRecord {
                        client: spawned.client,
                        join: Some(spawned.join),
                        lane,
                    },
                );
                if state.lifecycle == ReaderPoolState::Draining {
                    state.general.push_back(AvailableWorker {
                        id,
                        client,
                        idle_since: Instant::now(),
                    });
                    drop(state);
                    self.inner.capacity_changed.notify_all();
                    return Err(ReaderAcquireError::Interrupted {
                        reason: UnavailableReasonV1::Draining,
                    });
                }
                state.leased_general += 1;
                drop(state);
                return Ok(ReaderLease {
                    checkout: Checkout {
                        inner: Arc::clone(&self.inner),
                        lane,
                        worker: AvailableWorker {
                            id,
                            client,
                            idle_since: Instant::now(),
                        },
                        deferred_end: None,
                        retire: false,
                    },
                    snapshot_active: false,
                });
            }
            let elapsed = started.elapsed();
            if elapsed >= max_wait {
                return Err(ReaderAcquireError::Saturated {
                    scope: SaturationScopeV1::ReaderPool,
                });
            }
            let wait = (max_wait - elapsed).min(ACQUISITION_POLL_QUANTUM);
            let (_state, _) = self
                .inner
                .capacity_changed
                .wait_timeout(state, wait)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Opportunistically retire burst workers. This method performs no sleep;
    /// tests and maintenance callers can supply a deterministic monotonic time.
    pub fn retire_idle_at(&self, now: Instant) -> usize {
        let mut retired = Vec::new();
        {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut active = state.workers(ReaderLane::General);
            let minimum = self.inner.budget.min_per_hot_shard;
            let retire_after = self.inner.idle_burst_retire;
            let mut kept = VecDeque::new();
            while let Some(worker) = state.general.pop_front() {
                let idle = now
                    .checked_duration_since(worker.idle_since)
                    .unwrap_or_default();
                if active > minimum && idle >= retire_after {
                    active -= 1;
                    if let Some(record) = state.records.remove(&worker.id) {
                        retired.push(record);
                    }
                } else {
                    kept.push_back(worker);
                }
            }
            state.general = kept;
        }
        for record in &retired {
            record.client.shutdown();
        }
        for record in &mut retired {
            if let Some(join) = record.join.take() {
                let _ = join.join();
            }
        }
        retired.len()
    }

    fn add_idle_worker(&self, lane: ReaderLane) -> Result<(), ReaderStartError> {
        let spawned = worker::spawn(self.inner.locator.clone(), self.inner.executor.clone())?;
        let now = Instant::now();
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = state.next_id;
        state.next_id += 1;
        let client = spawned.client.clone();
        state.records.insert(
            id,
            WorkerRecord {
                client: spawned.client,
                join: Some(spawned.join),
                lane,
            },
        );
        state.available(lane).push_back(AvailableWorker {
            id,
            client,
            idle_since: now,
        });
        Ok(())
    }
}

struct Checkout<E: ReaderQueryExecutor> {
    inner: Arc<PoolInner<E>>,
    lane: ReaderLane,
    worker: AvailableWorker,
    deferred_end: Option<Receiver<Result<(), ReaderWorkerError>>>,
    retire: bool,
}

impl<E: ReaderQueryExecutor> Drop for Checkout<E> {
    fn drop(&mut self) {
        let deferred_end = self.deferred_end.take();
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let retired = if self.retire {
            state.records.remove(&self.worker.id)
        } else {
            None
        };
        if deferred_end.is_none() && retired.is_none() {
            self.worker.idle_since = Instant::now();
            state.available(self.lane).push_back(self.worker.clone());
        }
        match self.lane {
            ReaderLane::General => state.leased_general -= 1,
            ReaderLane::ReservedHealth => state.leased_health -= 1,
        }
        drop(state);
        self.inner.capacity_changed.notify_all();

        if let Some(mut record) = retired {
            record.client.shutdown();
            if let Some(join) = record.join.take() {
                let _ = join.join();
            }
        }
        if let Some(receive) = deferred_end {
            let inner = Arc::clone(&self.inner);
            let lane = self.lane;
            let worker = self.worker.clone();
            let _ = thread::Builder::new()
                .name("tracedecay-rusqlite-reader-return".to_owned())
                .spawn(move || finish_deferred_return(inner, lane, worker, receive));
        }
    }
}

/// RAII ownership of one pool worker. Dropping it always returns the worker to
/// the correct independently-accounted lane.
pub struct ReaderLease<E: ReaderQueryExecutor> {
    checkout: Checkout<E>,
    snapshot_active: bool,
}

impl<E: ReaderQueryExecutor> ReaderLease<E> {
    pub fn begin_snapshot(&mut self) -> Result<SnapshotLease<'_, E>, ReaderWorkerError> {
        if self.snapshot_active {
            return Err(ReaderWorkerError::SnapshotAlreadyActive);
        }
        self.checkout.worker.client.begin()?;
        self.snapshot_active = true;
        Ok(SnapshotLease { lease: self })
    }

    pub(crate) fn begin_pinned_snapshot(
        &mut self,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> Result<(), ReaderAcquireError> {
        if self.snapshot_active {
            return Err(ReaderAcquireError::Worker(
                ReaderWorkerError::SnapshotAlreadyActive,
            ));
        }
        self.checkout
            .worker
            .client
            .begin()
            .map_err(ReaderAcquireError::Worker)?;
        self.snapshot_active = true;
        self.checkout
            .worker
            .client
            .pin(probe)
            .map_err(map_worker_error)
    }

    pub(crate) fn execute_active_raw(
        &mut self,
        request: RuntimeReadRequestV1,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> Result<RuntimeReadOutcomeV1, ReaderAcquireError> {
        request
            .validate()
            .map_err(ReaderAcquireError::InvalidRequest)?;
        if request.binding() != &self.checkout.inner.binding {
            return Err(ReaderAcquireError::BindingMismatch);
        }
        validate_probe(&request, probe)?;
        if !self.snapshot_active {
            return Err(ReaderAcquireError::Worker(
                ReaderWorkerError::SnapshotNotActive,
            ));
        }
        if let Some(reason) = interruption(probe) {
            return Err(ReaderAcquireError::Interrupted { reason });
        }
        self.checkout
            .worker
            .client
            .execute(request, probe)
            .map_err(map_worker_error)
    }

    fn execute_active(
        &mut self,
        request: RuntimeReadRequestV1,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> Result<RuntimeReadOutcomeV1, ReaderAcquireError> {
        let outcome = match self.execute_active_raw(request.clone(), probe) {
            Ok(outcome) => outcome,
            Err(ReaderAcquireError::Interrupted { reason }) => {
                return unavailable_read(reason).map_err(|error| {
                    ReaderAcquireError::Worker(ReaderWorkerError::Storage(error))
                });
            }
            Err(error) => return Err(error),
        };
        super::validate_outcome(&request, outcome)
            .map_err(|error| ReaderAcquireError::Worker(ReaderWorkerError::Storage(error)))
    }

    fn finish_snapshot(&mut self) {
        if !self.snapshot_active {
            return;
        }
        self.snapshot_active = false;
        let receive = match self.checkout.worker.client.begin_end() {
            Ok(receive) => receive,
            Err(_) => {
                self.checkout.retire = true;
                return;
            }
        };
        match receive.recv_timeout(SNAPSHOT_END_GRACE) {
            Ok(Ok(())) => {}
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => {
                self.checkout.retire = true;
            }
            Err(RecvTimeoutError::Timeout) => {
                self.checkout.deferred_end = Some(receive);
            }
        }
    }
}

impl<E: ReaderQueryExecutor> Drop for ReaderLease<E> {
    fn drop(&mut self) {
        self.finish_snapshot();
    }
}

/// RAII deferred read transaction. Its first typed query establishes SQLite's
/// snapshot; subsequent queries on this lease observe the same committed view.
pub struct SnapshotLease<'a, E: ReaderQueryExecutor> {
    lease: &'a mut ReaderLease<E>,
}

impl<E: ReaderQueryExecutor> SnapshotLease<'_, E> {
    pub fn execute(
        &mut self,
        request: RuntimeReadRequestV1,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> Result<RuntimeReadOutcomeV1, ReaderAcquireError> {
        self.lease.execute_active(request, probe)
    }
}

impl<E: ReaderQueryExecutor> Drop for SnapshotLease<'_, E> {
    fn drop(&mut self) {
        self.lease.finish_snapshot();
    }
}

fn finish_deferred_return<E: ReaderQueryExecutor>(
    inner: Arc<PoolInner<E>>,
    lane: ReaderLane,
    mut worker: AvailableWorker,
    receive: Receiver<Result<(), ReaderWorkerError>>,
) {
    if matches!(receive.recv(), Ok(Ok(()))) {
        worker.idle_since = Instant::now();
        inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .available(lane)
            .push_back(worker);
        inner.capacity_changed.notify_all();
        return;
    }

    let record = inner
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .records
        .remove(&worker.id);
    if let Some(mut record) = record {
        record.client.shutdown();
        if let Some(join) = record.join.take() {
            let _ = join.join();
        }
    }
    inner.capacity_changed.notify_all();
}

fn map_worker_error(error: ReaderWorkerError) -> ReaderAcquireError {
    match error {
        ReaderWorkerError::Interrupted { reason } => ReaderAcquireError::Interrupted { reason },
        error => ReaderAcquireError::Worker(error),
    }
}

fn validate_probe(
    request: &RuntimeReadRequestV1,
    probe: &dyn RuntimeRequestProbeV1,
) -> Result<(), ReaderAcquireError> {
    if probe.cancellation_identity() != &request.control().cancellation {
        return Err(ReaderAcquireError::ProbeBindingMismatch {
            field: "cancellation identity",
        });
    }
    if probe.deadline_identity() != &request.control().deadline {
        return Err(ReaderAcquireError::ProbeBindingMismatch {
            field: "deadline identity",
        });
    }
    Ok(())
}

fn interruption(probe: &dyn RuntimeRequestProbeV1) -> Option<UnavailableReasonV1> {
    match probe.interruption() {
        Some(RuntimeInterruptionV1::Cancelled) => Some(UnavailableReasonV1::Cancelled),
        Some(RuntimeInterruptionV1::DeadlineExceeded) => {
            Some(UnavailableReasonV1::DeadlineExceeded)
        }
        None => None,
    }
}
