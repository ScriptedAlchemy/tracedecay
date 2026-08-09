//! Retained recovery from durable Work receipts into the observability outbox.

use std::num::NonZeroU16;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval, timeout};
use tracedecay_application::{
    ApplicationContractError, PendingWorkOwnerObservationV1, WorkOwnerObservationReceiptV1,
    WorkOwnerObservationScanCursorV1, WorkOwnerObservationStoragePortV1,
};
use tracedecay_rusqlite_runtime::work::WorkSqliteStorage;

use super::BoundedObservabilityProducerV1;
use super::work_duplicate_emit::work_duplicate_observation_envelope;
use super::work_retry_leak_emit::{
    work_leak_observation_envelope, work_retry_observation_envelope,
};

const RECOVERY_RUNNING: u8 = 0;
const RECOVERY_STOPPING: u8 = 1;
const RECOVERY_STOPPED: u8 = 2;
const RECOVERY_BATCH: u16 = 256;
const RECOVERY_INTERVAL: Duration = Duration::from_secs(1);
const RECOVERY_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WorkOwnerObservationRecoverySummaryV1 {
    pub marked_durable: u64,
    pub failed: u64,
}

enum RecoveryControl {
    Shutdown {
        reply: oneshot::Sender<WorkOwnerObservationRecoverySummaryV1>,
    },
}

/// Project-owned bounded recovery worker. It quiesces before its producer so
/// no durable source marker can be advanced after observability shutdown.
pub struct WorkOwnerObservationRecoveryV1 {
    control: mpsc::Sender<RecoveryControl>,
    state: Arc<AtomicU8>,
    admission: Mutex<()>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl WorkOwnerObservationRecoveryV1 {
    pub fn start(
        storage: WorkSqliteStorage,
        producer: Arc<BoundedObservabilityProducerV1>,
    ) -> Result<Self, &'static str> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| "work_owner_observation_recovery_runtime_unavailable")?;
        let (control, control_rx) = mpsc::channel(1);
        let state = Arc::new(AtomicU8::new(RECOVERY_RUNNING));
        let worker_state = Arc::clone(&state);
        let worker = runtime.spawn(run_recovery(storage, producer, control_rx, worker_state));
        Ok(Self {
            control,
            state,
            admission: Mutex::new(()),
            worker: Mutex::new(Some(worker)),
        })
    }

