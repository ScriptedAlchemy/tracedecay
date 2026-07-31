use std::collections::BTreeMap;
use std::fmt::Display;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};

use tracedecay_domain::{
    WorkAttemptIdentityV1, WorkAttemptStateV1, WorkAttemptV1, WorkLeaseFenceV1, WorkProviderRouteV1,
};

use crate::work_execution::WorkProviderExecutionError;

/// Passes shutdown will drain before it stops chasing new admissions.
///
/// One pass cannot see an execution admitted while it was joining, so a second
/// pass is what makes a racing admission reachable. The count is finite so a
/// caller that never closes admission cannot hold shutdown open.
const REAP_DRAIN_PASSES: usize = 2;

/// Upper bound on provider executions this process may run at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkDispatchBoundsV1 {
    capacity: NonZeroUsize,
}

impl WorkDispatchBoundsV1 {
    pub const fn new(capacity: NonZeroUsize) -> Self {
        Self { capacity }
    }

    pub const fn capacity(self) -> usize {
        self.capacity.get()
    }
}

/// Outcome a provider execution reports once it stops running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkProviderSettlementV1 {
    Completed { evidence: String },
    Cancelled,
    Failed { message: String },
}

/// A prepared provider execution owned by the bounded queue.
///
/// `execute` runs on a queue-owned worker thread and must return once the
/// provider stops; `cancel` asks that provider to stop and may be called from
/// any thread while `execute` is in flight.
pub trait WorkProviderRun: Send + Sync + 'static {
    fn execute(&self) -> WorkProviderSettlementV1;

    fn cancel(&self);
}

/// Builds provider executions without starting them.
///
/// `prepare` must not wait on the provider process: the queue holds its
/// admission lock across the call so that the capacity reservation and the
/// execution record are published together.
pub trait WorkProviderExecutionPort: Send + Sync {
    type Run: WorkProviderRun;

    fn route(&self) -> Result<WorkProviderRouteV1, WorkProviderExecutionError>;

