use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
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

enum ProducerControl {
    Shutdown {
        cancelled: bool,
        reply: oneshot::Sender<Result<ObservabilityProducerSummaryV1, ApplicationContractError>>,
    },
}

struct ProducerWorkerState {
    dropped: Arc<AtomicU64>,
    first_missing_sequence: Arc<AtomicU64>,
    last_missing_sequence: Arc<AtomicU64>,
    next_sequence: Arc<AtomicU64>,
    lifecycle: Arc<AtomicU8>,
}

struct ProducerWorkerProgress {
    persisted: u64,
    first_error: Option<ApplicationContractError>,
}

pub struct BoundedObservabilityProducerV1 {
    identity: ObservabilityProducerIdentityV1,
    data: mpsc::Sender<ObservabilityEnvelopeV1>,
    control: mpsc::Sender<ProducerControl>,
    dropped: Arc<AtomicU64>,
    first_missing_sequence: Arc<AtomicU64>,
    last_missing_sequence: Arc<AtomicU64>,
    next_sequence: Arc<AtomicU64>,
    state: Arc<AtomicU8>,
    emission_lock: Mutex<()>,
    worker: Option<JoinHandle<()>>,
}

impl BoundedObservabilityProducerV1 {
    pub fn start(
        db: Arc<RegisteredGlobalDb>,
        identity: ObservabilityProducerIdentityV1,
        capacity: usize,
    ) -> Result<Self, &'static str> {
        identity.validate()?;
        if capacity == 0 || capacity > MAX_PRODUCER_CAPACITY {
            return Err("observability_producer_capacity");
        }
        let (data, data_rx) = mpsc::channel(capacity);
        // The control lane remains writable when every data slot is occupied.
        let (control, control_rx) = mpsc::channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
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
                dropped: Arc::clone(&dropped),
                first_missing_sequence: Arc::clone(&first_missing_sequence),
                last_missing_sequence: Arc::clone(&last_missing_sequence),
                next_sequence: Arc::clone(&next_sequence),
                lifecycle: Arc::clone(&state),
            },
        ));
        Ok(Self {
            identity,
            data,
            control,
            dropped,
            first_missing_sequence,
            last_missing_sequence,
            next_sequence,
            state,
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
                let carried_drops = self.dropped.swap(0, Ordering::AcqRel);
                if carried_drops > 0 {
                    envelope.dropped_count = envelope.dropped_count.saturating_add(carried_drops);
                    envelope.coverage = CoverageStateV1::Partial;
                    self.first_missing_sequence.store(0, Ordering::Release);
                    self.last_missing_sequence.store(0, Ordering::Release);
                }
                permit.send(envelope);
                Ok(ObservabilityEmissionOutcomeV1::Enqueued)
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::AcqRel);
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
            .send(ProducerControl::Shutdown { cancelled, reply })
            .await
            .map_err(|_| {
                ApplicationContractError::Domain("observability_control_lane_closed".to_owned())
            })?;
        let outcome = result.await.map_err(|_| {
            ApplicationContractError::Domain("observability_worker_stopped".to_owned())
        })?;
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
    mut data: mpsc::Receiver<ObservabilityEnvelopeV1>,
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
            envelope = data.recv() => {
                let Some(envelope) = envelope else {
                    break;
                };
                record(
                    &db,
                    envelope,
                    &mut progress.persisted,
                    &mut progress.first_error,
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
    data: &mut mpsc::Receiver<ObservabilityEnvelopeV1>,
    state: &ProducerWorkerState,
    progress: &mut ProducerWorkerProgress,
    discard_pending: bool,
    clean_shutdown_observed: bool,
) -> u64 {
    data.close();
    if discard_pending {
        while let Ok(envelope) = data.try_recv() {
            let sequence = envelope.producer_sequence;
            state.dropped.fetch_add(1, Ordering::AcqRel);
            let _ = state.first_missing_sequence.compare_exchange(
                0,
                sequence,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            state
                .last_missing_sequence
                .store(sequence, Ordering::Release);
        }
    } else {
        while let Some(envelope) = data.recv().await {
            record(
                db,
                envelope,
                &mut progress.persisted,
                &mut progress.first_error,
            )
            .await;
        }
    }
    let dropped_count = state.dropped.swap(0, Ordering::AcqRel);
    if dropped_count > 0 {
        let drop_envelope = telemetry_drop_envelope(
            identity,
            &state.first_missing_sequence,
            &state.last_missing_sequence,
            &state.next_sequence,
            dropped_count,
            clean_shutdown_observed,
        );
        record(
            db,
            drop_envelope,
            &mut progress.persisted,
            &mut progress.first_error,
        )
        .await;
    }
    dropped_count
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
) {
    match record_observability(db, envelope).await {
        Ok(_) => *persisted = persisted.saturating_add(1),
        Err(error) if first_error.is_none() => *first_error = Some(error),
        Err(_) => {}
    }
}

fn telemetry_drop_envelope(
    identity: &ObservabilityProducerIdentityV1,
    first_missing_sequence: &AtomicU64,
    last_missing_sequence: &AtomicU64,
    next_sequence: &AtomicU64,
    dropped_count: u64,
    clean_shutdown_observed: bool,
) -> ObservabilityEnvelopeV1 {
    let sequence = next_sequence.fetch_add(1, Ordering::AcqRel);
    let first_missing = first_missing_sequence.load(Ordering::Acquire).max(1);
    let last_missing = last_missing_sequence
        .load(Ordering::Acquire)
        .max(first_missing);
    let observed_at = now_micros().0;
    let payload = ObservabilityPayloadV1::TelemetryDrop(TelemetryDropObservedV1 {
        first_missing_sequence: first_missing,
        last_missing_sequence: last_missing,
        proved_drop_lower_bound: dropped_count
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
        quantity: Some(dropped_count as f64),
        unit: Some("events".to_owned()),
        terminal_result: Some(ObservabilityTerminalResultV1::Partial),
        producer_revision: identity.producer_revision.clone(),
        configuration_revision: identity.configuration_revision.clone(),
        policy_revision: identity.policy_revision.clone(),
        watermark: format!("{}:{sequence}", identity.process_boot_id),
        coverage: CoverageStateV1::Partial,
        sampling_probability: None,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count,
        process_boot_id: identity.process_boot_id.clone(),
        producer_sequence: sequence,
        payload,
    }
}
