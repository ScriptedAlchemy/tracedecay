//! The per-hot-shard reader worker pool.
//!
//! This module owns capacity: how many workers exist per lane, who is holding
//! one, and when an idle one retires. The siblings own the two things that hang
//! off it — [`lease`] the RAII checkout that always returns a worker, and
//! [`outcome`] the result vocabulary an acquisition reports in.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Condvar, Mutex, Weak},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use tokio::sync::watch;
use tracedecay_store::{
    OperationPriorityV1, ReaderBudgetV1, RuntimeReadRequestV1, RuntimeRequestProbeV1,
    SaturationScopeV1, StoreRuntimeBindingV1, UnavailableReasonV1,
};

use super::{ExistingReaderLocator, ReaderQueryExecutor, ReaderStartError, worker};
use crate::CheckpointPressure;
use crate::migration_sql::{
    MigrationSqlError, MigrationSqlReadSnapshot, MigrationSqlRows, MigrationSqlStatement,
};

mod lease;
mod outcome;

pub use lease::{ReaderLease, SnapshotLease};
pub use outcome::{ReaderAcquireError, ReaderPoolSnapshot, ReaderPoolState};
use outcome::{interruption, validate_probe};

pub(super) const ACQUISITION_POLL_QUANTUM: Duration = Duration::from_millis(5);
pub(super) const SNAPSHOT_END_GRACE: Duration = Duration::from_millis(5);

/// How long a worker that outran [`SNAPSHOT_END_GRACE`] has to confirm its
/// rollback before the pool writes it off and replaces it.
///
/// This must stay comfortably below the attachment drain timeout (5s): a
/// shutdown that starts while a worker is in limbo has to be able to wait the
/// limbo out and still converge.
pub(super) const DEFERRED_SNAPSHOT_END_LIMIT: Duration = Duration::from_secs(2);

/// General-lane workers reachable only by interactive acquisitions.
///
/// Foreground and background reads share one lane of workers, so without a
/// reservation a bulk sweep that opens `max_per_hot_shard` concurrent reads
/// occupies the lane completely and every interactive read waits out its
/// deadline and reports `Saturated`. Background acquisitions therefore admit
/// against `max_per_hot_shard` minus this reservation.
pub(super) const FOREGROUND_RESERVED_GENERAL_WORKERS: u16 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReaderLane {
    General,
    ReservedHealth,
}

/// Which lane an acquisition enters, and whether it admits against the
/// reserved-interactive slice of that lane or only the unreserved remainder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LaneAdmission {
    lane: ReaderLane,
    background: bool,
}

impl LaneAdmission {
    fn for_priority(priority: OperationPriorityV1) -> Self {
        match priority {
            OperationPriorityV1::Health => Self {
                lane: ReaderLane::ReservedHealth,
                background: false,
            },
            OperationPriorityV1::Foreground => Self {
                lane: ReaderLane::General,
                background: false,
            },
            OperationPriorityV1::Background => Self {
                lane: ReaderLane::General,
                background: true,
            },
        }
    }

    const fn interactive(lane: ReaderLane) -> Self {
        Self {
            lane,
            background: false,
        }
    }
}

pub(super) struct WorkerRecord {
    pub(super) client: worker::WorkerClient,
    pub(super) join: Option<JoinHandle<()>>,
    lane: ReaderLane,
}

#[derive(Clone)]
pub(super) struct AvailableWorker {
    pub(super) id: u64,
    pub(super) client: worker::WorkerClient,
    pub(super) idle_since: Instant,
}

pub(super) struct PoolState {
    lifecycle: ReaderPoolState,
    health_admission_open: bool,
    next_id: u64,
    opening_general: u16,
    opening_health: u16,
    pub(super) records: BTreeMap<u64, WorkerRecord>,
    general: VecDeque<AvailableWorker>,
    health: VecDeque<AvailableWorker>,
    pub(super) leased_general: u16,
    pub(super) leased_health: u16,
    /// Workers whose snapshot end outran [`SNAPSHOT_END_GRACE`].
    ///
    /// Their lease has ended but the worker has not confirmed its rollback, so
    /// it is neither available nor leased. It is still counted here — a limbo
    /// worker that vanished from the accounting would silently shrink the lane
    /// and let a shutdown declare quiescence with work still in flight.
    pub(super) limbo_general: u16,
    pub(super) limbo_health: u16,
    /// Acquisitions currently blocked waiting for capacity in each lane.
    ///
    /// Occupancy alone cannot distinguish a lane that is merely busy from one
    /// that is turning callers away: a full lane with no waiters is working,
    /// a full lane with waiters is the saturation users report.
    pub(super) waiting_general: u16,
    pub(super) waiting_health: u16,
}

