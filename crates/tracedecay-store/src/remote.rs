use tracedecay_domain::{
    BrainNodeId, EntityId, ManifestDigest, ProjectId, RemoteWriterFenceV1, UtcMicros,
};

use crate::{AnchoredObservationWrite, StorageRuntimeContractErrorV1};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteObservationReplayWriteV1 {
    pub event_id: String,
    pub authority_key: ManifestDigest,
    pub frame_digest: ManifestDigest,
    pub enrollment_id: EntityId,
    pub enrollment_revision: u64,
    pub node_id: BrainNodeId,
    pub policy_revision: u64,
    pub capture_sequence: u64,
    pub previous_event_id: Option<String>,
    pub project_id: ProjectId,
    pub writer_fence: RemoteWriterFenceV1,
    pub captured_at: UtcMicros,
    pub command_digest: ManifestDigest,
    pub observation: AnchoredObservationWrite,
}

impl RemoteObservationReplayWriteV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        let valid_event_id = |event_id: &str| {
            (16..=160).contains(&event_id.len())
                && event_id.trim() == event_id
                && !event_id.chars().any(char::is_control)
        };
        let observation_project = match self.observation.observation().scope() {
            tracedecay_domain::ObservationScopeV1::Project { project_id } => project_id,
            tracedecay_domain::ObservationScopeV1::Profile => {
                return Err(StorageRuntimeContractErrorV1::InvalidRepositoryPayload {
                    payload: "replay remote observation",
                });
            }
        };
        let expected_authority_key = tracedecay_domain::canonical_sha256(&(
            "tracedecay.remote-recovery-authority.v1",
            &self.writer_fence.brain_id,
            &self.writer_fence.shard_id,
            &self.writer_fence.generation_id,
        ));
        if !valid_event_id(&self.event_id)
            || self
                .previous_event_id
                .as_deref()
                .is_some_and(|event_id| !valid_event_id(event_id))
            || (self.capture_sequence == 1) != self.previous_event_id.is_none()
            || self.enrollment_revision == 0
            || self.policy_revision == 0
            || i64::try_from(self.capture_sequence).is_err()
            || observation_project != &self.project_id
            || self.writer_fence.validate().is_err()
            || !expected_authority_key.is_ok_and(|key| key == self.authority_key)
            || self.frame_digest.validate().is_err()
            || self.command_digest.validate().is_err()
        {
            return Err(StorageRuntimeContractErrorV1::InvalidRepositoryPayload {
                payload: "replay remote observation",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteWriterFenceInstallV1 {
    pub project_id: ProjectId,
    pub target_binding: crate::StoreRuntimeBindingV1,
    pub authority_key: ManifestDigest,
    pub expected: RemoteWriterFenceV1,
    pub replacement: RemoteWriterFenceV1,
    pub installed_at: UtcMicros,
}

impl RemoteWriterFenceInstallV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        let same_lineage = self.expected.brain_id == self.replacement.brain_id
            && self.expected.shard_id == self.replacement.shard_id
            && self.expected.generation_id == self.replacement.generation_id;
        let next_epoch = self
            .expected
            .authority_epoch
            .0
            .checked_add(1)
            .is_some_and(|epoch| epoch == self.replacement.authority_epoch.0);
        let next_placement = self
            .expected
            .placement_revision
            .get()
            .checked_add(1)
            .is_some_and(|revision| revision == self.replacement.placement_revision.get());
        let expected_authority_key = tracedecay_domain::canonical_sha256(&(
            "tracedecay.remote-recovery-authority.v1",
            &self.expected.brain_id,
            &self.expected.shard_id,
            &self.expected.generation_id,
        ));
        let target_matches_project = matches!(
            &self.target_binding.shard_id.scope,
            crate::StoreShardScopeV1::ProjectSessions { project_id }
                if project_id == &self.project_id
        );
        if self.authority_key.validate().is_err()
            || self.project_id.validate().is_err()
            || !target_matches_project
            || !expected_authority_key.is_ok_and(|key| key == self.authority_key)
            || self.expected.validate().is_err()
            || self.replacement.validate().is_err()
            || !same_lineage
            || !next_epoch
            || !next_placement
        {
            return Err(StorageRuntimeContractErrorV1::InvalidRepositoryPayload {
                payload: "install remote writer fence",
            });
        }
        Ok(())
    }
}
