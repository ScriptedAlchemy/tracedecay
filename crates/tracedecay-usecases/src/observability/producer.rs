use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracedecay_application::{ApplicationContractError, now_micros};
use tracedecay_domain::{
    CoverageStateV1, ObservabilityEnvelopeV1, ObservabilityPayloadV1,
    ObservabilityRetentionClassV1, ObservabilityTerminalResultV1, TelemetryDropObservedV1,
};
use tracedecay_global_db::RegisteredGlobalDb;

use crate::event_lane::record_observability;

const PRODUCER_RUNNING: u8 = 0;
const PRODUCER_STOPPING: u8 = 1;
const PRODUCER_STOPPED: u8 = 2;
const MAX_PRODUCER_CAPACITY: usize = 1_024;
const MAX_PRODUCER_DEADLINE: Duration = Duration::from_secs(60);

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
}

struct ProducerWorkerState {
    pending_dropped: Arc<AtomicU64>,
    total_dropped: Arc<AtomicU64>,
    first_missing_sequence: Arc<AtomicU64>,
    last_missing_sequence: Arc<AtomicU64>,
    lifecycle: Arc<AtomicU8>,
    deadlines: ObservabilityProducerDeadlinesV1,
}

struct ProducerWorkerProgress {
    persisted: u64,
    first_error: Option<ApplicationContractError>,
}

pub struct BoundedObservabilityProducerV1 {
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
    worker: Option<JoinHandle<()>>,
}

impl BoundedObservabilityProducerV1 {
    pub fn start(
        db: Arc<RegisteredGlobalDb>,
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
        db: Arc<RegisteredGlobalDb>,
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
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| "observability_producer_runtime_unavailable")?;
        let worker = runtime.spawn(run_worker(
            db,
            identity.clone(),
            data_rx,
            control_rx,
            ProducerWorkerState {
                pending_dropped: Arc::clone(&pending_dropped),
                total_dropped: Arc::clone(&total_dropped),
                first_missing_sequence: Arc::clone(&first_missing_sequence),
                last_missing_sequence: Arc::clone(&last_missing_sequence),
                lifecycle: Arc::clone(&state),
                deadlines,
            },
        ));
        Ok(Self {
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
            worker: Some(worker),
        })
    }

    pub fn try_emit(
        &self,
        mut envelope: ObservabilityEnvelopeV1,
    ) -> Result<ObservabilityEmissionOutcomeV1, &'static str> {
        let _emission_guard = self
            .emission_lock
            .lock()
            .map_err(|_| "observability_producer_lock_poisoned")?;
        if self.state.load(Ordering::Acquire) != PRODUCER_RUNNING {
            return Err("observability_producer_closed");
        }
        if envelope.scope_ref != self.identity.authorized_scope_ref
            || envelope.process_boot_id != self.identity.process_boot_id
            || envelope.producer_revision != self.identity.producer_revision
            || envelope.configuration_revision != self.identity.configuration_revision
            || envelope.policy_revision != self.identity.policy_revision
        {
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
        envelope.validate()?;
        let sequence = self.next_sequence.fetch_add(1, Ordering::AcqRel);
        envelope.producer_sequence = sequence;
        envelope.watermark = format!("{}:{sequence}", self.identity.process_boot_id);
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
                });
                Ok(ObservabilityEmissionOutcomeV1::Enqueued)
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
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
                Ok(ObservabilityEmissionOutcomeV1::DroppedAtCapacity)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err("observability_producer_closed"),
        }
    }

    pub async fn shutdown(
        &mut self,
    ) -> Result<ObservabilityProducerSummaryV1, ApplicationContractError> {
        self.stop(false).await
    }

    pub async fn cancel(
        &mut self,
    ) -> Result<ObservabilityProducerSummaryV1, ApplicationContractError> {
        self.stop(true).await
    }

    async fn stop(
        &mut self,
        cancelled: bool,
    ) -> Result<ObservabilityProducerSummaryV1, ApplicationContractError> {
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
        let (reply, result) = oneshot::channel();
        self.control
            .try_send(ProducerControl::Shutdown { cancelled, reply })
            .map_err(|_| {
                ApplicationContractError::Domain("observability_control_lane_closed".to_owned())
            })?;
        let outcome = match timeout(self.deadlines.shutdown, result).await {
            Ok(result) => result.map_err(|_| {
                ApplicationContractError::Domain("observability_worker_stopped".to_owned())
            })?,
            Err(_) => {
                if let Some(worker) = self.worker.take() {
                    worker.abort();
                    let _ = worker.await;
                }
                self.state.store(PRODUCER_STOPPED, Ordering::Release);
                return Err(ApplicationContractError::Domain(
                    "observability_shutdown_deadline".to_owned(),
                ));
            }
        };
        if let Some(worker) = self.worker.take() {
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
    db: Arc<RegisteredGlobalDb>,
    identity: ObservabilityProducerIdentityV1,
    mut data: mpsc::Receiver<QueuedObservation>,
    mut control: mpsc::Receiver<ProducerControl>,
    state: ProducerWorkerState,
) {
    let mut progress = ProducerWorkerProgress {
        persisted: 0,
        first_error: None,
    };
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
                record_queued(
                    &db,
                    &identity,
                    observation,
                    &mut progress.persisted,
                    &mut progress.first_error,
                    state.deadlines.persistence,
                )
                .await;
            }
        }
    }
    state.lifecycle.store(PRODUCER_STOPPED, Ordering::Release);
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
            if let Some(carried_drop) = observation.carried_drop {
                push_drop_range(&mut ranges, carried_drop);
            }
            let sequence = observation.envelope.producer_sequence;
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
                observation,
                &mut progress.persisted,
                &mut progress.first_error,
                state.deadlines.persistence,
            )
            .await;
        }
        if let Some(pending) = take_pending_drop(state) {
            let drop_envelope = telemetry_drop_envelope(identity, pending, clean_shutdown_observed);
            record(
                db,
                drop_envelope,
                &mut progress.persisted,
                &mut progress.first_error,
                state.deadlines.persistence,
            )
            .await;
        }
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
    observation: QueuedObservation,
    persisted: &mut u64,
    first_error: &mut Option<ApplicationContractError>,
    persistence_deadline: Duration,
) {
    if let Some(range) = observation.carried_drop {
        let drop_envelope = telemetry_drop_envelope(identity, range, false);
        record(
            db,
            drop_envelope,
            persisted,
            first_error,
            persistence_deadline,
        )
        .await;
    }
    record(
        db,
        observation.envelope,
        persisted,
        first_error,
        persistence_deadline,
    )
    .await;
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
        terminal_result: Some(ObservabilityTerminalResultV1::Partial),
        producer_revision: identity.producer_revision.clone(),
        configuration_revision: identity.configuration_revision.clone(),
        policy_revision: identity.policy_revision.clone(),
        watermark: format!("{}:{last_missing}", identity.process_boot_id),
        coverage: CoverageStateV1::Partial,
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
