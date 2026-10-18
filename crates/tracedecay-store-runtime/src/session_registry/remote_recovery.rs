use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

use tracedecay_contracts::RequestId;
use tracedecay_contracts::remote::recovery::{
    PromotionCasReceiptV1, RecoveryAuthorityExpectationV1, RemoteRecoveryCallerV1,
    RemoteRecoveryControlPortV1, RemoteRecoveryInterruptionV1,
};
use tracedecay_domain::{
    CurrentRemoteAuthorityV1, ProjectId, RemoteWriterFenceV1, canonical_sha256,
};
use tracedecay_rusqlite_runtime::remote::{
    RemoteRecoveryPhysicalCommitV1, RemoteRecoveryPhysicalEffectErrorV1,
    RemoteRecoveryPhysicalEffectFutureV1, RemoteRecoveryPhysicalEffectsV1,
    RemoteSqliteStorageErrorV1, RemoteSqliteStorageV1,
};
use tracedecay_store::RemoteWriterFenceInstallV1;

use super::{Result, session_registry_error};

const INTERRUPTION_NONE: u8 = 0;
const INTERRUPTION_CANCELLED: u8 = 1;
const INTERRUPTION_DEADLINE: u8 = 2;

#[cfg(test)]
mod interruption_tests;
mod support;
mod writer_publication;

use support::{RecoveryRuntimeProbeV1, authority_key};

#[derive(Clone)]
pub(super) struct DaemonRemoteRecoveryPhysicalEffectsV1 {
    storage: RemoteSqliteStorageV1,
    replay: Arc<crate::remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1>,
    project_lifecycle: Arc<OnceLock<Arc<dyn super::RemoteRecoveryProjectLifecycle>>>,
}

impl DaemonRemoteRecoveryPhysicalEffectsV1 {
    pub(super) fn new(
        storage: RemoteSqliteStorageV1,
        replay: Arc<crate::remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1>,
        project_lifecycle: Arc<OnceLock<Arc<dyn super::RemoteRecoveryProjectLifecycle>>>,
    ) -> Self {
        Self {
            storage,
            replay,
            project_lifecycle,
        }
    }

    async fn authorize_project_recovery(
        &self,
        project_id: &ProjectId,
    ) -> Result<super::RemoteRecoveryAdmission> {
        self.project_lifecycle
            .get()
            .cloned()
            .ok_or_else(|| {
                session_registry_error(
                    "authorize remote project recovery",
                    "remote recovery project lifecycle is unavailable".to_owned(),
                )
            })?
            .authorize_project_recovery(project_id)
            .await
    }
}

impl RemoteRecoveryPhysicalEffectsV1 for DaemonRemoteRecoveryPhysicalEffectsV1 {
    fn current_authority<'a>(
        &'a self,
        expected: &'a RecoveryAuthorityExpectationV1,
        caller: &'a RemoteRecoveryCallerV1,
    ) -> RemoteRecoveryPhysicalEffectFutureV1<'a, (CurrentRemoteAuthorityV1, u64)> {
        Box::pin(async move {
            let writer = self
                .storage
                .recovery_writer(expected)
                .map_err(map_writer_lookup_error)?;
            if writer.scope != caller.scope {
                return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
            }
            let project_id = writer
                .target_project_id()
                .cloned()
                .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
            let _recovery_admission = self
                .authorize_project_recovery(&project_id)
                .await
                .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
            let authority_key = authority_key(expected)?;
            match self
                .replay
                .current_writer_fence(project_id, authority_key)
                .await?
            {
                Some((fence, frontier)) if fence == writer.authority.fence => {
                    Ok((writer.authority, frontier))
                }
                _ => Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption),
            }
        })
    }

    fn required_promotion_sink_ids(
        &self,
        expected: &RecoveryAuthorityExpectationV1,
    ) -> std::result::Result<Vec<String>, RemoteRecoveryPhysicalEffectErrorV1> {
        let authority_key = authority_key(expected)?;
        Ok(vec![
            format!("remote-node.recovery-journal.{}", authority_key.as_str()),
            format!("remote-node.writer-authority.{}", authority_key.as_str()),
            format!("project-sessions.writer-fence.{}", authority_key.as_str()),
        ])
    }

    #[tracing::instrument(
        name = "daemon.session_registry.remote_recovery.promote",
        level = "trace",
        skip_all
    )]
    fn promote<'a>(
        &'a self,
        operation_id: &'a str,
        expected: &'a RecoveryAuthorityExpectationV1,
        replacement: &'a RemoteWriterFenceV1,
        required_sink_ids: &'a [String],
        caller: &'a RemoteRecoveryCallerV1,
        control: Arc<dyn RemoteRecoveryControlPortV1>,
        request_id: &'a RequestId,
    ) -> RemoteRecoveryPhysicalEffectFutureV1<
        'a,
        RemoteRecoveryPhysicalCommitV1<PromotionCasReceiptV1>,
    > {
        Box::pin(async move {
            let writer = self
                .storage
                .recovery_writer_for_lineage(expected)
                .map_err(map_writer_lookup_error)?;
            if writer.scope != caller.scope {
                return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
            }
            let project_id = writer
                .target_project_id()
                .cloned()
                .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
            let _recovery_admission = self
                .authorize_project_recovery(&project_id)
                .await
                .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
            let expected_sinks = self.required_promotion_sink_ids(expected)?;
            if expected_sinks != required_sink_ids {
                return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
            }
            let policy_digest = self
                .storage
                .recovery_policy_digest(&caller.scope)
                .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
            let authority_key = authority_key(expected)?;
            let current = remote_fence(expected)?;
            let installed_at = tracedecay_contracts::clock::now_micros();
            let (binding, _) = self
                .replay
                .target_descriptor(&project_id)
                .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
            let install = RemoteWriterFenceInstallV1 {
                project_id: project_id.clone(),
                target_binding: binding,
                authority_key: authority_key.clone(),
                expected: Some(current),
                replacement: replacement.clone(),
                installed_at,
            };
            let interruption = Arc::new(AtomicU8::new(INTERRUPTION_NONE));
            let probe = Arc::new(
                RecoveryRuntimeProbeV1::new(request_id, Arc::clone(&interruption))?
                    .observing(Arc::clone(&control), request_id.clone()),
            );
            let receipt = self
                .replay
                .install_writer_fence(project_id.clone(), install, probe)
                .await?;
            observe_control(control.as_ref(), request_id, &interruption);
            let (_, published_frontier_sequence) = self
                .replay
                .current_writer_fence(project_id, authority_key)
                .await?
                .filter(|(fence, _)| fence == replacement)
                .ok_or(RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
            let receipt_id = format!("remote.promotion.{}", safe_suffix(operation_id)?);
            let output = PromotionCasReceiptV1 {
                receipt_id,
                preview_id: operation_id.to_owned(),
                previous_epoch: expected.authority_epoch,
                installed_epoch: replacement.authority_epoch.0,
                installed_placement_revision: replacement.placement_revision.get(),
                installed_sink_ids: required_sink_ids.to_vec(),
                published_frontier_sequence,
                old_authority_fenced: true,
            };
            let bytes_consumed = u64::try_from(
                serde_json::to_vec(&(&output, &receipt))
                    .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?
                    .len(),
            )
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?
            .max(1);
            Ok(RemoteRecoveryPhysicalCommitV1 {
                committed_state_digest: canonical_sha256(&(&output, &receipt))
                    .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?,
                output,
                policy_digest,
                committed_at: receipt.committed_at,
                units_consumed: 1,
                bytes_consumed,
                interruption_observed_after_commit: interruption_value(&interruption),
            })
        })
    }
}