impl PoolState {
    fn workers(&self, lane: ReaderLane) -> u16 {
        self.records
            .values()
            .filter(|record| record.lane == lane)
            .count() as u16
    }

    /// Workers this lane can actually hand out: its records minus the ones
    /// stuck finishing a snapshot. Excluding limbo lets the lane spawn a
    /// replacement instead of running degraded until the straggler resolves.
    fn serviceable_workers(&self, lane: ReaderLane) -> u16 {
        self.workers(lane).saturating_sub(self.limbo(lane))
    }

    pub(super) const fn limbo(&self, lane: ReaderLane) -> u16 {
        match lane {
            ReaderLane::General => self.limbo_general,
            ReaderLane::ReservedHealth => self.limbo_health,
        }
    }

    pub(super) const fn limbo_mut(&mut self, lane: ReaderLane) -> &mut u16 {
        match lane {
            ReaderLane::General => &mut self.limbo_general,
            ReaderLane::ReservedHealth => &mut self.limbo_health,
        }
    }

    const fn waiting(&self, lane: ReaderLane) -> u16 {
        match lane {
            ReaderLane::General => self.waiting_general,
            ReaderLane::ReservedHealth => self.waiting_health,
        }
    }

    const fn waiting_mut(&mut self, lane: ReaderLane) -> &mut u16 {
        match lane {
            ReaderLane::General => &mut self.waiting_general,
            ReaderLane::ReservedHealth => &mut self.waiting_health,
        }
    }

    pub(super) fn available(&mut self, lane: ReaderLane) -> &mut VecDeque<AvailableWorker> {
        match lane {
            ReaderLane::General => &mut self.general,
            ReaderLane::ReservedHealth => &mut self.health,
        }
    }

    fn opening(&self, lane: ReaderLane) -> u16 {
        match lane {
            ReaderLane::General => self.opening_general,
            ReaderLane::ReservedHealth => self.opening_health,
        }
    }

    fn opening_mut(&mut self, lane: ReaderLane) -> &mut u16 {
        match lane {
            ReaderLane::General => &mut self.opening_general,
            ReaderLane::ReservedHealth => &mut self.opening_health,
        }
    }

    fn leased_mut(&mut self, lane: ReaderLane) -> &mut u16 {
        match lane {
            ReaderLane::General => &mut self.leased_general,
            ReaderLane::ReservedHealth => &mut self.leased_health,
        }
    }
}

/// Counts one acquisition as a waiter for as long as it is blocked.
///
/// The count is armed the first time the acquisition has to wait and released
/// on every exit path, including the interrupted and saturated ones. Declaring
/// it before the state guard inside the loop means the guard is always dropped
/// first, so re-locking here can never deadlock.
struct WaitingGuard<'pool, E: ReaderQueryExecutor> {
    inner: &'pool PoolInner<E>,
    lane: ReaderLane,
    counted: bool,
}

impl<'pool, E: ReaderQueryExecutor> WaitingGuard<'pool, E> {
    const fn new(inner: &'pool PoolInner<E>, lane: ReaderLane) -> Self {
        Self {
            inner,
            lane,
            counted: false,
        }
    }

    const fn arm(&mut self, state: &mut PoolState) {
        if !self.counted {
            self.counted = true;
            *state.waiting_mut(self.lane) += 1;
        }
    }
}

impl<E: ReaderQueryExecutor> Drop for WaitingGuard<'_, E> {
    fn drop(&mut self) {
        if !self.counted {
            return;
        }
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *state.waiting_mut(self.lane) = state.waiting(self.lane).saturating_sub(1);
    }
}

