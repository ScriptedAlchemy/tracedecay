//! RAII ownership of one checked-out pool worker.
//!
//! Every exit path, normal drop, retirement, or a snapshot end that outran its
//! grace period, returns or retires the worker in its lane, so pool capacity
//! cannot leak.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tracedecay_store::{RuntimeReadOutcomeV1, RuntimeReadRequestV1, RuntimeRequestProbeV1};

use super::super::worker::SnapshotEndReply;
use super::super::{
    READ_SNAPSHOT_LIFETIME, ReaderQueryExecutor, ReaderWorkerError, unavailable_read, worker,
};
use super::outcome::{ReaderAcquireError, interruption, map_worker_error, validate_probe};
use super::{
    AvailableWorker, DEFERRED_SNAPSHOT_END_LIMIT, PoolInner, ReaderLane, SNAPSHOT_END_GRACE,
};
use crate::exact_sql::{ExactSqlError, ExactSqlRows, ExactSqlStatement, ExactSqlStatementBudget};

struct Checkout<E: ReaderQueryExecutor> {
    inner: Arc<PoolInner<E>>,
    lane: ReaderLane,
    worker: AvailableWorker,
    deferred_end: Option<SnapshotEndReply>,
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
        // accounting: it is still a live task holding a record, and the pool
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
                        async move {
                            record.client.shutdown();
                            let _ = crate::storage_pool::timeout(
                                DEFERRED_SNAPSHOT_END_LIMIT,
                                receive.recv(),
                            )
                            .await;
                            if let Some(join) = record.join.take() {
                                let _ = join.finished().await;
                            }
                            checkpoint_blockers.finish(reader_id);
                        },
                        crate::storage_pool::spawn,
                    );
                }
            } else {
                record.client.shutdown();
                super::join_worker(&mut record);
            }
        }
        if let Some(receive) = deferred_end {
            let inner = Arc::clone(&self.inner);
            let lane = self.lane;
            let worker = self.worker.clone();
            spawn_or_run_deferred_return(
                finish_deferred_return(inner, lane, worker, receive),
                crate::storage_pool::spawn,
            );
        } else if self.retire && !timed_out_retirement {
            // A failed end retains its blocker until the retired worker has
            // shut down. Successful ends already released it before checkout
            // return; clearing it here could erase a new lease's snapshot.
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

    #[tracing::instrument(
        name = "rusqlite.reader_lease.begin_snapshot",
        level = "trace",
        skip_all
    )]
    pub fn begin_snapshot(&mut self) -> Result<SnapshotLease<'_, E>, ReaderWorkerError> {
        self.start_snapshot(READ_SNAPSHOT_LIFETIME)?;
        Ok(SnapshotLease { lease: self })
    }

    #[tracing::instrument(name = "rusqlite.reader_lease.execute", level = "trace", skip_all)]
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

    #[tracing::instrument(
        name = "rusqlite.reader_lease.exact_sql_query",
        level = "trace",
        skip_all
    )]
    pub(super) fn execute_exact_sql_query(
        &mut self,
        statement: ExactSqlStatement,
    ) -> Result<ExactSqlRows, ExactSqlError> {
        self.start_snapshot(READ_SNAPSHOT_LIFETIME)
            .map_err(|error| ExactSqlError::ReaderUnavailable(error.to_string()))?;
        self.checkout
            .worker
            .client
            .execute_exact_sql_query(statement, ExactSqlStatementBudget::Request)
    }

    #[tracing::instrument(
        name = "rusqlite.reader_lease.begin_exact_sql_snapshot",
        level = "trace",
        skip_all
    )]
    pub(super) fn begin_exact_sql_snapshot(
        &mut self,
        lifetime: Duration,
    ) -> Result<(), ExactSqlError> {
        self.start_snapshot(lifetime)
            .map_err(|error| ExactSqlError::ReaderUnavailable(error.to_string()))?;
        self.checkout.worker.client.pin_exact_sql()
    }

    #[tracing::instrument(
        name = "rusqlite.reader_lease.exact_sql_snapshot_query",
        level = "trace",
        skip_all
    )]
    pub(super) fn execute_active_exact_sql_query(
        &mut self,
        statement: ExactSqlStatement,
        budget: ExactSqlStatementBudget,
    ) -> Result<ExactSqlRows, ExactSqlError> {
        if !self.snapshot_active {
            return Err(ExactSqlError::ReaderUnavailable(
                ReaderWorkerError::SnapshotNotActive.to_string(),
            ));
        }
        self.checkout
            .worker
            .client
            .execute_exact_sql_query(statement, budget)
    }

    #[tracing::instrument(name = "rusqlite.reader_lease.store_size", level = "trace", skip_all)]
    pub(super) fn read_store_size(
        &mut self,
        reply_bound: std::time::Duration,
    ) -> Result<worker::StoreSizeTelemetrySample, ReaderAcquireError> {
        self.start_snapshot(READ_SNAPSHOT_LIFETIME)
            .map_err(ReaderAcquireError::Worker)?;
        self.checkout
            .worker
            .client
            .store_size(reply_bound)
            .map_err(map_worker_error)
    }

    #[tracing::instrument(name = "rusqlite.reader_lease.table_sizes", level = "trace", skip_all)]
    pub(super) fn read_table_sizes(
        &mut self,
        reply_bound: std::time::Duration,
    ) -> Result<Vec<worker::TableSizeTelemetrySample>, ReaderAcquireError> {
        self.start_snapshot(READ_SNAPSHOT_LIFETIME)
            .map_err(ReaderAcquireError::Worker)?;
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

    fn start_snapshot(&mut self, lifetime: Duration) -> Result<(), ReaderWorkerError> {
        self.settle_deferred_end()?;
        if self.snapshot_active
            || !self.checkout.inner.checkpoint_blockers.begin(
                self.checkout.worker.id,
                self.checkout.worker.client.revoker(),
            )
        {
            return Err(ReaderWorkerError::SnapshotAlreadyActive);
        }
        if let Err(error) = self.checkout.worker.client.begin(lifetime) {
            self.checkout
                .inner
                .checkpoint_blockers
                .finish(self.checkout.worker.id);
            return Err(error);
        }
        self.snapshot_active = true;
        Ok(())
    }

    fn settle_deferred_end(&mut self) -> Result<(), ReaderWorkerError> {
        let Some(receive) = self.checkout.deferred_end.take() else {
            return Ok(());
        };
        let error = match end_reply_within(&receive, DEFERRED_SNAPSHOT_END_LIMIT) {
            Some(Ok(Ok(()))) => {
                self.checkout
                    .inner
                    .checkpoint_blockers
                    .finish(self.checkout.worker.id);
                return Ok(());
            }
            Some(Ok(Err(error))) => error,
            Some(Err(_)) => ReaderWorkerError::WorkerClosed,
            None => {
                self.checkout.deferred_end = Some(receive);
                ReaderWorkerError::WorkerClosed
            }
        };
        self.checkout.retire = true;
        Err(error)
    }

    fn defer_snapshot_end(&mut self, receive: SnapshotEndReply) {
        self.checkout
            .inner
            .checkpoint_blockers
            .release(self.checkout.worker.id);
        self.checkout.deferred_end = Some(receive);
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
        match end_reply_within(&receive, SNAPSHOT_END_GRACE) {
            Some(Ok(Ok(()))) => self
                .checkout
                .inner
                .checkpoint_blockers
                .finish(self.checkout.worker.id),
            Some(Ok(Err(_)) | Err(_)) => {
                self.checkout.retire = true;
            }
            None => self.defer_snapshot_end(receive),
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

fn end_reply_within(
    receive: &SnapshotEndReply,
    bound: Duration,
) -> Option<Result<Result<(), ReaderWorkerError>, async_channel::RecvError>> {
    crate::storage_pool::wait_until(receive.recv(), Some(Instant::now() + bound))
}

async fn finish_deferred_return<E: ReaderQueryExecutor>(
    inner: Arc<PoolInner<E>>,
    lane: ReaderLane,
    mut worker: AvailableWorker,
    receive: SnapshotEndReply,
) {
    let checkpoint_blockers = inner.checkpoint_blockers.clone();
    let reader_id = worker.id;
    // Bounded, not open-ended. An unbounded wait here holds this task for the
    // life of the process against a worker that never answers, and the limbo
    // slot it holds never clears, so the lane runs one worker short and
    // shutdown cannot converge. Past the bound the worker is written off.
    let returned = matches!(
        crate::storage_pool::timeout(DEFERRED_SNAPSHOT_END_LIMIT, receive.recv()).await,
        Some(Ok(Ok(())))
    );
    if returned {
        // Rollback is acknowledged. Release this snapshot's blocker before
        // publishing the worker; its next lease may start immediately.
        checkpoint_blockers.finish(reader_id);
    }
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
            let _ = join.finished().await;
        }
    }
    if !returned {
        checkpoint_blockers.finish(reader_id);
    }
}

fn spawn_or_run_deferred_return<F>(
    task: F,
    spawn: impl FnOnce(F) -> Result<crate::storage_pool::PoolTask, F>,
) where
    F: Future<Output = ()> + Send + 'static,
{
    let Err(task) = spawn(task) else {
        return;
    };
    if crate::storage_pool::on_pool() {
        crate::storage_pool::adopt(task);
    } else {
        crate::storage_pool::wait_until(task, None);
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

        spawn_or_run_deferred_return(async move { observed.store(true, Ordering::Release) }, Err);

        assert!(ran.load(Ordering::Acquire));
    }

    #[test]
    fn a_refused_deferred_return_on_a_pool_thread_still_runs_to_completion() {
        let (done, finished) = std::sync::mpsc::channel();
        crate::storage_pool::spawn(async move {
            spawn_or_run_deferred_return(
                async move {
                    crate::storage_pool::sleep(Duration::from_millis(20)).await;
                    let _ = done.send(());
                },
                Err,
            );
        })
        .unwrap_or_else(|_| panic!("the storage pool must start"));

        assert_eq!(
            finished.recv_timeout(Duration::from_secs(10)),
            Ok(()),
            "a pool thread must finish the return it could not spawn, not drop it"
        );
    }
}

#[cfg(test)]
mod snapshot_return_tests {
    use std::thread;
    use std::time::Duration;

    use rusqlite::Connection;
    use tracedecay_store::{OperationPriorityV1, RuntimeReadResultV1};

    use crate::checkpoint::CheckpointBlockerSource;
    use crate::reader::tests::{CountExecutor, Probe, TestStore, request, two_reader_budget};
    use crate::reader::{ReaderPool, ReaderQueryExecutor};

    /// Completes a snapshot end that outran its grace the way `finish_snapshot`
    /// completes one acknowledged in time, so the test does not race the 5ms
    /// grace.
    fn acknowledge_deferred_end<E: ReaderQueryExecutor>(lease: &mut super::ReaderLease<E>) {
        if let Some(end) = lease.checkout.deferred_end.take() {
            end.recv_blocking().unwrap().unwrap();
            lease
                .checkout
                .inner
                .checkpoint_blockers
                .finish(lease.checkout.worker.id);
        }
    }

    #[test]
    fn completed_checkout_return_does_not_touch_snapshot_blockers_again() {
        let store = TestStore::new();
        let pool = ReaderPool::start(store.locator(), two_reader_budget(), CountExecutor).unwrap();
        let read = request(&store.binding, OperationPriorityV1::Foreground);
        let probe = Probe::for_request(&read);
        let mut lease = pool.acquire(&read, &probe, Duration::from_secs(2)).unwrap();
        let _other = pool.acquire(&read, &probe, Duration::from_secs(2)).unwrap();
        let reader_id = lease.checkout.worker.id;
        lease
            .begin_snapshot()
            .unwrap()
            .execute(read.clone(), &probe)
            .unwrap();
        acknowledge_deferred_end(&mut lease);
        assert!(
            pool.inner
                .checkpoint_blockers
                .checkpoint_blockers()
                .is_clear()
        );

        // Rollback has already relinquished this checkout's snapshot. Hold
        // the shared authority to prove its return does not try to clear a
        // blocker after publishing the worker to another borrower.
        let blockers = pool.inner.checkpoint_blockers.active.lock().unwrap();
        // A return that touched the held authority would deadlock this join.
        thread::spawn(move || drop(lease)).join().unwrap();
        drop(blockers);

        let mut next = pool.acquire(&read, &probe, Duration::from_secs(2)).unwrap();
        assert_eq!(next.checkout.worker.id, reader_id);
        let _snapshot = next.begin_snapshot().unwrap();
        assert_eq!(
            pool.inner.checkpoint_blockers.checkpoint_blockers().count(),
            1
        );
    }

    #[test]
    fn deferred_return_keeps_worker_unavailable_until_snapshot_blocker_is_released() {
        let store = TestStore::new();
        let pool = ReaderPool::start(store.locator(), two_reader_budget(), CountExecutor).unwrap();
        let read = request(&store.binding, OperationPriorityV1::Foreground);
        let probe = Probe::for_request(&read);
        let mut lease = pool.acquire(&read, &probe, Duration::from_secs(2)).unwrap();
        let other = pool.acquire(&read, &probe, Duration::from_secs(2)).unwrap();
        let reader_id = lease.checkout.worker.id;
        lease
            .begin_exact_sql_snapshot(crate::reader::READ_SNAPSHOT_LIFETIME)
            .unwrap();

        // Route the real worker's asynchronous rollback acknowledgement
        // through deferred return without depending on the 5ms grace race.
        let blockers = pool.inner.checkpoint_blockers.active.lock().unwrap();
        lease.checkout.deferred_end = Some(lease.checkout.worker.client.begin_end().unwrap());
        lease.snapshot_active = false;
        drop(lease);
        let state = pool.inner.state.lock().unwrap();
        let (state, _) = pool
            .inner
            .capacity_changed
            .wait_timeout_while(state, Duration::from_secs(1), |state| {
                state.limbo_general == 1
            })
            .unwrap();
        let unavailable = state.general.is_empty() && state.limbo_general == 1;
        drop(state);
        drop(blockers);

        let state = pool.inner.state.lock().unwrap();
        let state = pool
            .inner
            .capacity_changed
            .wait_while(state, |state| state.limbo_general != 0)
            .unwrap();
        drop(state);
        assert!(
            unavailable,
            "worker was published while its old snapshot still blocked checkpoints"
        );

        let mut next = pool.acquire(&read, &probe, Duration::from_secs(2)).unwrap();
        assert_eq!(next.checkout.worker.id, reader_id);
        let snapshot = next.begin_snapshot().unwrap();
        assert_eq!(
            pool.inner.checkpoint_blockers.checkpoint_blockers().count(),
            1
        );
        drop(snapshot);
        acknowledge_deferred_end(&mut next);
        assert!(
            pool.inner
                .checkpoint_blockers
                .checkpoint_blockers()
                .is_clear()
        );
        drop(other);
    }

    #[test]
    fn lease_begins_a_new_snapshot_after_a_deferred_end() {
        let store = TestStore::new();
        let pool = ReaderPool::start(store.locator(), two_reader_budget(), CountExecutor).unwrap();
        let read = request(&store.binding, OperationPriorityV1::Foreground);
        let probe = Probe::for_request(&read);
        let mut lease = pool.acquire(&read, &probe, Duration::from_secs(2)).unwrap();
        lease
            .start_snapshot(crate::reader::READ_SNAPSHOT_LIFETIME)
            .unwrap();
        let receive = lease.checkout.worker.client.begin_end().unwrap();
        lease.snapshot_active = false;
        lease.defer_snapshot_end(receive);

        let mut snapshot = lease.begin_snapshot().unwrap();
        assert_eq!(
            snapshot.execute(read.clone(), &probe).unwrap().value(),
            Some(&RuntimeReadResultV1::GraphQuickCheck { healthy: false })
        );
        Connection::open(pool.path())
            .unwrap()
            .execute("INSERT INTO markers(value) VALUES (1)", [])
            .unwrap();
        assert_eq!(
            snapshot.execute(read.clone(), &probe).unwrap().value(),
            Some(&RuntimeReadResultV1::GraphQuickCheck { healthy: false }),
            "the reused snapshot must stay on the view it opened"
        );
        drop(snapshot);

        let mut next = lease.begin_snapshot().unwrap();
        assert_eq!(
            next.execute(read, &probe).unwrap().value(),
            Some(&RuntimeReadResultV1::GraphQuickCheck { healthy: true })
        );
    }
}
