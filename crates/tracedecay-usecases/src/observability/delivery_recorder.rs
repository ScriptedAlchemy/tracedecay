use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval, timeout};
use tracedecay_application::ApplicationContractError;
use tracedecay_domain::DeliverySettlementV1;

use super::delivery_spool::{
    DeliveryRecorderSourceReceiptV1, DeliveryRecorderSpoolError, DeliveryRecorderSpoolV1,
};
use super::{DeliverySettlementAuthorityV1, ObservabilityProducerIdentityV1};

const RECORDER_RUNNING: u8 = 0;
const RECORDER_STOPPING: u8 = 1;
const RECORDER_STOPPED: u8 = 2;
const MAX_RECORDER_CAPACITY: usize = 1_024;
const RECORDER_REPLAY_BATCH: usize = 64;
const RECORDER_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const RECORDER_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliverySettlementRecordOutcomeV1 {
    Enqueued,
    DroppedAtCapacity,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DeliverySettlementRecorderSummaryV1 {
    pub settled: u64,
    pub failed: u64,
    pub retained: u64,
}

enum RecorderControl {
    Shutdown {
        reply: oneshot::Sender<DeliverySettlementRecorderSummaryV1>,
    },
}

/// Store-owner state shared by every linked root's admission frontend: the
/// spool, wake lane, lifecycle, and worker exist exactly once per started
/// recorder. Frontends are cheap identity carriers over one `Arc` of this
/// core, so aliasing is handle construction and shutdown drains the one
/// worker no matter which frontend drives it.
struct DeliverySettlementRecorderCoreV1 {
    /// The founding owner identity every alias must match on the
    /// store-authority fields; frontends stamp their own policy revision.
    identity: ObservabilityProducerIdentityV1,
    wake: mpsc::Sender<()>,
    control: mpsc::Sender<RecorderControl>,
    // `state` and `spool` stay `Arc` because the spawned worker shares them.
    // The worker must not hold the core itself: the wake sender lives in the
    // core, so a worker-held core could never observe channel closure.
    state: Arc<AtomicU8>,
    spool: Arc<DeliveryRecorderSpoolV1>,
    admission: Mutex<()>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

/// Bounded, daemon-owned durable write-behind lane for post-delivery receipts.
///
/// Surface adapters offer only outcomes they observed at a real write, flush,
/// or acknowledgement boundary. Admission synchronously publishes the exact
/// receipt to a project-local spool before signaling the bounded worker. Queue
/// pressure therefore delays SQLite work without erasing delivery evidence;
/// transient failures remain replayable across process restart.
pub struct BoundedDeliverySettlementRecorderV1 {
    core: Arc<DeliverySettlementRecorderCoreV1>,
    /// The identity stamped on receipts admitted through this frontend. It
    /// differs from the core owner identity only in policy provenance.
    identity: ObservabilityProducerIdentityV1,
}

impl BoundedDeliverySettlementRecorderV1 {
    pub fn start(
        authority: Arc<DeliverySettlementAuthorityV1>,
        capacity: usize,
    ) -> Result<Self, &'static str> {
        if capacity == 0 || capacity > MAX_RECORDER_CAPACITY {
            return Err("delivery_settlement_recorder_capacity");
        }
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| "delivery_settlement_recorder_runtime_unavailable")?;
        let spool = Arc::new(
            DeliveryRecorderSpoolV1::open(authority.spool_root()).map_err(map_spool_start_error)?,
        );
        let (wake, wake_rx) = mpsc::channel(capacity);
        let (control, control_rx) = mpsc::channel(1);
        let state = Arc::new(AtomicU8::new(RECORDER_RUNNING));
        let worker_state = Arc::clone(&state);
        let worker_spool = Arc::clone(&spool);
        let identity = authority.identity().clone();
        let worker = runtime.spawn(run_recorder(
            Arc::clone(&authority),
            worker_spool,
            wake_rx,
            control_rx,
            worker_state,
        ));
        let core = Arc::new(DeliverySettlementRecorderCoreV1 {
            identity: identity.clone(),
            wake,
            control,
            state,
            spool,
            admission: Mutex::new(()),
            worker: Mutex::new(Some(worker)),
        });
        Ok(Self { core, identity })
    }

    /// Attach one linked root's policy-specific admission frontend to this
    /// recorder's shared core: one spool, wake lane, lifecycle, and worker.
    pub fn alias_with_policy_identity(
        &self,
        identity: ObservabilityProducerIdentityV1,
    ) -> Result<Self, &'static str> {
        identity.validate()?;
        if !identity.is_policy_alias_of(&self.core.identity) {
            return Err("delivery_settlement_recorder_alias_identity");
        }
        Ok(Self {
            core: Arc::clone(&self.core),
            identity,
        })
    }

    pub fn try_record(
        &self,
        settlement: DeliverySettlementV1,
    ) -> Result<DeliverySettlementRecordOutcomeV1, &'static str> {
        settlement.validate()?;
        let receipt = DeliveryRecorderSourceReceiptV1::new(settlement, self.identity.clone())
            .map_err(map_spool_admission_error)?;
        let _admission = self
            .core
            .admission
            .lock()
            .map_err(|_| "delivery_settlement_recorder_lock_poisoned")?;
        if self.core.state.load(Ordering::Acquire) != RECORDER_RUNNING {
            return Err("delivery_settlement_recorder_closed");
        }
        match self.core.spool.append(&receipt) {
            Ok(_) => {}
            Err(DeliveryRecorderSpoolError::Full) => {
                return Ok(DeliverySettlementRecordOutcomeV1::DroppedAtCapacity);
            }
            Err(error) => return Err(map_spool_admission_error(error)),
        }
        // A full wake queue is safe: the durable receipt is already visible to
        // the active worker's bounded scan and periodic restart replay.
        match self.core.wake.try_send(()) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(())) => {
                Ok(DeliverySettlementRecordOutcomeV1::Enqueued)
            }
            Err(mpsc::error::TrySendError::Closed(())) => {
                Ok(DeliverySettlementRecordOutcomeV1::Enqueued)
            }
        }
    }

    pub async fn shutdown(
        &self,
    ) -> Result<DeliverySettlementRecorderSummaryV1, ApplicationContractError> {
        self.core.shutdown().await
    }
}

