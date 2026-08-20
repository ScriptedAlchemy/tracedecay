use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until, timeout, timeout_at};
use tracedecay_application::{
    ApplicationContractError, EXECUTION_TOPOLOGY_EVENT_KINDS_V1, now_micros,
};
use tracedecay_domain::{
    CoverageStateV1, ObservabilityEnvelopeV1, ObservabilityPayloadV1,
    ObservabilityRetentionClassV1, ObservabilityTerminalResultV1, TelemetryDropObservedV1,
};
use tracedecay_global_db::{
    AnalyticsEventInsert, ObservabilityEmissionClaimV1, ObservabilityEmissionOutboxRecordV1,
    RegisteredGlobalDb, RegisteredGlobalDbLeaseV1,
};

use crate::event_lane::record_observability;

mod outbox;
mod rollup_rebuild;
use outbox::{
    claim_and_settle_durable, mark_delivery_delayed, recover_pending, settle_claimed_durable,
};
use rollup_rebuild::{RollupAdvanceOutcome, run_one_rollup_maintenance};

const PRODUCER_RUNNING: u8 = 0;
const PRODUCER_STOPPING: u8 = 1;
const PRODUCER_STOPPED: u8 = 2;
const MAX_PRODUCER_CAPACITY: usize = 1_024;
const MAX_PRODUCER_DEADLINE: Duration = Duration::from_secs(60);
const ROLLUP_BACKLOG_REBUILD_INTERVAL: Duration = Duration::from_secs(1);
const ROLLUP_IDLE_RETRY_INTERVAL: Duration = Duration::from_secs(5 * 60);
const TELEMETRY_DROP_EVENT_KIND: &str = "telemetry.drop.observed.v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservabilityProducerIdentityV1 {
    pub authorized_scope_ref: String,
    pub process_boot_id: String,
    pub producer_revision: String,
    pub configuration_revision: String,
    pub policy_revision: String,
}