    fn prepare(&self, attempt: &WorkAttemptV1) -> Result<Self::Run, WorkProviderExecutionError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkDispatchError {
    /// No durable running intent was recorded before the provider effect.
    NotAdmitted,
    /// The durable intent names a route this queue does not run.
    RouteNotMounted,
    /// Every execution slot is taken and this attempt does not hold one.
    Saturated {
        capacity: usize,
    },
    /// A different or older lease fence tried to claim an in-flight execution.
    StaleFence,
    /// This process holds no execution for the attempt; durable state decides.
    Detached,
    Provider(WorkProviderExecutionError),
}

impl Display for WorkDispatchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAdmitted => {
                formatter.write_str("work attempt has no durable running intent to execute")
            }
            Self::RouteNotMounted => {
                formatter.write_str("work attempt route is not mounted on this queue")
            }
            Self::Saturated { capacity } => write!(
                formatter,
                "work execution queue is saturated at {capacity} concurrent executions"
            ),
            Self::StaleFence => {
                formatter.write_str("work attempt lease fence cannot claim this execution")
            }
            Self::Detached => formatter.write_str("work attempt has no execution in this process"),
            Self::Provider(error) => Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for WorkDispatchError {}

impl From<WorkProviderExecutionError> for WorkDispatchError {
    fn from(error: WorkProviderExecutionError) -> Self {
        Self::Provider(error)
    }
}

struct InFlightV1<R> {
    lease: WorkLeaseFenceV1,
    run: Arc<R>,
    worker: Option<JoinHandle<WorkProviderSettlementV1>>,
}

/// The single execution authority for provider work in this process.
///
/// Admission requires a durable running intent, is idempotent per attempt
/// identity, is fenced by the attempt lease, and is bounded: the queue owns
/// every worker thread and frees a slot only when the execution is settled.
pub struct WorkExecutionQueueV1<P>
where
    P: WorkProviderExecutionPort,
{
    provider: P,
    bounds: WorkDispatchBoundsV1,
    closed: AtomicBool,
    in_flight: Mutex<BTreeMap<WorkAttemptIdentityV1, InFlightV1<P::Run>>>,
}

impl<P> WorkExecutionQueueV1<P>
where
    P: WorkProviderExecutionPort,
{
    pub fn new(provider: P, bounds: WorkDispatchBoundsV1) -> Self {
        Self {
            provider,
            bounds,
            closed: AtomicBool::new(false),
            in_flight: Mutex::new(BTreeMap::new()),
        }
    }

    pub const fn provider(&self) -> &P {
        &self.provider
    }

    pub const fn bounds(&self) -> WorkDispatchBoundsV1 {
        self.bounds
    }

    pub fn route(&self) -> Result<WorkProviderRouteV1, WorkProviderExecutionError> {
        self.provider.route()
    }

    pub fn in_flight(&self) -> usize {
        self.registry().len()
    }

    /// A panic inside a provider must not brick the queue: a poisoned registry
    /// would make every live execution unstoppable and unclaimable.
    fn registry(&self) -> MutexGuard<'_, BTreeMap<WorkAttemptIdentityV1, InFlightV1<P::Run>>> {
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts the provider execution for an attempt whose running intent is
    /// already durable, or reports the bound that refused it.
    ///
    /// `prepare` runs under the admission lock so that the capacity reservation
    /// and the execution record are published together. Callers on an async
    /// runtime must therefore admit from a blocking task.
    pub fn admit(&self, attempt: &WorkAttemptV1) -> Result<(), WorkDispatchError> {
        if attempt.state() != WorkAttemptStateV1::Running {
            return Err(WorkDispatchError::NotAdmitted);
        }
        let mut in_flight = self.registry();
        if self.closed.load(Ordering::Acquire) {
            return Err(WorkDispatchError::NotAdmitted);
        }
        let route = self.provider.route()?;
        if attempt.actual_route() != Some(&route) {
            return Err(WorkDispatchError::RouteNotMounted);
        }
        if let Some(existing) = in_flight.get_mut(attempt.identity()) {
            if existing.lease.lease_id() != attempt.lease().lease_id()
                || attempt.lease().epoch() < existing.lease.epoch()
            {
                return Err(WorkDispatchError::StaleFence);
            }
            if existing.worker.is_none() {
                // The settlement is already claimed; this attempt is finishing,
                // not running, and must not be reported as admitted.
                return Err(WorkDispatchError::Detached);
            }
            existing.lease = attempt.lease().clone();
            return Ok(());
        }
        if in_flight.len() >= self.bounds.capacity() {
            return Err(WorkDispatchError::Saturated {
                capacity: self.bounds.capacity(),
            });
        }
        let run = Arc::new(self.provider.prepare(attempt)?);
        let worker_run = Arc::clone(&run);
        let worker = thread::Builder::new()
            .name(format!(
                "tracedecay-work-{}",
                attempt.identity().attempt_id().as_str()
            ))
            .spawn(move || worker_run.execute())
            .map_err(|error| {
                WorkProviderExecutionError::Unavailable(format!(
                    "work execution worker could not start: {error}"
                ))
            })?;
        in_flight.insert(
            attempt.identity().clone(),
            InFlightV1 {
                lease: attempt.lease().clone(),
                run,
                worker: Some(worker),
            },
        );
        Ok(())
    }

    /// Asks the provider execution held under `lease` to stop.
    pub fn cancel(
        &self,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
    ) -> Result<(), WorkDispatchError> {
        let run = {
            let in_flight = self.registry();
            let entry = in_flight.get(identity).ok_or(WorkDispatchError::Detached)?;
            entry.fenced_by(lease)?;
            Arc::clone(&entry.run)
        };
        run.cancel();
        Ok(())
    }

    /// Claims the settlement for the execution held under `lease` exactly once
    /// and frees its slot.
    pub fn settle(
        &self,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
    ) -> Result<WorkProviderSettlementV1, WorkDispatchError> {
        self.claim(identity, Some(lease))
    }

    /// Cancels and joins every execution this process still owns.
    ///
    /// Shutdown outranks the lease fence: an execution nobody can renew must
    /// still be stopped rather than detached. Draining repeats because an
    /// admission racing shutdown would survive a single snapshot, and it is
    /// bounded because a caller that keeps admitting during shutdown must not
    /// be able to hold the drain open forever.
    pub fn reap(&self) -> usize {
        let mut reaped = 0;
        {
            let _in_flight = self.registry();
            self.closed.store(true, Ordering::Release);
        }
        for _ in 0..REAP_DRAIN_PASSES {
            let identities = self.registry().keys().cloned().collect::<Vec<_>>();
            if identities.is_empty() {
                break;
            }
            for identity in identities {
                if let Some(run) = self
                    .registry()
                    .get(&identity)
                    .map(|entry| Arc::clone(&entry.run))
                {
                    run.cancel();
                }
                if self.claim(&identity, None).is_ok() {
                    reaped += 1;
                } else {
                    // Somebody else holds the settlement, so there is nothing to
                    // join — retire the slot so the drain still terminates.
                    self.registry().remove(&identity);
                }
            }
        }
        // Anything still registered was admitted while shutdown was draining.
        // Its slot is retired so capacity cannot leak past this process, but the
        // execution is not reported as joined, because it was not.
        self.registry().clear();
        reaped
    }

    /// Takes an attempt's worker and joins it, releasing the slot afterwards.
    ///
    /// Validating the fence and taking the worker in one critical section is
    /// what makes a settlement claimable exactly once: checking under one lock
    /// and taking under another let two callers both believe they owned the
    /// execution, and the loser invented a failure the provider never
    /// reported. The loser now learns it was detached. The slot is retired only
    /// after the join, so a draining worker still counts against the bound.
    fn claim(
        &self,
        identity: &WorkAttemptIdentityV1,
        lease: Option<&WorkLeaseFenceV1>,
    ) -> Result<WorkProviderSettlementV1, WorkDispatchError> {
        let worker = {
            let mut in_flight = self.registry();
            let entry = in_flight
                .get_mut(identity)
                .ok_or(WorkDispatchError::Detached)?;
            if let Some(lease) = lease {
                entry.fenced_by(lease)?;
            }
            entry.worker.take().ok_or(WorkDispatchError::Detached)?
        };
        let settlement = worker.join().unwrap_or(WorkProviderSettlementV1::Failed {
            message: "work execution worker panicked".to_owned(),
        });
        self.registry().remove(identity);
        Ok(settlement)
    }
}

impl<R> InFlightV1<R> {
    fn fenced_by(&self, lease: &WorkLeaseFenceV1) -> Result<(), WorkDispatchError> {
        if self.lease.lease_id() != lease.lease_id() || lease.epoch() < self.lease.epoch() {
            return Err(WorkDispatchError::StaleFence);
        }
        if self.worker.is_none() {
            return Err(WorkDispatchError::Detached);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use tracedecay_domain::{
        AttemptId, ProjectionGenerationId, ProviderId, RunId, TaskId,
        WorkAttemptProjectionBindingV1, WorkCancellationStateV1, WorkFenceEpochV1, WorkLeaseId,
        WorkProjectionSequenceV1, WorkProviderRouteId, WorkRecoveryStateV1, WorkVersion,
    };

    use super::*;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn route(value: &str) -> WorkProviderRouteV1 {
        WorkProviderRouteV1::new(
            id::<ProviderId>("provider.work.dispatch"),
            id::<WorkProviderRouteId>(value),
        )
        .unwrap()
    }

    fn lease(lease_id: &str, epoch: u64) -> WorkLeaseFenceV1 {
        WorkLeaseFenceV1::new(
            id::<WorkLeaseId>(lease_id),
            WorkFenceEpochV1::new(epoch).unwrap(),
        )
        .unwrap()
    }

    fn attempt(
        attempt_id: &str,
        state: WorkAttemptStateV1,
        lease: WorkLeaseFenceV1,
        actual_route: Option<WorkProviderRouteV1>,
    ) -> WorkAttemptV1 {
        WorkAttemptV1::new(
            WorkAttemptIdentityV1::new(
                id::<TaskId>("task.work.dispatch"),
                id::<RunId>("run.work.dispatch"),
                id::<AttemptId>(attempt_id),
            )
            .unwrap(),
            WorkAttemptProjectionBindingV1::new(
                id::<ProjectionGenerationId>("generation.work.dispatch"),
                WorkProjectionSequenceV1::new(2),
                WorkVersion::initial(),
            )
            .unwrap(),
            lease,
            state,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            route("route.work.dispatch"),
            actual_route,
            None,
        )
        .unwrap()
    }

    fn running(attempt_id: &str, fence: u64) -> WorkAttemptV1 {
        attempt(
            attempt_id,
            WorkAttemptStateV1::Running,
            lease("lease.work.dispatch", fence),
            Some(route("route.work.dispatch")),
        )
    }

    struct BlockingRun {
        released: Arc<AtomicBool>,
        cancelled: Arc<AtomicBool>,
    }

    impl WorkProviderRun for BlockingRun {
        fn execute(&self) -> WorkProviderSettlementV1 {
            while !self.released.load(Ordering::SeqCst) && !self.cancelled.load(Ordering::SeqCst) {
                thread::sleep(std::time::Duration::from_millis(1));
            }
            if self.cancelled.load(Ordering::SeqCst) {
                WorkProviderSettlementV1::Cancelled
            } else {
                WorkProviderSettlementV1::Completed {
                    evidence: "released".to_owned(),
                }
            }
        }

        fn cancel(&self) {
            self.cancelled.store(true, Ordering::SeqCst);
        }
    }

    struct BlockingProvider {
        released: Arc<AtomicBool>,
        prepared: Arc<AtomicUsize>,
        route_id: &'static str,
    }

    impl BlockingProvider {
        fn new(released: Arc<AtomicBool>) -> Self {
            Self {
                released,
                prepared: Arc::new(AtomicUsize::new(0)),
                route_id: "route.work.dispatch",
            }
        }
    }

    impl WorkProviderExecutionPort for BlockingProvider {
        type Run = BlockingRun;

        fn route(&self) -> Result<WorkProviderRouteV1, WorkProviderExecutionError> {
            Ok(route(self.route_id))
        }

        fn prepare(
            &self,
            _attempt: &WorkAttemptV1,
        ) -> Result<Self::Run, WorkProviderExecutionError> {
            self.prepared.fetch_add(1, Ordering::SeqCst);
            Ok(BlockingRun {
                released: Arc::clone(&self.released),
                cancelled: Arc::new(AtomicBool::new(false)),
            })
        }
    }

    fn bounds(capacity: usize) -> WorkDispatchBoundsV1 {
        WorkDispatchBoundsV1::new(NonZeroUsize::new(capacity).unwrap())
    }

    #[test]
    fn admission_requires_a_durable_running_intent() {
        let queue = WorkExecutionQueueV1::new(
            BlockingProvider::new(Arc::new(AtomicBool::new(true))),
            bounds(1),
        );
        let leased = attempt(
            "attempt.work.leased",
            WorkAttemptStateV1::Leased,
            lease("lease.work.dispatch", 1),
            None,
        );

        assert_eq!(
            queue.admit(&leased).unwrap_err(),
            WorkDispatchError::NotAdmitted
        );
        assert_eq!(queue.in_flight(), 0);
        assert_eq!(queue.provider().prepared.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn admission_rejects_intents_bound_to_an_unmounted_route() {
        let queue = WorkExecutionQueueV1::new(
            BlockingProvider::new(Arc::new(AtomicBool::new(true))),
            bounds(1),
        );
        let elsewhere = attempt(
            "attempt.work.elsewhere",
            WorkAttemptStateV1::Running,
            lease("lease.work.dispatch", 1),
            Some(route("route.work.other")),
        );

        assert_eq!(
            queue.admit(&elsewhere).unwrap_err(),
            WorkDispatchError::RouteNotMounted
        );
        assert_eq!(queue.provider().prepared.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn saturated_admission_refuses_new_work_and_recovers_after_settlement() {
        let released = Arc::new(AtomicBool::new(false));
        let queue =
            WorkExecutionQueueV1::new(BlockingProvider::new(Arc::clone(&released)), bounds(1));
        let first = running("attempt.work.first", 1);
        let second = attempt(
            "attempt.work.second",
            WorkAttemptStateV1::Running,
            lease("lease.work.second", 1),
            Some(route("route.work.dispatch")),
        );

        queue.admit(&first).unwrap();
        assert_eq!(
            queue.admit(&second).unwrap_err(),
            WorkDispatchError::Saturated { capacity: 1 }
        );
        assert_eq!(queue.in_flight(), 1);

        released.store(true, Ordering::SeqCst);
        assert_eq!(
            queue.settle(first.identity(), first.lease()).unwrap(),
            WorkProviderSettlementV1::Completed {
                evidence: "released".to_owned()
            }
        );
        assert_eq!(queue.in_flight(), 0);
        queue.admit(&second).unwrap();
        assert_eq!(queue.in_flight(), 1);
        assert_eq!(queue.provider().prepared.load(Ordering::SeqCst), 2);
        queue.reap();
    }

    #[test]
    fn repeated_admission_reuses_one_execution_and_fences_older_leases() {
        let released = Arc::new(AtomicBool::new(false));
        let queue =
            WorkExecutionQueueV1::new(BlockingProvider::new(Arc::clone(&released)), bounds(4));
        let first = running("attempt.work.idempotent", 1);
        let renewed = running("attempt.work.idempotent", 2);

        queue.admit(&first).unwrap();
        queue.admit(&first).unwrap();
        queue.admit(&renewed).unwrap();
        assert_eq!(queue.in_flight(), 1);
        assert_eq!(queue.provider().prepared.load(Ordering::SeqCst), 1);

        assert_eq!(
            queue.admit(&first).unwrap_err(),
            WorkDispatchError::StaleFence
        );
        let stolen = attempt(
            "attempt.work.idempotent",
            WorkAttemptStateV1::Running,
            lease("lease.work.other", 9),
            Some(route("route.work.dispatch")),
        );
        assert_eq!(
            queue.admit(&stolen).unwrap_err(),
            WorkDispatchError::StaleFence
        );

        assert_eq!(
            queue.settle(renewed.identity(), first.lease()).unwrap_err(),
            WorkDispatchError::StaleFence,
            "an older fence must not claim the settlement the renewed lease owns"
        );
        assert_eq!(
            queue.cancel(renewed.identity(), first.lease()).unwrap_err(),
            WorkDispatchError::StaleFence,
            "an older fence must not stop the execution the renewed lease owns"
        );

        released.store(true, Ordering::SeqCst);
        queue.settle(renewed.identity(), renewed.lease()).unwrap();
    }

    #[test]
    fn cancellation_settles_once_and_then_detaches() {
        let queue = WorkExecutionQueueV1::new(
            BlockingProvider::new(Arc::new(AtomicBool::new(false))),
            bounds(2),
        );
        let attempt = running("attempt.work.cancel", 1);

        queue.admit(&attempt).unwrap();
        queue.cancel(attempt.identity(), attempt.lease()).unwrap();
        assert_eq!(
            queue.settle(attempt.identity(), attempt.lease()).unwrap(),
            WorkProviderSettlementV1::Cancelled
        );
        assert_eq!(
            queue
                .settle(attempt.identity(), attempt.lease())
                .unwrap_err(),
            WorkDispatchError::Detached
        );
        assert_eq!(
            queue
                .cancel(attempt.identity(), attempt.lease())
                .unwrap_err(),
            WorkDispatchError::Detached
        );
        assert_eq!(queue.in_flight(), 0);
    }

    #[test]
    fn reaping_stops_and_joins_every_execution_a_restart_would_abandon() {
        let queue = WorkExecutionQueueV1::new(
            BlockingProvider::new(Arc::new(AtomicBool::new(false))),
            bounds(3),
        );
        for suffix in ["one", "two", "three"] {
            let attempt = attempt(
                &format!("attempt.work.reap.{suffix}"),
                WorkAttemptStateV1::Running,
                lease(&format!("lease.work.reap.{suffix}"), 1),
                Some(route("route.work.dispatch")),
            );
            queue.admit(&attempt).unwrap();
        }
        assert_eq!(queue.in_flight(), 3);

        assert_eq!(queue.reap(), 3);
        assert_eq!(queue.in_flight(), 0);
        assert_eq!(
            queue.admit(&running("attempt.work.after-reap", 1)),
            Err(WorkDispatchError::NotAdmitted),
            "a snapshotted queue must not admit provider work after shutdown"
        );
    }

    #[test]
    fn a_panicking_execution_settles_as_failed_rather_than_hanging() {
        struct PanicRun;

        impl WorkProviderRun for PanicRun {
            fn execute(&self) -> WorkProviderSettlementV1 {
                panic!("provider execution panicked");
            }

            fn cancel(&self) {}
        }

        struct PanicProvider;

        impl WorkProviderExecutionPort for PanicProvider {
            type Run = PanicRun;

            fn route(&self) -> Result<WorkProviderRouteV1, WorkProviderExecutionError> {
                Ok(route("route.work.dispatch"))
            }

            fn prepare(
                &self,
                _attempt: &WorkAttemptV1,
            ) -> Result<Self::Run, WorkProviderExecutionError> {
                Ok(PanicRun)
            }
        }

        let queue = WorkExecutionQueueV1::new(PanicProvider, bounds(1));
        let attempt = running("attempt.work.panic", 1);
        queue.admit(&attempt).unwrap();

        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let settlement = queue.settle(attempt.identity(), attempt.lease()).unwrap();
        std::panic::set_hook(previous);

        assert_eq!(
            settlement,
            WorkProviderSettlementV1::Failed {
                message: "work execution worker panicked".to_owned()
            }
        );
        assert_eq!(queue.in_flight(), 0);
    }

    #[test]
    fn a_provider_that_cannot_prepare_never_occupies_a_slot() {
        struct UnavailableProvider;

        impl WorkProviderExecutionPort for UnavailableProvider {
            type Run = BlockingRun;

            fn route(&self) -> Result<WorkProviderRouteV1, WorkProviderExecutionError> {
                Ok(route("route.work.dispatch"))
            }

            fn prepare(
                &self,
                _attempt: &WorkAttemptV1,
            ) -> Result<Self::Run, WorkProviderExecutionError> {
                Err(WorkProviderExecutionError::Unavailable(
                    "provider is offline".to_owned(),
                ))
            }
        }

        let queue = WorkExecutionQueueV1::new(UnavailableProvider, bounds(1));
        let attempt = running("attempt.work.unavailable", 1);

        assert_eq!(
            queue.admit(&attempt).unwrap_err(),
            WorkDispatchError::Provider(WorkProviderExecutionError::Unavailable(
                "provider is offline".to_owned()
            ))
        );
        assert_eq!(queue.in_flight(), 0);
        queue.admit(&attempt).unwrap_err();
        assert_eq!(queue.in_flight(), 0);
    }
}