fn map_writer_lookup_error(
    error: RemoteSqliteStorageErrorV1,
) -> RemoteRecoveryPhysicalEffectErrorV1 {
    match error {
        RemoteSqliteStorageErrorV1::WriterAuthorityUnpublished => {
            RemoteRecoveryPhysicalEffectErrorV1::WriterAuthorityUnpublished
        }
        _ => RemoteRecoveryPhysicalEffectErrorV1::Unavailable,
    }
}

fn observe_control(
    control: &dyn RemoteRecoveryControlPortV1,
    request_id: &RequestId,
    interruption: &Arc<AtomicU8>,
) {
    let value = match control.interruption(request_id) {
        Some(RemoteRecoveryInterruptionV1::Cancelled) => INTERRUPTION_CANCELLED,
        Some(RemoteRecoveryInterruptionV1::DeadlineExceeded) => INTERRUPTION_DEADLINE,
        None => INTERRUPTION_NONE,
    };
    if value != INTERRUPTION_NONE {
        match interruption.compare_exchange(
            INTERRUPTION_NONE,
            value,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) | Err(INTERRUPTION_CANCELLED | INTERRUPTION_DEADLINE) => {}
            Err(_) => tracing::warn!("remote recovery interruption state is invalid"),
        }
    }
}

fn interruption_value(interruption: &Arc<AtomicU8>) -> Option<RemoteRecoveryInterruptionV1> {
    match interruption.load(Ordering::Acquire) {
        INTERRUPTION_CANCELLED => Some(RemoteRecoveryInterruptionV1::Cancelled),
        INTERRUPTION_DEADLINE => Some(RemoteRecoveryInterruptionV1::DeadlineExceeded),
        _ => None,
    }
}

fn remote_fence(
    expected: &RecoveryAuthorityExpectationV1,
) -> std::result::Result<RemoteWriterFenceV1, RemoteRecoveryPhysicalEffectErrorV1> {
    Ok(RemoteWriterFenceV1 {
        brain_id: tracedecay_domain::BrainId::new(expected.brain_id.clone())
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?,
        shard_id: tracedecay_domain::ShardId::new(expected.shard_id.clone())
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?,
        generation_id: tracedecay_domain::ProjectionGenerationId::new(
            expected.generation_id.clone(),
        )
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?,
        placement_revision: tracedecay_domain::RemotePlacementRevisionV1::new(
            expected.placement_revision,
        )
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?,
        authority_epoch: tracedecay_domain::AuthorityEpoch(expected.authority_epoch),
        authority_node_id: tracedecay_domain::BrainNodeId::new(expected.authority_node_id.clone())
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?,
    })
}

fn safe_suffix(value: &str) -> std::result::Result<&str, RemoteRecoveryPhysicalEffectErrorV1> {
    if value.is_empty()
        || value.len() > 160
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
    }
    Ok(value)
}
