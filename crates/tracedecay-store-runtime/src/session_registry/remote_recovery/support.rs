use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use tracedecay_contracts::RequestId;
use tracedecay_contracts::remote::recovery::{
    RecoveryAuthorityExpectationV1, RemoteRecoveryControlPortV1, RemoteRecoveryInterruptionV1,
};
use tracedecay_domain::{ManifestDigest, canonical_sha256, remote_recovery_authority_key};
use tracedecay_rusqlite_runtime::remote::RemoteRecoveryPhysicalEffectErrorV1;
use tracedecay_store::{
    RuntimeCancellationIdV1, RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1,
    RuntimeInterruptionV1, RuntimeRequestProbeV1,
};

use super::{interruption_value, observe_control};

pub(super) struct RecoveryRuntimeProbeV1 {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    interruption: Arc<AtomicU8>,
    control: Option<(Arc<dyn RemoteRecoveryControlPortV1>, RequestId)>,
    commit_started: AtomicBool,
}

impl RecoveryRuntimeProbeV1 {
    pub(super) fn new(
        request_id: &RequestId,
        interruption: Arc<AtomicU8>,
    ) -> Result<Self, RemoteRecoveryPhysicalEffectErrorV1> {
        let digest = canonical_sha256(&("tracedecay.remote-recovery-control.v1", request_id))
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
        let suffix = digest
            .hex_suffix()
            .ok_or(RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
        Ok(Self {
            cancellation: RuntimeCancellationIdentityV1 {
                cancellation_id: RuntimeCancellationIdV1::new(format!("cancellation.{suffix}"))
                    .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?,
                generation: 1,
            },
            deadline: RuntimeDeadlineV1 {
                deadline_id: RuntimeDeadlineIdV1::new(format!("deadline.{suffix}"))
                    .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?,
            },
            interruption,
            control: None,
            commit_started: AtomicBool::new(false),
        })
    }

    /// Latches the caller's cancellation or deadline whenever the writer asks.
    pub(super) fn observing(
        mut self,
        control: Arc<dyn RemoteRecoveryControlPortV1>,
        request_id: RequestId,
    ) -> Self {
        self.control = Some((control, request_id));
        self
    }
}

impl RuntimeRequestProbeV1 for RecoveryRuntimeProbeV1 {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        if let Some((control, request_id)) = &self.control {
            observe_control(control.as_ref(), request_id, &self.interruption);
        }
        match interruption_value(&self.interruption) {
            Some(RemoteRecoveryInterruptionV1::Cancelled) => Some(RuntimeInterruptionV1::Cancelled),
            Some(RemoteRecoveryInterruptionV1::DeadlineExceeded) => {
                Some(RuntimeInterruptionV1::DeadlineExceeded)
            }
            None => None,
        }
    }

    fn try_begin_commit(&self) -> bool {
        self.interruption().is_none()
            && self
                .commit_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }
}

pub(super) fn authority_key(
    expected: &RecoveryAuthorityExpectationV1,
) -> Result<ManifestDigest, RemoteRecoveryPhysicalEffectErrorV1> {
    remote_recovery_authority_key(
        &expected.brain_id,
        &expected.shard_id,
        &expected.generation_id,
    )
    .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)
}
