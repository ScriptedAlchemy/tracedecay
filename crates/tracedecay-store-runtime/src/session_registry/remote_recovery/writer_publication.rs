//! Operator publication of a Remote Brain lineage's first writer.
//!
//! The writer is installed at every durable sink before any node acts on it:
//! the ProjectSessions writer fence first, then the RemoteNode authority row
//! that capture, replay, query, and failover read. Both sinks only seed, so a
//! retry after a partial publication converges and a rival writer is refused
//! instead of overwriting a fenced one.

use std::sync::Arc;
use std::sync::atomic::AtomicU8;

use tracedecay_contracts::RequestId;
use tracedecay_contracts::remote::capture::RemoteWriterAuthorityV1;
use tracedecay_contracts::remote::replay::RemoteReplayPolicyEvidenceV1;
use tracedecay_domain::{BrainNodeId, ManifestDigest, UtcMicros, canonical_sha256};
use tracedecay_rusqlite_runtime::remote::{
    RemoteRecoveryPhysicalEffectErrorV1, RemoteSqliteStorageV1, RemoteWriterPublicationErrorV1,
};
use tracedecay_store::RemoteWriterFenceInstallV1;

use super::super::DaemonSessionRuntimeRegistryV1;
use super::INTERRUPTION_NONE;
use super::support::RecoveryRuntimeProbeV1;
use crate::remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1;

impl DaemonSessionRuntimeRegistryV1 {
    /// Publishes `writer` as the first writer authority `node_id` serves for
    /// its Brain lineage, together with the replay policy admitting its
    /// frames. Publishing the identical writer again is an exact replay.
    #[hotpath::skip]
    pub async fn publish_remote_writer_authority(
        &self,
        node_id: &BrainNodeId,
        writer: RemoteWriterAuthorityV1,
        policy: RemoteReplayPolicyEvidenceV1,
    ) -> Result<(), RemoteWriterPublicationErrorV1> {
        let storage = self
            .remote_credential_authority
            .registered_node_storage(node_id)
            .map_err(|_| RemoteWriterPublicationErrorV1::Unavailable)?
            .ok_or(RemoteWriterPublicationErrorV1::NodeNotRegistered)?;
        let project_id = writer
            .target_project_id()
            .map_err(|_| RemoteWriterPublicationErrorV1::InvalidWriter)?
            .clone();
        let lifecycle = self
            .remote_recovery_project_lifecycle
            .get()
            .cloned()
            .ok_or(RemoteWriterPublicationErrorV1::Unavailable)?;
        let _admission = lifecycle
            .authorize_project_recovery(&project_id)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "remote writer publication project is not admitted");
                RemoteWriterPublicationErrorV1::ProjectUnavailable
            })?;
        let replay = Arc::clone(&self.remote_replay_transaction);
        tokio::task::spawn_blocking(move || {
            publish_first_writer(
                &storage,
                &replay,
                &writer,
                &policy,
                tracedecay_contracts::clock::now_micros(),
            )
        })
        .await
        .map_err(|_| RemoteWriterPublicationErrorV1::Unavailable)?
    }
}

fn publish_first_writer(
    storage: &RemoteSqliteStorageV1,
    replay: &DaemonRemoteReplayTransactionAuthorityV1,
    writer: &RemoteWriterAuthorityV1,
    policy: &RemoteReplayPolicyEvidenceV1,
    published_at: UtcMicros,
) -> Result<(), RemoteWriterPublicationErrorV1> {
    storage.writer_publication_state(writer, policy, published_at)?;
    seed_project_writer_fence(replay, writer, published_at)?;
    storage.publish_authority(writer, policy, published_at)
}

fn seed_project_writer_fence(
    replay: &DaemonRemoteReplayTransactionAuthorityV1,
    writer: &RemoteWriterAuthorityV1,
    installed_at: UtcMicros,
) -> Result<(), RemoteWriterPublicationErrorV1> {
    let project_id = writer
        .target_project_id()
        .map_err(|_| RemoteWriterPublicationErrorV1::InvalidWriter)?
        .clone();
    let fence = &writer.authority.fence;
    let (target_binding, _) = replay
        .target_descriptor(&project_id)
        .map_err(|_| RemoteWriterPublicationErrorV1::ProjectUnavailable)?;
    let authority_key = canonical_sha256(&(
        "tracedecay.remote-recovery-authority.v1",
        &fence.brain_id,
        &fence.shard_id,
        &fence.generation_id,
    ))
    .map_err(|_| RemoteWriterPublicationErrorV1::InvalidWriter)?;
    let stored_fence = |replay: &DaemonRemoteReplayTransactionAuthorityV1| {
        replay
            .current_writer_fence(project_id.clone(), authority_key.clone())
            .map(|stored| stored.map(|(stored, _)| stored))
            .map_err(map_fence_error)
    };
    match stored_fence(replay)? {
        Some(stored) if stored == *fence => return Ok(()),
        Some(_) => return Err(RemoteWriterPublicationErrorV1::Conflict),
        None => {}
    }
    let probe = Arc::new(
        RecoveryRuntimeProbeV1::new(
            &publication_request_id(&authority_key)?,
            Arc::new(AtomicU8::new(INTERRUPTION_NONE)),
        )
        .map_err(map_fence_error)?,
    );
    let install = RemoteWriterFenceInstallV1 {
        project_id: project_id.clone(),
        target_binding,
        authority_key: authority_key.clone(),
        expected: None,
        replacement: fence.clone(),
        installed_at,
    };
    match replay.install_writer_fence(project_id.clone(), install, probe) {
        Ok(_) => Ok(()),
        Err(error) => match stored_fence(replay)? {
            Some(stored) if stored != *fence => Err(RemoteWriterPublicationErrorV1::Conflict),
            _ => Err(map_fence_error(error)),
        },
    }
}

fn publication_request_id(
    authority_key: &ManifestDigest,
) -> Result<RequestId, RemoteWriterPublicationErrorV1> {
    let suffix = authority_key
        .hex_suffix()
        .ok_or(RemoteWriterPublicationErrorV1::Corruption)?;
    RequestId::new(format!("request.remote-writer-publication.{suffix}"))
        .map_err(|_| RemoteWriterPublicationErrorV1::Corruption)
}

fn map_fence_error(error: RemoteRecoveryPhysicalEffectErrorV1) -> RemoteWriterPublicationErrorV1 {
    match error {
        RemoteRecoveryPhysicalEffectErrorV1::Corruption => {
            RemoteWriterPublicationErrorV1::Corruption
        }
        RemoteRecoveryPhysicalEffectErrorV1::ForwardRecoveryRequired
        | RemoteRecoveryPhysicalEffectErrorV1::Cancelled
        | RemoteRecoveryPhysicalEffectErrorV1::TimedOut
        | RemoteRecoveryPhysicalEffectErrorV1::Unavailable
        | RemoteRecoveryPhysicalEffectErrorV1::WriterAuthorityUnpublished => {
            RemoteWriterPublicationErrorV1::Unavailable
        }
    }
}