impl DeliverySettlementRecorderCoreV1 {
    /// Shutdown lives only on the core: any frontend may drive it, and the
    /// lifecycle compare-and-swap admits exactly one drain.
    async fn shutdown(
        &self,
    ) -> Result<DeliverySettlementRecorderSummaryV1, ApplicationContractError> {
        {
            let _admission = self.admission.lock().map_err(|_| {
                ApplicationContractError::Domain(
                    "delivery_settlement_recorder_lock_poisoned".to_owned(),
                )
            })?;
            self.state
                .compare_exchange(
                    RECORDER_RUNNING,
                    RECORDER_STOPPING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .map_err(|_| {
                    ApplicationContractError::Domain(
                        "delivery_settlement_recorder_closed".to_owned(),
                    )
                })?;
        }
        let (reply, result) = oneshot::channel();
        self.control
            .try_send(RecorderControl::Shutdown { reply })
            .map_err(|_| {
                ApplicationContractError::Domain(
                    "delivery_settlement_recorder_control_closed".to_owned(),
                )
            })?;
        let worker = self
            .worker
            .lock()
            .map_err(|_| {
                ApplicationContractError::Domain(
                    "delivery_settlement_recorder_lock_poisoned".to_owned(),
                )
            })?
            .take();
        let summary = match timeout(RECORDER_SHUTDOWN_DEADLINE, result).await {
            Ok(Ok(summary)) => summary,
            Ok(Err(_)) => {
                return Err(ApplicationContractError::Domain(
                    "delivery_settlement_recorder_stopped".to_owned(),
                ));
            }
            Err(_) => {
                if let Some(worker) = worker {
                    worker.abort();
                    let _ = worker.await;
                }
                self.state.store(RECORDER_STOPPED, Ordering::Release);
                return Err(ApplicationContractError::Domain(
                    "delivery_settlement_recorder_shutdown_deadline".to_owned(),
                ));
            }
        };
        if let Some(worker) = worker {
            worker.await.map_err(|error| {
                ApplicationContractError::Domain(format!(
                    "delivery settlement recorder join failed: {error}"
                ))
            })?;
        }
        Ok(summary)
    }
}

async fn run_recorder(
    authority: Arc<DeliverySettlementAuthorityV1>,
    spool: Arc<DeliveryRecorderSpoolV1>,
    mut wake: mpsc::Receiver<()>,
    mut control: mpsc::Receiver<RecorderControl>,
    state: Arc<AtomicU8>,
) {
    let mut summary = DeliverySettlementRecorderSummaryV1::default();
    let mut retry = interval(RECORDER_RETRY_INTERVAL);
    retry.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            Some(()) = wake.recv() => {
                drain_once(authority.as_ref(), spool.as_ref(), &mut summary).await;
            }
            _ = retry.tick() => {
                drain_once(authority.as_ref(), spool.as_ref(), &mut summary).await;
            }
            Some(RecorderControl::Shutdown { reply }) = control.recv() => {
                while wake.try_recv().is_ok() {}
                while drain_once(authority.as_ref(), spool.as_ref(), &mut summary).await > 0 {}
                summary.retained = pending_count(spool.as_ref());
                state.store(RECORDER_STOPPED, Ordering::Release);
                let _ = reply.send(summary);
                return;
            }
            else => {
                state.store(RECORDER_STOPPED, Ordering::Release);
                return;
            }
        }
    }
}

