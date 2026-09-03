//! RAII ownership of one checked-out pool worker.
//!
//! Every exit path — normal drop, retirement, or a snapshot end that outran its
//! grace period — returns or retires the worker in its lane, so pool capacity
//! cannot leak.

use std::{
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::Instant,
};

use tracedecay_store::{RuntimeReadOutcomeV1, RuntimeReadRequestV1, RuntimeRequestProbeV1};

use super::super::{ReaderQueryExecutor, ReaderWorkerError, unavailable_read, worker};
use super::outcome::{ReaderAcquireError, interruption, map_worker_error, validate_probe};
use super::{
    AvailableWorker, DEFERRED_SNAPSHOT_END_LIMIT, PoolInner, ReaderLane, SNAPSHOT_END_GRACE,
};
use crate::exact_sql::{ExactSqlError, ExactSqlRows, ExactSqlStatement};

struct Checkout<E: ReaderQueryExecutor> {
    inner: Arc<PoolInner<E>>,
    lane: ReaderLane,
    worker: AvailableWorker,
    deferred_end: Option<Receiver<Result<(), ReaderWorkerError>>>,
    retire: bool,
}

impl<E: ReaderQueryExecutor> Drop for Checkout<E> {
    fn drop(&mut self) {
        let mut deferred_end = self.deferred_end.take();
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
        // The lease is over but the worker has not confirmed its rollback.
        // Move it from leased to limbo rather than dropping it out of the
        // accounting: it is still a live thread holding a record, and the pool
        // must be able to see it, replace it, and refuse to call itself
        // quiescent while it is outstanding.
        //
        // A retired worker is not in limbo. Its record has already left the
        // pool and is shut down below, so nothing is waiting to come back.
        let timed_out_retirement = deferred_end.is_some() && retired.is_some();
        if deferred_end.is_some() && retired.is_none() {
            *state.limbo_mut(self.lane) += 1;
        }
        drop(state);
        self.inner.admission.released();
        self.inner.capacity_changed.notify_all();

        if let Some(mut record) = retired {
            if timed_out_retirement {
                if let Some(receive) = deferred_end.take() {
                    let checkpoint_blockers = self.inner.checkpoint_blockers.clone();
                    let reader_id = self.worker.id;
                    spawn_or_run_deferred_return(
                        Box::new(move || {
                            record.client.shutdown();
                            let _ = receive.recv_timeout(DEFERRED_SNAPSHOT_END_LIMIT);
                            if let Some(join) = record.join.take() {
                                let _ = join.join();
                            }
                            checkpoint_blockers.finish(reader_id);
                        }),
                        |task| {
                            thread::Builder::new()
                                .name("tracedecay-rusqlite-reader-retire".to_owned())
                                .spawn(task)
                        },
                    );
                }
            } else {
                record.client.shutdown();
                if let Some(join) = record.join.take() {
                    let _ = join.join();
                }
            }
        }
        if let Some(receive) = deferred_end {
            let inner = Arc::clone(&self.inner);
            let lane = self.lane;
            let worker = self.worker.clone();
            spawn_or_run_deferred_return(
                Box::new(move || finish_deferred_return(inner, lane, worker, receive)),
                |task| {
                    thread::Builder::new()
                        .name("tracedecay-rusqlite-reader-return".to_owned())
                        .spawn(task)
                },
            );
        } else if !timed_out_retirement {
            self.inner.checkpoint_blockers.finish(self.worker.id);
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
    /// Take ownership of a worker the pool has already accounted as leased.
    ///
    /// The caller must have incremented the lane's leased count first: dropping
    /// this lease decrements it unconditionally.
    pub(super) fn checkout(
        inner: Arc<PoolInner<E>>,
        lane: ReaderLane,
        worker: AvailableWorker,
    ) -> Self {
        Self {
            checkout: Checkout {
                inner,
                lane,
                worker,
                deferred_end: None,
                retire: false,
            },
            snapshot_active: false,
        }
    }

    pub(super) fn retire_after_snapshot(&mut self) {
        self.checkout.retire = true;
    }

    #[hotpath::measure(label = "rusqlite.reader_lease.begin_snapshot")]
    pub fn begin_snapshot(&mut self) -> Result<SnapshotLease<'_, E>, ReaderWorkerError> {
        self.start_snapshot()?;
        Ok(SnapshotLease { lease: self })
    }

    #[hotpath::measure(label = "rusqlite.reader_lease.execute")]
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

    #[hotpath::measure(label = "rusqlite.reader_lease.exact_sql_query")]
    pub(super) fn execute_exact_sql_query(
        &mut self,
        statement: ExactSqlStatement,
    ) -> Result<ExactSqlRows, ExactSqlError> {
        self.start_snapshot()
            .map_err(|error| ExactSqlError::ReaderUnavailable(error.to_string()))?;
        self.checkout
            .worker
            .client
            .execute_exact_sql_query(statement)
    }

    #[hotpath::measure(label = "rusqlite.reader_lease.begin_exact_sql_snapshot")]
    pub(super) fn begin_exact_sql_snapshot(&mut self) -> Result<(), ExactSqlError> {
        self.start_snapshot()
            .map_err(|error| ExactSqlError::ReaderUnavailable(error.to_string()))?;
        self.checkout.worker.client.pin_exact_sql()
    }

    #[hotpath::measure(label = "rusqlite.reader_lease.exact_sql_snapshot_query")]
    pub(super) fn execute_active_exact_sql_query(
        &mut self,
        statement: ExactSqlStatement,
    ) -> Result<ExactSqlRows, ExactSqlError> {
        if !self.snapshot_active {
            return Err(ExactSqlError::ReaderUnavailable(
                ReaderWorkerError::SnapshotNotActive.to_string(),
            ));
        }
        self.checkout
            .worker
            .client
            .execute_exact_sql_query(statement)
    }

    #[hotpath::measure(label = "rusqlite.reader_lease.store_size")]
    pub(super) fn read_store_size(
        &mut self,
        reply_bound: std::time::Duration,
    ) -> Result<worker::StoreSizeTelemetrySample, ReaderAcquireError> {
        self.start_snapshot().map_err(ReaderAcquireError::Worker)?;
        self.checkout
            .worker
            .client
            .store_size(reply_bound)
            .map_err(map_worker_error)
    }

    #[hotpath::measure(label = "rusqlite.reader_lease.table_sizes")]
    pub(super) fn read_table_sizes(
        &mut self,
        reply_bound: std::time::Duration,
    ) -> Result<Vec<worker::TableSizeTelemetrySample>, ReaderAcquireError> {
        self.start_snapshot().map_err(ReaderAcquireError::Worker)?;
        self.checkout
            .worker
            .client
            .table_sizes(reply_bound)
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
        super::super::validate_outcome(&request, outcome)
            .map_err(|error| ReaderAcquireError::Worker(ReaderWorkerError::Storage(error)))
    }

    fn start_snapshot(&mut self) -> Result<(), ReaderWorkerError> {
        if self.snapshot_active
            || !self
                .checkout
                .inner
                .checkpoint_blockers
                .begin(self.checkout.worker.id)
        {
            return Err(ReaderWorkerError::SnapshotAlreadyActive);
        }
        if let Err(error) = self.checkout.worker.client.begin() {
            self.checkout
                .inner
                .checkpoint_blockers
                .finish(self.checkout.worker.id);
            return Err(error);
        }
        self.snapshot_active = true;
        Ok(())
    }

    #[hotpath::measure(label = "rusqlite.reader_lease.finish_snapshot")]
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
            Ok(Ok(())) => self
                .checkout
                .inner
                .checkpoint_blockers
                .finish(self.checkout.worker.id),
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
    let checkpoint_blockers = inner.checkpoint_blockers.clone();
    let reader_id = worker.id;
    // Bounded, not open-ended. An unbounded `recv()` here parks this thread
    // for the life of the process against a worker that never answers, and the
    // limbo slot it holds never clears — so the lane runs one worker short and
    // shutdown cannot converge. Past the bound the worker is written off.
    let returned = matches!(
        receive.recv_timeout(DEFERRED_SNAPSHOT_END_LIMIT),
        Ok(Ok(()))
    );
    let discarded = {
        let mut state = inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *state.limbo_mut(lane) = state.limbo(lane).saturating_sub(1);
        if returned {
            worker.idle_since = Instant::now();
            state.available(lane).push_back(worker);
            None
        } else {
            state.records.remove(&worker.id)
        }
    };
    inner.capacity_changed.notify_all();
    // Release the pool before the join below. Shutting down a worker that
    // already missed its deadline can itself block, and `is_quiescent` counts
    // strong references: holding one here would make a wedged worker stall
    // shutdown all over again, just one level further down.
    drop(inner);
    if let Some(mut record) = discarded {
        record.client.shutdown();
        if let Some(join) = record.join.take() {
            let _ = join.join();
        }
    }
    checkpoint_blockers.finish(reader_id);
}

type DeferredReturnTask = Box<dyn FnOnce() + Send + 'static>;

fn spawn_or_run_deferred_return(
    task: DeferredReturnTask,
    spawn: impl FnOnce(DeferredReturnTask) -> std::io::Result<JoinHandle<()>>,
) {
    // `Builder::spawn` does not return the closure on failure. Keep the real
    // return task recoverable so worker capacity cannot disappear silently.
    let pending = Arc::new(Mutex::new(Some(task)));
    let threaded = Arc::clone(&pending);
    let wrapper: DeferredReturnTask = Box::new(move || {
        if let Some(task) = threaded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            task();
        }
    });
    if spawn(wrapper).is_err() {
        let task = pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            task();
        }
    }
}
#[cfg(test)]
mod deferred_return_spawn_tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[test]
    fn spawn_failure_runs_deferred_return_inline() {
        let ran = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&ran);

        spawn_or_run_deferred_return(
            Box::new(move || observed.store(true, Ordering::Release)),
            |task| {
                drop(task);
                Err(std::io::Error::other("injected spawn failure"))
            },
        );

        assert!(ran.load(Ordering::Acquire));
    }
}