pub(super) struct PoolInner<E: ReaderQueryExecutor> {
    pub(super) binding: StoreRuntimeBindingV1,
    locator: ExistingReaderLocator,
    budget: ReaderBudgetV1,
    idle_burst_retire: Duration,
    executor: E,
    checkpoint_pressure: Option<watch::Receiver<CheckpointPressure>>,
    pub(super) state: Mutex<PoolState>,
    pub(super) capacity_changed: Condvar,
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

pub(crate) struct WeakReaderPool<E: ReaderQueryExecutor> {
    inner: Weak<PoolInner<E>>,
}

impl<E: ReaderQueryExecutor> Clone for WeakReaderPool<E> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<E: ReaderQueryExecutor> WeakReaderPool<E> {
    pub(crate) fn upgrade(&self) -> Option<ReaderPool<E>> {
        self.inner.upgrade().map(|inner| ReaderPool { inner })
    }
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
        Self::start_with_checkpoint_pressure(locator, budget, executor, None)
    }

    pub(crate) fn start_with_checkpoint_pressure(
        locator: ExistingReaderLocator,
        budget: ReaderBudgetV1,
        executor: E,
        checkpoint_pressure: Option<watch::Receiver<CheckpointPressure>>,
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
            checkpoint_pressure,
            state: Mutex::new(PoolState {
                lifecycle: ReaderPoolState::Ready,
                health_admission_open: true,
                next_id: 1,
                opening_general: 0,
                opening_health: 0,
                records: BTreeMap::new(),
                general: VecDeque::new(),
                health: VecDeque::new(),
                leased_general: 0,
                leased_health: 0,
                limbo_general: 0,
                limbo_health: 0,
                waiting_general: 0,
                waiting_health: 0,
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

    pub(crate) fn verified_locator(&self) -> &tracedecay_store::VerifiedStoreLocatorV1 {
        self.inner.locator.verified_locator()
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        self.inner.locator.path()
    }

    pub(crate) fn opened_file_identity(&self) -> Option<u64> {
        self.inner.locator.expected_file_identity()
    }

    pub(crate) fn downgrade(&self) -> WeakReaderPool<E> {
        WeakReaderPool {
            inner: Arc::downgrade(&self.inner),
        }
    }

    /// Run one migration-SQL query under the caller's declared priority.
    ///
    /// The priority is the caller's, not a pool default: a bulk sweep that
    /// declares `Background` admits against the unreserved slice of the
    /// general lane and cannot displace interactive reads.
    pub(crate) fn execute_migration_query(
        &self,
        statement: MigrationSqlStatement,
        priority: OperationPriorityV1,
        max_wait: Duration,
    ) -> Result<MigrationSqlRows, MigrationSqlError> {
        let mut lease = self
            .acquire_lane(LaneAdmission::for_priority(priority), max_wait, || None)
            .map_err(|error| MigrationSqlError::ReaderUnavailable(error.to_string()))?;
        lease.execute_migration_query(statement)
    }

    pub(crate) fn begin_migration_snapshot(
        &self,
        priority: OperationPriorityV1,
        max_wait: Duration,
    ) -> Result<MigrationSqlReadSnapshot, MigrationSqlError> {
        let mut lease = self
            .acquire_lane(LaneAdmission::for_priority(priority), max_wait, || None)
            .map_err(|error| MigrationSqlError::ReaderUnavailable(error.to_string()))?;
        lease.begin_migration_snapshot()?;
        Ok(MigrationSqlReadSnapshot::new(move |statement| {
            lease.execute_active_migration_query(statement)
        }))
    }

    pub(crate) fn begin_migration_health_snapshot(
        &self,
        max_wait: Duration,
    ) -> Result<MigrationSqlReadSnapshot, MigrationSqlError> {
        let mut lease = self
            .acquire_lane(
                LaneAdmission::interactive(ReaderLane::ReservedHealth),
                max_wait,
                || None,
            )
            .map_err(|error| MigrationSqlError::ReaderUnavailable(error.to_string()))?;
        lease.retire_after_snapshot();
        lease.begin_migration_snapshot()?;
        Ok(MigrationSqlReadSnapshot::new(move |statement| {
            lease.execute_active_migration_query(statement)
        }))
    }

    pub fn read_store_size<F>(
        &self,
        max_wait: Duration,
        interrupted: F,
    ) -> Result<worker::StoreSizeTelemetrySample, ReaderAcquireError>
    where
        F: FnMut() -> Option<UnavailableReasonV1>,
    {
        let mut lease = self.acquire_lane(
            LaneAdmission::interactive(ReaderLane::ReservedHealth),
            max_wait,
            interrupted,
        )?;
        lease.read_store_size()
    }

    pub fn read_table_sizes<F>(
        &self,
        max_wait: Duration,
        interrupted: F,
    ) -> Result<Vec<worker::TableSizeTelemetrySample>, ReaderAcquireError>
    where
        F: FnMut() -> Option<UnavailableReasonV1>,
    {
        let mut lease = self.acquire_lane(
            LaneAdmission::interactive(ReaderLane::ReservedHealth),
            max_wait,
            interrupted,
        )?;
        lease.read_table_sizes()
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
            limbo_general: state.limbo_general,
            limbo_health: state.limbo_health,
            waiting_general: state.waiting_general,
            waiting_health: state.waiting_health,
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

    /// Stop all reader admission for final attachment shutdown.
    ///
    /// Unlike `begin_drain`, this also fences the reserved health lane. The
    /// health lane remains available during ordinary maintenance drains, but
    /// retaining it during physical eviction would allow new work to race the
    /// final close.
    pub(crate) fn begin_shutdown_drain(&self) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.lifecycle = ReaderPoolState::Draining;
        state.health_admission_open = false;
        drop(state);
        self.inner.capacity_changed.notify_all();
    }

    pub(crate) fn is_quiescent(&self) -> bool {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.opening_general == 0
            && state.opening_health == 0
            && state.leased_general == 0
            && state.leased_health == 0
            // A worker still finishing a deferred snapshot end is in flight
            // even though nothing holds its lease. Dropping the pool now would
            // join a worker mid-rollback with no bound.
            && state.limbo_general == 0
            && state.limbo_health == 0
            && Arc::strong_count(&self.inner) == 1
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
        let admission = LaneAdmission::for_priority(request.priority());
        self.acquire_lane(admission, max_wait, || interruption(probe))
    }

    /// How many concurrent leases this acquisition may hold in its lane.
    ///
    /// A background acquisition stops short of `max_per_hot_shard` so the
    /// remainder stays reachable by interactive reads. The reservation never
    /// shrinks background below one worker: with the smallest legal budget
    /// (`max_per_hot_shard == 2`) maintenance would otherwise never admit.
    fn lease_ceiling(&self, admission: LaneAdmission) -> u16 {
        match admission.lane {
            ReaderLane::ReservedHealth => 1,
            ReaderLane::General if admission.background => self
                .inner
                .budget
                .max_per_hot_shard
                .saturating_sub(FOREGROUND_RESERVED_GENERAL_WORKERS)
                .max(1),
            ReaderLane::General => self.inner.budget.max_per_hot_shard,
        }
    }

    /// Give a direct dispatch one bounded poll quantum to absorb a transient
    /// lease handoff instead of reporting saturation immediately.
    pub(crate) fn acquire_for_dispatch(
        &self,
        request: &RuntimeReadRequestV1,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> Result<ReaderLease<E>, ReaderAcquireError> {
        self.acquire(request, probe, ACQUISITION_POLL_QUANTUM)
    }

    fn acquire_lane<F>(
        &self,
        admission: LaneAdmission,
        max_wait: Duration,
        mut interrupted: F,
    ) -> Result<ReaderLease<E>, ReaderAcquireError>
    where
        F: FnMut() -> Option<UnavailableReasonV1>,
    {
        let lane = admission.lane;
        let lease_ceiling = self.lease_ceiling(admission);
        let mut waiting = WaitingGuard::new(&self.inner, lane);
        let started = Instant::now();
        // Retiring burst workers walks and rebuilds the idle deque under the
        // state lock; it only has anything to do when the idle set has actually
        // changed. Run it on entry and after a notified wake, never on every
        // bounded poll tick — a timed-out wait leaves the idle set untouched, so
        // repeating the scan each 5ms merely adds lock traffic to the hot path.
        let mut retire_pending = true;

        loop {
            if let Some(reason) = interrupted() {
                return Err(ReaderAcquireError::Interrupted { reason });
            }
            if std::mem::take(&mut retire_pending) {
                self.retire_idle_at(Instant::now());
            }
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.lifecycle == ReaderPoolState::Draining
                && (lane == ReaderLane::General || !state.health_admission_open)
            {
                return Err(ReaderAcquireError::Interrupted {
                    reason: UnavailableReasonV1::Draining,
                });
            }
            if lane == ReaderLane::General
                && self
                    .inner
                    .checkpoint_pressure
                    .as_ref()
                    .is_some_and(|pressure| {
                        matches!(&*pressure.borrow(), CheckpointPressure::BlockGeneral { .. })
                    })
            {
                let elapsed = started.elapsed();
                if elapsed >= max_wait {
                    return Err(ReaderAcquireError::Saturated {
                        scope: SaturationScopeV1::ReaderPool,
                    });
                }
                let wait = (max_wait - elapsed).min(ACQUISITION_POLL_QUANTUM);
                waiting.arm(&mut state);
                let (_state, wait_result) = self
                    .inner
                    .capacity_changed
                    .wait_timeout(state, wait)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                retire_pending = !wait_result.timed_out();
                continue;
            }
            // Foreground reservation. A background acquisition holding fewer
            // than `lease_ceiling` leases may take or grow a worker; at the
            // ceiling it waits here instead, leaving the rest of the lane —
            // both idle workers and unspawned headroom — for interactive
            // reads. Foreground acquisitions see the whole lane.
            let leased = match lane {
                ReaderLane::General => state.leased_general,
                ReaderLane::ReservedHealth => state.leased_health,
            };
            if leased < lease_ceiling {
                if let Some(worker) = state.available(lane).pop_front() {
                    match lane {
                        ReaderLane::General => state.leased_general += 1,
                        ReaderLane::ReservedHealth => state.leased_health += 1,
                    }
                    drop(state);
                    return Ok(ReaderLease::checkout(Arc::clone(&self.inner), lane, worker));
                }
                // The reserved-health lane must be able to grow too. Its single
                // worker is otherwise only ever spawned in `start`, and a
                // transient snapshot-end failure retires it permanently: from
                // then on every health acquisition spins to `max_wait` and
                // reports Saturated for the life of the attachment, while
                // `snapshot()` still says Ready.
                let lane_capacity = match lane {
                    ReaderLane::General => self.inner.budget.max_per_hot_shard,
                    ReaderLane::ReservedHealth => 1,
                };
                if state
                    .serviceable_workers(lane)
                    .saturating_add(state.opening(lane))
                    < lane_capacity
                {
                    *state.opening_mut(lane) += 1;
                    drop(state);
                    let spawned =
                        worker::spawn(self.inner.locator.clone(), self.inner.executor.clone())
                            .and_then(|spawned| self.validate_worker_identity(spawned));
                    let mut state = self
                        .inner
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    *state.opening_mut(lane) -= 1;
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
                        state.available(lane).push_back(AvailableWorker {
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
                    *state.leased_mut(lane) += 1;
                    drop(state);
                    return Ok(ReaderLease::checkout(
                        Arc::clone(&self.inner),
                        lane,
                        AvailableWorker {
                            id,
                            client,
                            idle_since: Instant::now(),
                        },
                    ));
                }
            }
            let elapsed = started.elapsed();
            if elapsed >= max_wait {
                return Err(ReaderAcquireError::Saturated {
                    scope: SaturationScopeV1::ReaderPool,
                });
            }
            let wait = (max_wait - elapsed).min(ACQUISITION_POLL_QUANTUM);
            waiting.arm(&mut state);
            let (_state, wait_result) = self
                .inner
                .capacity_changed
                .wait_timeout(state, wait)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            retire_pending = !wait_result.timed_out();
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
        let spawned = worker::spawn(self.inner.locator.clone(), self.inner.executor.clone())
            .and_then(|spawned| self.validate_worker_identity(spawned))?;
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

    fn validate_worker_identity(
        &self,
        spawned: worker::SpawnedWorker,
    ) -> Result<worker::SpawnedWorker, ReaderStartError> {
        let Some(expected) = self.opened_file_identity() else {
            return Ok(spawned);
        };
        if spawned.opened_file_identity == expected {
            return Ok(spawned);
        }
        let actual = spawned.opened_file_identity;
        spawned.client.shutdown();
        let _ = spawned.join.join();
        Err(ReaderStartError::OpenedDatabaseIdentityMismatch { expected, actual })
    }
}