async fn drain_once(
    authority: &DeliverySettlementAuthorityV1,
    spool: &DeliveryRecorderSpoolV1,
    summary: &mut DeliverySettlementRecorderSummaryV1,
) -> usize {
    let receipts = match spool.pending(RECORDER_REPLAY_BATCH) {
        Ok(receipts) => receipts,
        Err(error) => {
            summary.failed = summary.failed.saturating_add(1);
            tracing::warn!(%error, "delivery settlement recorder could not read durable receipts");
            return 0;
        }
    };
    let mut acknowledged = 0_usize;
    for receipt in receipts {
        let result = async {
            let receipt_authority = match receipt.emission_identity.as_ref() {
                Some(identity) => authority.alias_for_durable_replay(identity),
                None => authority.alias_with_policy_identity(authority.identity().clone()),
            }
            .map_err(|error| ApplicationContractError::Domain(error.to_owned()))?;
            receipt_authority.begin(&receipt.settlement.attempt).await?;
            receipt_authority.settle(&receipt.settlement).await?;
            spool
                .acknowledge(receipt.receipt_id)
                .map_err(|error| ApplicationContractError::Domain(error.to_string()))?;
            Ok::<(), ApplicationContractError>(())
        }
        .await;
        if let Err(error) = result {
            summary.failed = summary.failed.saturating_add(1);
            tracing::warn!(%error, "delivery settlement recorder retained receipt for retry");
        } else {
            summary.settled = summary.settled.saturating_add(1);
            acknowledged = acknowledged.saturating_add(1);
        }
    }
    acknowledged
}

fn pending_count(spool: &DeliveryRecorderSpoolV1) -> u64 {
    match spool.len() {
        Ok(pending) => u64::try_from(pending).unwrap_or(u64::MAX),
        Err(_) => u64::MAX,
    }
}

const fn map_spool_start_error(error: DeliveryRecorderSpoolError) -> &'static str {
    match error {
        DeliveryRecorderSpoolError::Busy => "delivery_settlement_recorder_already_running",
        DeliveryRecorderSpoolError::Full => "delivery_settlement_recorder_spool_full",
        DeliveryRecorderSpoolError::UnsafePath => "delivery_settlement_recorder_spool_unsafe",
        DeliveryRecorderSpoolError::Corrupt => "delivery_settlement_recorder_spool_corrupt",
        DeliveryRecorderSpoolError::Io => "delivery_settlement_recorder_spool_io",
        DeliveryRecorderSpoolError::InvalidReceipt => "delivery_settlement_recorder_spool_invalid",
        DeliveryRecorderSpoolError::LockPoisoned => {
            "delivery_settlement_recorder_spool_lock_poisoned"
        }
    }
}