    pub async fn shutdown(
        &self,
    ) -> Result<WorkOwnerObservationRecoverySummaryV1, ApplicationContractError> {
        {
            let _admission = self.admission.lock().map_err(|_| {
                ApplicationContractError::Domain(
                    "work owner-observation recovery lock poisoned".to_owned(),
                )
            })?;
            self.state
                .compare_exchange(
                    RECOVERY_RUNNING,
                    RECOVERY_STOPPING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .map_err(|_| {
                    ApplicationContractError::Domain(
                        "work owner-observation recovery closed".to_owned(),
                    )
                })?;
        }
        let (reply, result) = oneshot::channel();
        self.control
            .try_send(RecoveryControl::Shutdown { reply })
            .map_err(|_| {
                ApplicationContractError::Domain(
                    "work owner-observation recovery control closed".to_owned(),
                )
            })?;
        let worker = self
            .worker
            .lock()
            .map_err(|_| {
                ApplicationContractError::Domain(
                    "work owner-observation recovery lock poisoned".to_owned(),
                )
            })?
            .take();
        let summary = match timeout(RECOVERY_SHUTDOWN_DEADLINE, result).await {
            Ok(Ok(summary)) => summary,
            Ok(Err(_)) => {
                return Err(ApplicationContractError::Domain(
                    "work owner-observation recovery stopped".to_owned(),
                ));
            }
            Err(_) => {
                if let Some(worker) = worker {
                    worker.abort();
                    let _ = worker.await;
                }
                self.state.store(RECOVERY_STOPPED, Ordering::Release);
                return Err(ApplicationContractError::Domain(
                    "work owner-observation recovery shutdown deadline".to_owned(),
                ));
            }
        };
        if let Some(worker) = worker {
            worker.await.map_err(|error| {
                ApplicationContractError::Domain(format!(
                    "work owner-observation recovery join failed: {error}"
                ))
            })?;
        }
        Ok(summary)
    }
}

async fn run_recovery(
    storage: WorkSqliteStorage,
    producer: Arc<BoundedObservabilityProducerV1>,
    mut control: mpsc::Receiver<RecoveryControl>,
    state: Arc<AtomicU8>,
) {
    let mut schedule = interval(RECOVERY_INTERVAL);
    schedule.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut summary = WorkOwnerObservationRecoverySummaryV1::default();
    let mut cursor = None;
    loop {
        tokio::select! {
            _ = schedule.tick() => recover_batch(&storage, producer.as_ref(), &mut cursor, &mut summary).await,
            Some(RecoveryControl::Shutdown { reply }) = control.recv() => {
                recover_batch(&storage, producer.as_ref(), &mut cursor, &mut summary).await;
                state.store(RECOVERY_STOPPED, Ordering::Release);
                let _ = reply.send(summary);
                return;
            }
            else => {
                state.store(RECOVERY_STOPPED, Ordering::Release);
                return;
            }
        }
    }
}

async fn recover_batch(
    storage: &WorkSqliteStorage,
    producer: &BoundedObservabilityProducerV1,
    cursor: &mut Option<WorkOwnerObservationScanCursorV1>,
    summary: &mut WorkOwnerObservationRecoverySummaryV1,
) {
    let Some(limit) = NonZeroU16::new(RECOVERY_BATCH) else {
        summary.failed = summary.failed.saturating_add(1);
        return;
    };
    let scan_storage = storage.clone();
    let after = cursor.clone();
    let pending = match tokio::task::spawn_blocking(move || {
        scan_storage.pending_owner_observations(after.as_ref(), limit)
    })
    .await
    {
        Ok(Ok(pending)) => pending,
        Ok(Err(error)) => {
            summary.failed = summary.failed.saturating_add(1);
            tracing::warn!(%error, "Work owner-observation pending scan failed");
            return;
        }
        Err(error) => {
            summary.failed = summary.failed.saturating_add(1);
            tracing::warn!(%error, "Work owner-observation pending scan task failed");
            return;
        }
    };
    let wrapped = pending.len() < usize::from(RECOVERY_BATCH);
    for pending in pending {
        *cursor = Some(pending.scan_cursor.clone());
        recover_one(storage, producer, pending, summary).await;
    }
    if wrapped {
        *cursor = None;
    }
}

async fn recover_one(
    storage: &WorkSqliteStorage,
    producer: &BoundedObservabilityProducerV1,
    pending: PendingWorkOwnerObservationV1,
    summary: &mut WorkOwnerObservationRecoverySummaryV1,
) {
    if !pending.validate() {
        summary.failed = summary.failed.saturating_add(1);
        return;
    }
    let scope = pending.marker.authority.project_id().as_str();
    let envelope = match &pending.receipt {
        WorkOwnerObservationReceiptV1::Retry(receipt) => {
            work_retry_observation_envelope(producer.identity(), scope, receipt)
        }
        WorkOwnerObservationReceiptV1::Leak(receipt) => {
            work_leak_observation_envelope(producer.identity(), scope, receipt)
        }
        WorkOwnerObservationReceiptV1::Duplicate(receipt) => work_duplicate_observation_envelope(
            producer.identity(),
            scope,
            &pending.marker.authority,
            receipt,
        ),
    };
    let Some(envelope) = envelope else {
        summary.failed = summary.failed.saturating_add(1);
        return;
    };
    if let Err(error) = producer.emit_owner_fact(envelope).await {
        summary.failed = summary.failed.saturating_add(1);
        tracing::warn!(%error, "Work owner-observation durable claim failed");
        return;
    }
    let marker_storage = storage.clone();
    let marker = pending.marker;
    match tokio::task::spawn_blocking(move || {
        marker_storage.mark_owner_observation_durable(&marker)
    })
    .await
    {
        Ok(Ok(_)) => summary.marked_durable = summary.marked_durable.saturating_add(1),
        Ok(Err(error)) => {
            summary.failed = summary.failed.saturating_add(1);
            tracing::warn!(%error, "Work owner-observation source marker CAS failed");
        }
        Err(error) => {
            summary.failed = summary.failed.saturating_add(1);
            tracing::warn!(%error, "Work owner-observation source marker task failed");
        }
    }
}