impl ObservabilityProducerIdentityV1 {
    fn validate(&self) -> Result<(), &'static str> {
        for value in [
            self.authorized_scope_ref.as_str(),
            self.process_boot_id.as_str(),
            self.producer_revision.as_str(),
            self.configuration_revision.as_str(),
            self.policy_revision.as_str(),
        ] {
            if !payload_safe_label(value, 128) {
                return Err("observability_producer_identity");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservabilityEmissionOutcomeV1 {
    Enqueued,
    DroppedAtCapacity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservabilityOwnerEmissionOutcomeV1 {
    Enqueued,
    Replayed,
    DeferredDurable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservabilityProducerSummaryV1 {
    pub persisted: u64,
    pub dropped: u64,
    pub cancelled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservabilityProducerDeadlinesV1 {
    pub persistence: Duration,
    pub shutdown: Duration,
}

impl Default for ObservabilityProducerDeadlinesV1 {
    fn default() -> Self {
        Self {
            persistence: Duration::from_secs(2),
            shutdown: Duration::from_secs(5),
        }
    }
}

impl ObservabilityProducerDeadlinesV1 {
    fn validate(self) -> Result<Self, &'static str> {
        if self.persistence.is_zero()
            || self.shutdown < self.persistence
            || self.shutdown > MAX_PRODUCER_DEADLINE
        {
            return Err("observability_producer_deadlines");
        }
        Ok(self)
    }
}

enum ProducerControl {
    Shutdown {
        cancelled: bool,
        reply: oneshot::Sender<Result<ObservabilityProducerSummaryV1, ApplicationContractError>>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DropRange {
    first: u64,
    last: u64,
    count: u64,
}

impl DropRange {
    fn merge(&mut self, other: Self) -> bool {
        if self.last.saturating_add(1) != other.first {
            return false;
        }
        self.last = other.last;
        self.count = self.count.saturating_add(other.count);
        true
    }
}

struct QueuedObservation {
    envelope: ObservabilityEnvelopeV1,
    carried_drop: Option<DropRange>,
    owner_fact: Option<QueuedOwnerFact>,
}

struct QueuedOwnerFact {
    json: String,
    durable_claimed: bool,
}

struct ProducerWorkerState {
    pending_dropped: Arc<AtomicU64>,
    total_dropped: Arc<AtomicU64>,
    first_missing_sequence: Arc<AtomicU64>,
    last_missing_sequence: Arc<AtomicU64>,
    next_sequence: Arc<AtomicU64>,
    lifecycle: Arc<AtomicU8>,
    durable_emission_lock: Arc<AsyncMutex<()>>,
    deadlines: ObservabilityProducerDeadlinesV1,
}

struct ProducerWorkerProgress {
    persisted: u64,
    first_error: Option<ApplicationContractError>,
    rollup_frontier_initialized: bool,
}

pub struct BoundedObservabilityProducerV1 {
    db: RegisteredGlobalDbLeaseV1,
    identity: ObservabilityProducerIdentityV1,
    data: mpsc::Sender<QueuedObservation>,
    control: mpsc::Sender<ProducerControl>,
    pending_dropped: Arc<AtomicU64>,
    total_dropped: Arc<AtomicU64>,
    first_missing_sequence: Arc<AtomicU64>,
    last_missing_sequence: Arc<AtomicU64>,
    next_sequence: Arc<AtomicU64>,
    state: Arc<AtomicU8>,
    deadlines: ObservabilityProducerDeadlinesV1,
    emission_lock: Mutex<()>,
    durable_emission_lock: Arc<AsyncMutex<()>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl BoundedObservabilityProducerV1 {
    pub fn start(
        db: RegisteredGlobalDbLeaseV1,
        identity: ObservabilityProducerIdentityV1,
        capacity: usize,
    ) -> Result<Self, &'static str> {
        Self::start_with_deadlines(
            db,
            identity,
            capacity,
            ObservabilityProducerDeadlinesV1::default(),
        )
    }

    pub fn start_with_deadlines(
        db: RegisteredGlobalDbLeaseV1,
        identity: ObservabilityProducerIdentityV1,
        capacity: usize,
        deadlines: ObservabilityProducerDeadlinesV1,
    ) -> Result<Self, &'static str> {
        identity.validate()?;
        if capacity == 0 || capacity > MAX_PRODUCER_CAPACITY {
            return Err("observability_producer_capacity");
        }
        let deadlines = deadlines.validate()?;
        let (data, data_rx) = mpsc::channel(capacity);
        // The control lane remains writable when every data slot is occupied.
        let (control, control_rx) = mpsc::channel(1);
        let pending_dropped = Arc::new(AtomicU64::new(0));
        let total_dropped = Arc::new(AtomicU64::new(0));
        let first_missing_sequence = Arc::new(AtomicU64::new(0));
        let last_missing_sequence = Arc::new(AtomicU64::new(0));
        let next_sequence = Arc::new(AtomicU64::new(1));
        let state = Arc::new(AtomicU8::new(PRODUCER_RUNNING));
        let durable_emission_lock = Arc::new(AsyncMutex::new(()));
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| "observability_producer_runtime_unavailable")?;
        let worker = runtime.spawn(run_worker(
            db.clone(),
            identity.clone(),
            data_rx,
            control_rx,
            ProducerWorkerState {
                pending_dropped: Arc::clone(&pending_dropped),
                total_dropped: Arc::clone(&total_dropped),
                first_missing_sequence: Arc::clone(&first_missing_sequence),
                last_missing_sequence: Arc::clone(&last_missing_sequence),
                next_sequence: Arc::clone(&next_sequence),
                lifecycle: Arc::clone(&state),
                durable_emission_lock: Arc::clone(&durable_emission_lock),
                deadlines,
            },
        ));
        Ok(Self {
            db,
            identity,
            data,
            control,
            pending_dropped,
            total_dropped,
            first_missing_sequence,
            last_missing_sequence,
            next_sequence,
            state,
            deadlines,
            emission_lock: Mutex::new(()),
            durable_emission_lock,
            worker: Mutex::new(Some(worker)),
        })
    }

    /// The authority this producer stamps on every accepted envelope.
    ///
    /// Callers use the scope to construct owner-derived payloads. Producer
    /// identity, sequence, and watermark remain producer-owned and are
    /// overwritten at admission rather than trusted from the caller.
    pub const fn identity(&self) -> &ObservabilityProducerIdentityV1 {
        &self.identity
    }

    pub const fn persistence_deadline(&self) -> Duration {
        self.deadlines.persistence
    }

    fn validate_admission(&self, envelope: &ObservabilityEnvelopeV1) -> Result<(), &'static str> {
        if self.state.load(Ordering::Acquire) != PRODUCER_RUNNING {
            return Err("observability_producer_closed");
        }
        if envelope.scope_ref != self.identity.authorized_scope_ref {
            return Err("observability_producer_binding");
        }
        if [
            envelope.event_id.as_str(),
            envelope.idempotency_key.as_str(),
            envelope.trace_id.as_str(),
            envelope.capability.as_str(),
            envelope.operation.as_str(),
        ]
        .into_iter()
        .any(|value| !payload_safe_label(value, 128))
        {
            return Err("observability_producer_redaction");
        }
        Ok(())
    }

    fn prepare_delivery(
        &self,
        envelope: ObservabilityEnvelopeV1,
        sequence: u64,
        delayed: bool,
    ) -> Result<ObservabilityEnvelopeV1, &'static str> {
        prepare_delivery_with_identity(&self.identity, envelope, sequence, delayed)
    }

    pub fn try_emit(
        &self,
        envelope: ObservabilityEnvelopeV1,
    ) -> Result<ObservabilityEmissionOutcomeV1, &'static str> {
        let _emission_guard = self
            .emission_lock
            .lock()
            .map_err(|_| "observability_producer_lock_poisoned")?;
        self.validate_admission(&envelope)?;
        let sequence = self.next_sequence.fetch_add(1, Ordering::AcqRel);
        let envelope = self.prepare_delivery(envelope, sequence, false)?;
        self.offer_prepared(envelope, None)
    }

    fn offer_prepared(
        &self,
        mut envelope: ObservabilityEnvelopeV1,
        owner_fact: Option<QueuedOwnerFact>,
    ) -> Result<ObservabilityEmissionOutcomeV1, &'static str> {
        let sequence = envelope.producer_sequence;
        match self.data.try_reserve() {
            Ok(permit) => {
                let carried_drops = self.pending_dropped.swap(0, Ordering::AcqRel);
                let carried_drop = (carried_drops > 0).then(|| {
                    let first = self.first_missing_sequence.swap(0, Ordering::AcqRel);
                    let last = self.last_missing_sequence.swap(0, Ordering::AcqRel);
                    DropRange {
                        first,
                        last,
                        count: carried_drops,
                    }
                });
                if carried_drops > 0 {
                    envelope.dropped_count = envelope.dropped_count.saturating_add(carried_drops);
                    envelope.coverage = CoverageStateV1::Partial;
                }
                permit.send(QueuedObservation {
                    envelope,
                    carried_drop,
                    owner_fact,
                });
                Ok(ObservabilityEmissionOutcomeV1::Enqueued)
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.record_capacity_drop(sequence);
                Ok(ObservabilityEmissionOutcomeV1::DroppedAtCapacity)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err("observability_producer_closed"),
        }
    }

    fn record_capacity_drop(&self, sequence: u64) {
        self.pending_dropped.fetch_add(1, Ordering::AcqRel);
        self.total_dropped.fetch_add(1, Ordering::AcqRel);
        let _ = self.first_missing_sequence.compare_exchange(
            0,
            sequence,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.last_missing_sequence
            .store(sequence, Ordering::Release);
    }

    pub async fn shutdown(
        &self,
    ) -> Result<ObservabilityProducerSummaryV1, ApplicationContractError> {
        self.stop(false).await
    }

    pub async fn cancel(&self) -> Result<ObservabilityProducerSummaryV1, ApplicationContractError> {
        self.stop(true).await
    }

    async fn stop(
        &self,
        cancelled: bool,
    ) -> Result<ObservabilityProducerSummaryV1, ApplicationContractError> {
        let shutdown_deadline = Instant::now() + self.deadlines.shutdown;
        if self
            .state
            .compare_exchange(
                PRODUCER_RUNNING,
                PRODUCER_STOPPING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return Err(ApplicationContractError::Domain(
                "observability_producer_closed".to_owned(),
            ));
        }
        let (mut worker, worker_lock_poisoned) = match self.worker.lock() {
            Ok(mut guard) => (guard.take(), false),
            Err(poisoned) => (poisoned.into_inner().take(), true),
        };
        if worker_lock_poisoned {
            if let Some(worker) = worker.take() {
                worker.abort();
                let _ = worker.await;
            }
            self.state.store(PRODUCER_STOPPED, Ordering::Release);
            return Err(ApplicationContractError::Domain(
                "observability_producer_lock_poisoned".to_owned(),
            ));
        }
        // STOPPING rejects new synchronous admissions immediately. Polling the
        // lock under the same absolute deadline then fences any admission that
        // had already validated RUNNING without blocking the async executor.
        loop {
            let admission_fenced = match self.emission_lock.try_lock() {
                Ok(guard) => {
                    drop(guard);
                    Ok(true)
                }
                Err(std::sync::TryLockError::Poisoned(_)) => Err(()),
                Err(std::sync::TryLockError::WouldBlock) => Ok(false),
            };
            match admission_fenced {
                Ok(true) => break,
                Err(()) => {
                    if let Some(worker) = worker.take() {
                        worker.abort();
                        let _ = worker.await;
                    }
                    self.state.store(PRODUCER_STOPPED, Ordering::Release);
                    return Err(ApplicationContractError::Domain(
                        "observability_producer_lock_poisoned".to_owned(),
                    ));
                }
                Ok(false) => {
                    let now = Instant::now();
                    if now >= shutdown_deadline {
                        if let Some(worker) = worker.take() {
                            worker.abort();
                            let _ = worker.await;
                        }
                        self.state.store(PRODUCER_STOPPED, Ordering::Release);
                        return Err(ApplicationContractError::Domain(
                            "observability_shutdown_deadline".to_owned(),
                        ));
                    }
                    sleep_until((now + Duration::from_millis(1)).min(shutdown_deadline)).await;
                }
            }
        }
        // An owner fact may have passed admission under the async durable
        // lock just before the state transition. Wait for that claim and its
        // sequence allocation before sealing the worker stream.
        let durable_guard =
            match timeout_at(shutdown_deadline, self.durable_emission_lock.lock()).await {
                Ok(guard) => guard,
                Err(_) => {
                    if let Some(worker) = worker.take() {
                        worker.abort();
                        let _ = worker.await;
                    }
                    self.state.store(PRODUCER_STOPPED, Ordering::Release);
                    return Err(ApplicationContractError::Domain(
                        "observability_shutdown_deadline".to_owned(),
                    ));
                }
            };
        drop(durable_guard);
        let (reply, result) = oneshot::channel();
        if self
            .control
            .try_send(ProducerControl::Shutdown { cancelled, reply })
            .is_err()
        {
            if let Some(worker) = worker.take() {
                worker.abort();
                let _ = worker.await;
            }
            self.state.store(PRODUCER_STOPPED, Ordering::Release);
            return Err(ApplicationContractError::Domain(
                "observability_control_lane_closed".to_owned(),
            ));
        }
        let outcome = match timeout_at(shutdown_deadline, result).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => {
                if let Some(worker) = worker.take() {
                    let _ = worker.await;
                }
                self.state.store(PRODUCER_STOPPED, Ordering::Release);
                return Err(ApplicationContractError::Domain(
                    "observability_worker_stopped".to_owned(),
                ));
            }
            Err(_) => {
                if let Some(worker) = worker.take() {
                    worker.abort();
                    let _ = worker.await;
                }
                self.state.store(PRODUCER_STOPPED, Ordering::Release);
                return Err(ApplicationContractError::Domain(
                    "observability_shutdown_deadline".to_owned(),
                ));
            }
        };
        if let Some(worker) = worker.take() {
            worker.await.map_err(|error| {
                ApplicationContractError::Domain(format!(
                    "observability worker join failed: {error}"
                ))
            })?;
        }
        outcome
    }
}

async fn run_worker(
    db: RegisteredGlobalDbLeaseV1,
    identity: ObservabilityProducerIdentityV1,
    mut data: mpsc::Receiver<QueuedObservation>,
    mut control: mpsc::Receiver<ProducerControl>,
    state: ProducerWorkerState,
) {
    let mut progress = ProducerWorkerProgress {
        persisted: 0,
        first_error: None,
        rollup_frontier_initialized: false,
    };
    // Frontier write is idle-only: it happens inside the rollup arm of the
    // biased select below, never as a concurrent future racing persist for
    // the same 2s write deadline (live: exceeded persistence deadline). The
    // first tick is due immediately so an idle producer still initializes
    // the frontier and closes proved-quiet days promptly; a busy startup
    // drains control and observations first because the select is biased.
    let rollup_tick = sleep_until(Instant::now());
    tokio::pin!(rollup_tick);
    let mut rollup_source_persisted = false;
    loop {
        tokio::select! {
            biased;
            command = control.recv() => {
                let Some(ProducerControl::Shutdown { cancelled, reply }) = command else {
                    settle_worker(
                        &db,
                        &identity,
                        &mut data,
                        &state,
                        &mut progress,
                        false,
                        false,
                    )
                    .await;
                    break;
                };
                let dropped_count = settle_worker(
                    &db,
                    &identity,
                    &mut data,
                    &state,
                    &mut progress,
                    cancelled,
                    !cancelled,
                )
                .await;
                state.lifecycle.store(PRODUCER_STOPPED, Ordering::Release);
                let result = progress.first_error.map_or_else(
                    || Ok(ObservabilityProducerSummaryV1 {
                        persisted: progress.persisted,
                        dropped: dropped_count,
                        cancelled,
                    }),
                    Err,
                );
                let _ = reply.send(result);
                break;
            }
            observation = data.recv() => {
                let Some(observation) = observation else {
                    break;
                };
                let wakes_rollup = observation_dirties_rollup(&observation);
                let persisted_before = progress.persisted;
                record_queued(
                    &db,
                    &identity,
                    &state.durable_emission_lock,
                    &state.next_sequence,
                    observation,
                    &mut progress,
                    state.deadlines.persistence,
                )
                .await;
                recover_pending(
                    &db,
                    &identity,
                    &data,
                    &state.durable_emission_lock,
                    &mut progress,
                    state.deadlines.persistence,
                )
                .await;
                rollup_source_persisted |= wakes_rollup && progress.persisted > persisted_before;
                if should_wake_rollup_now(
                    progress.rollup_frontier_initialized,
                    rollup_source_persisted,
                    data.is_empty(),
                ) {
                    // Only a newly durable topology/drop source can revoke a
                    // prior deferral and wake the slow no-work cadence, and
                    // only after frontier init so persist does not pay the
                    // write-lock fight again.
                    rollup_source_persisted = false;
                    rollup_tick.as_mut().reset(Instant::now());
                }
            }
            () = &mut rollup_tick => {
                // Advance one dirty day at a bounded idle cadence. Control and
                // ordinary observations remain prioritized above maintenance.
                let outcome = run_one_rollup_maintenance(
                    &db,
                    &identity,
                    state.deadlines.persistence,
                    &mut progress.rollup_frontier_initialized,
                )
                .await;
                rollup_tick
                    .as_mut()
                    .reset(Instant::now() + rollup_rebuild_delay(outcome));
            }
        }
    }
    state.lifecycle.store(PRODUCER_STOPPED, Ordering::Release);
}

fn should_wake_rollup_now(
    frontier_initialized: bool,
    rollup_source_persisted: bool,
    queue_empty: bool,
) -> bool {
    frontier_initialized && rollup_source_persisted && queue_empty
}

fn observation_dirties_rollup(observation: &QueuedObservation) -> bool {
    observation.carried_drop.is_some()
        || observation.envelope.event_kind == TELEMETRY_DROP_EVENT_KIND
        || EXECUTION_TOPOLOGY_EVENT_KINDS_V1.contains(&observation.envelope.event_kind.as_str())
}

fn rollup_rebuild_delay(outcome: RollupAdvanceOutcome) -> Duration {
    match outcome {
        RollupAdvanceOutcome::Progressed => ROLLUP_BACKLOG_REBUILD_INTERVAL,
        RollupAdvanceOutcome::None | RollupAdvanceOutcome::Deferred => ROLLUP_IDLE_RETRY_INTERVAL,
    }
}

async fn settle_worker(
    db: &RegisteredGlobalDb,
    identity: &ObservabilityProducerIdentityV1,
    data: &mut mpsc::Receiver<QueuedObservation>,
    state: &ProducerWorkerState,
    progress: &mut ProducerWorkerProgress,
    discard_pending: bool,
    clean_shutdown_observed: bool,
) -> u64 {
    data.close();
    if discard_pending {
        let mut ranges = Vec::new();
        while let Ok(observation) = data.try_recv() {
            if observation
                .owner_fact
                .as_ref()
                .is_some_and(|owner| owner.durable_claimed)
            {
                // Durable owner facts remain pending for the next mounted
                // producer; cancellation never relabels them as loss.
                continue;
            }
            if let Some(carried_drop) = observation.carried_drop {
                push_drop_range(&mut ranges, carried_drop);
            }
            let sequence = if observation.owner_fact.is_some() {
                state.next_sequence.fetch_add(1, Ordering::AcqRel)
            } else {
                observation.envelope.producer_sequence
            };
            state.total_dropped.fetch_add(1, Ordering::AcqRel);
            push_drop_range(
                &mut ranges,
                DropRange {
                    first: sequence,
                    last: sequence,
                    count: 1,
                },
            );
        }
        if let Some(pending) = take_pending_drop(state) {
            push_drop_range(&mut ranges, pending);
        }
        for range in ranges {
            let drop_envelope = telemetry_drop_envelope(identity, range, false);
            record(
                db,
                drop_envelope,
                &mut progress.persisted,
                &mut progress.first_error,
                state.deadlines.persistence,
            )
            .await;
        }
    } else {
        while let Some(observation) = data.recv().await {
            record_queued(
                db,
                identity,
                &state.durable_emission_lock,
                &state.next_sequence,
                observation,
                progress,
                state.deadlines.persistence,
            )
            .await;
        }
        recover_pending(
            db,
            identity,
            data,
            &state.durable_emission_lock,
            progress,
            state.deadlines.persistence,
        )
        .await;
        let pending = take_pending_drop(state);
        let had_pending = pending.is_some();
        if let Some(pending) = pending {
            let closes_cleanly = clean_shutdown_observed && progress.first_error.is_none();
            let drop_envelope = telemetry_drop_envelope(identity, pending, closes_cleanly);
            record(
                db,
                drop_envelope,
                &mut progress.persisted,
                &mut progress.first_error,
                state.deadlines.persistence,
            )
            .await;
        }
        if clean_shutdown_observed && (!had_pending || progress.first_error.is_some()) {
            let sequence = state.next_sequence.fetch_add(1, Ordering::AcqRel);
            // TelemetryDrop is Plan 26's reserved terminal carrier. A zero
            // lower bound seals this sequence without asserting a drop.
            let zero_terminal = telemetry_drop_envelope(
                identity,
                DropRange {
                    first: sequence,
                    last: sequence,
                    count: 0,
                },
                progress.first_error.is_none(),
            );
            record(
                db,
                zero_terminal,
                &mut progress.persisted,
                &mut progress.first_error,
                state.deadlines.persistence,
            )
            .await;
        }
        let _ = run_one_rollup_maintenance(
            db,
            identity,
            state.deadlines.persistence,
            &mut progress.rollup_frontier_initialized,
        )
        .await;
    }
    state.total_dropped.load(Ordering::Acquire)
}

fn take_pending_drop(state: &ProducerWorkerState) -> Option<DropRange> {
    let count = state.pending_dropped.swap(0, Ordering::AcqRel);
    (count > 0).then(|| DropRange {
        first: state.first_missing_sequence.swap(0, Ordering::AcqRel),
        last: state.last_missing_sequence.swap(0, Ordering::AcqRel),
        count,
    })
}

fn push_drop_range(ranges: &mut Vec<DropRange>, range: DropRange) {
    if ranges.last_mut().is_some_and(|last| last.merge(range)) {
        return;
    }
    ranges.push(range);
}

async fn record_queued(
    db: &RegisteredGlobalDb,
    identity: &ObservabilityProducerIdentityV1,
    durable_emission_lock: &AsyncMutex<()>,
    next_sequence: &AtomicU64,
    observation: QueuedObservation,
    progress: &mut ProducerWorkerProgress,
    persistence_deadline: Duration,
) {
    if let Some(owner_fact) = observation.owner_fact {
        let _durable_guard = durable_emission_lock.lock().await;
        if owner_fact.durable_claimed {
            settle_claimed_durable(
                db,
                observation.envelope,
                owner_fact.json,
                &mut progress.persisted,
                &mut progress.first_error,
                persistence_deadline,
            )
            .await;
        } else {
            claim_and_settle_durable(
                db,
                identity,
                next_sequence,
                observation.envelope,
                owner_fact.json,
                progress,
                persistence_deadline,
            )
            .await;
        }
        return;
    }
    if let Some(range) = observation.carried_drop {
        let drop_envelope = telemetry_drop_envelope(identity, range, false);
        record(
            db,
            drop_envelope,
            &mut progress.persisted,
            &mut progress.first_error,
            persistence_deadline,
        )
        .await;
    }
    record(
        db,
        observation.envelope,
        &mut progress.persisted,
        &mut progress.first_error,
        persistence_deadline,
    )
    .await;
}

fn prepare_delivery_with_identity(
    identity: &ObservabilityProducerIdentityV1,
    mut envelope: ObservabilityEnvelopeV1,
    sequence: u64,
    delayed: bool,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    envelope.process_boot_id = identity.process_boot_id.clone();
    envelope.producer_revision = identity.producer_revision.clone();
    envelope.configuration_revision = identity.configuration_revision.clone();
    envelope.policy_revision = identity.policy_revision.clone();
    envelope.producer_sequence = sequence;
    envelope.watermark = format!("{}:{sequence}", identity.process_boot_id);
    if delayed {
        mark_delivery_delayed(&mut envelope);
    }
    envelope.validate()?;
    Ok(envelope)
}

fn payload_safe_label(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'-' | b'_'))
}