const fn map_spool_admission_error(error: DeliveryRecorderSpoolError) -> &'static str {
    match error {
        DeliveryRecorderSpoolError::Full => "delivery_settlement_recorder_spool_full",
        DeliveryRecorderSpoolError::Busy => "delivery_settlement_recorder_spool_busy",
        DeliveryRecorderSpoolError::UnsafePath => "delivery_settlement_recorder_spool_unsafe",
        DeliveryRecorderSpoolError::Corrupt => "delivery_settlement_recorder_spool_corrupt",
        DeliveryRecorderSpoolError::Io => "delivery_settlement_recorder_spool_io",
        DeliveryRecorderSpoolError::InvalidReceipt => "delivery_settlement_recorder_spool_invalid",
        DeliveryRecorderSpoolError::LockPoisoned => {
            "delivery_settlement_recorder_spool_lock_poisoned"
        }
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::{
        DeliveryChannelIdentityV1, DeliveryEventClassV1, DeliverySettlementAttemptV1,
        DeliverySettlementOutcomeV1, DeliverySurfaceFamilyV1, ProjectId, UtcMicros,
        canonical_sha256,
    };

    use super::super::BoundedObservabilityProducerV1;
    use super::*;

    fn legacy_receipt_id(settlement: &DeliverySettlementV1) -> [u8; 16] {
        let digest =
            canonical_sha256(&("tracedecay.delivery-recorder-source-receipt.v1", settlement))
                .expect("legacy receipt digest");
        let hex = digest
            .as_str()
            .strip_prefix("sha256:")
            .expect("canonical digest prefix");
        let mut receipt_id = [0_u8; 16];
        for (index, slot) in receipt_id.iter_mut().enumerate() {
            let offset = index * 2;
            *slot = u8::from_str_radix(&hex[offset..offset + 2], 16).expect("canonical digest hex");
        }
        receipt_id
    }

    #[tokio::test]
    async fn legacy_v1_receipt_replays_through_current_process_identity() {
        let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project");
        let project_id = ProjectId::new("project.delivery.legacy-replay").expect("project id");
        let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
            tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
            project.path(),
            project_id.clone(),
        )
        .await
        .expect("registered runtime");
        let db = runtime.project_database_arc().expect("project database");
        let identity = ObservabilityProducerIdentityV1 {
            authorized_scope_ref: project_id.as_str().to_owned(),
            process_boot_id: "boot:delivery-legacy-replay".to_owned(),
            producer_revision: "delivery-legacy-replay-producer.v1".to_owned(),
            configuration_revision: "delivery-legacy-replay-config.v1".to_owned(),
            policy_revision: "delivery-legacy-replay-policy.v1".to_owned(),
        };
        let producer = Arc::new(
            BoundedObservabilityProducerV1::start(db.clone(), identity.clone(), 8)
                .expect("producer"),
        );
        let authority = Arc::new(
            DeliverySettlementAuthorityV1::new(db, Arc::clone(&producer), identity)
                .expect("settlement authority"),
        );
        let settlement = DeliverySettlementV1 {
            attempt: DeliverySettlementAttemptV1 {
                owner_event_id: "work:delivery-legacy-replay".to_owned(),
                event_class: DeliveryEventClassV1::OperationTerminal,
                channel: DeliveryChannelIdentityV1 {
                    surface: DeliverySurfaceFamilyV1::Mcp,
                    channel_ref: "mcp:delivery-legacy-replay".to_owned(),
                },
                work_attempt: None,
                eligible: 1,
                valid_at: UtcMicros(100),
                attempted_at: UtcMicros(110),
            },
            outcome: DeliverySettlementOutcomeV1::Delivered,
            settled_at: UtcMicros(120),
            drop_reason: None,
        };
        let spool = DeliveryRecorderSpoolV1::open(authority.spool_root()).expect("spool");
        spool
            .append(&DeliveryRecorderSourceReceiptV1 {
                receipt_id: legacy_receipt_id(&settlement),
                settlement,
                emission_identity: None,
            })
            .expect("append legacy receipt");
        let mut summary = DeliverySettlementRecorderSummaryV1::default();

        assert_eq!(
            drain_once(authority.as_ref(), &spool, &mut summary).await,
            1
        );
        assert_eq!(summary.settled, 1);
        assert_eq!(spool.len().expect("pending receipts"), 0);

        drop(spool);
        drop(authority);
        let producer = Arc::try_unwrap(producer)
            .unwrap_or_else(|_| panic!("authority must release producer after replay"));
        producer.shutdown().await.expect("flush producer");
    }
}