async fn record(
    db: &RegisteredGlobalDb,
    envelope: ObservabilityEnvelopeV1,
    persisted: &mut u64,
    first_error: &mut Option<ApplicationContractError>,
    persistence_deadline: Duration,
) {
    match timeout(persistence_deadline, record_observability(db, envelope)).await {
        Ok(Ok(_)) => *persisted = persisted.saturating_add(1),
        Ok(Err(error)) if first_error.is_none() => *first_error = Some(error),
        Err(_) if first_error.is_none() => {
            *first_error = Some(ApplicationContractError::Domain(
                "observability_persistence_deadline".to_owned(),
            ));
        }
        Ok(Err(_)) | Err(_) => {}
    }
}

fn telemetry_drop_envelope(
    identity: &ObservabilityProducerIdentityV1,
    range: DropRange,
    clean_shutdown_observed: bool,
) -> ObservabilityEnvelopeV1 {
    let first_missing = range.first.max(1);
    let last_missing = range.last.max(first_missing);
    let observed_at = now_micros().0;
    let payload = ObservabilityPayloadV1::TelemetryDrop(TelemetryDropObservedV1 {
        first_missing_sequence: first_missing,
        last_missing_sequence: last_missing,
        proved_drop_lower_bound: range
            .count
            .min(last_missing.saturating_sub(first_missing).saturating_add(1)),
        clean_shutdown_observed,
    });
    ObservabilityEnvelopeV1 {
        event_id: format!(
            "{}:drop:{first_missing}:{last_missing}",
            identity.process_boot_id
        ),
        event_kind: payload.event_kind().to_owned(),
        schema_revision: 1,
        idempotency_key: format!(
            "{}:drop:{first_missing}:{last_missing}",
            identity.process_boot_id
        ),
        trace_id: identity.process_boot_id.clone(),
        scope_ref: identity.authorized_scope_ref.clone(),
        capability: "observability".to_owned(),
        operation: "drop".to_owned(),
        event_time_micros: observed_at,
        observation_time_micros: observed_at,
        valid_from_micros: None,
        valid_until_micros: None,
        quantity: Some(range.count as f64),
        unit: Some("events".to_owned()),
        terminal_result: Some(if range.count > 0 {
            ObservabilityTerminalResultV1::Partial
        } else if clean_shutdown_observed {
            ObservabilityTerminalResultV1::Succeeded
        } else {
            ObservabilityTerminalResultV1::Unknown
        }),
        producer_revision: identity.producer_revision.clone(),
        configuration_revision: identity.configuration_revision.clone(),
        policy_revision: identity.policy_revision.clone(),
        watermark: format!("{}:{last_missing}", identity.process_boot_id),
        coverage: if range.count > 0 {
            CoverageStateV1::Partial
        } else if clean_shutdown_observed {
            CoverageStateV1::Known
        } else {
            CoverageStateV1::Unknown
        },
        sampling_probability: None,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count: range.count,
        process_boot_id: identity.process_boot_id.clone(),
        producer_sequence: last_missing,
        payload,
    }
}

#[cfg(test)]
mod cheaper_frontier_tests {
    use super::should_wake_rollup_now;

    #[test]
    fn persist_wake_does_not_retry_uninitialized_frontier() {
        assert!(
            !should_wake_rollup_now(false, true, true),
            "uninitialized frontier must wait for idle, not persist-wake"
        );
        assert!(should_wake_rollup_now(true, true, true));
        assert!(!should_wake_rollup_now(true, true, false));
    }
}
