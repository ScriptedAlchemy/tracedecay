use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    BrainNodeId, CurrentRemoteAuthorityV1, EntityId, ManifestDigest, ProjectId,
    RemoteRepositoryScopeV1, UtcMicros,
};

use crate::AnchoredObservationWrite;

use super::{
    CommandDigestV1, IdempotencyIdentityV1, RepositoryWritePayloadV1,
    StorageRuntimeContractErrorV1, StoreIdempotencyKeyV1, StoreOperationMetadataV1,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationRuntimeCommandV1 {
    pub command: serde_json::Value,
    pub idempotency: Option<IdempotencyIdentityV1>,
}

pub fn canonical_observation_runtime_command_v1(
    payload: &RepositoryWritePayloadV1,
) -> Result<ObservationRuntimeCommandV1, StorageRuntimeContractErrorV1> {
    let (command, idempotency) = match payload {
        RepositoryWritePayloadV1::Observation(write) => (
            serde_json::json!({
                "kind": "observation",
                "observation": write.observation(),
                "expected_cursor": write.expected_cursor(),
                "next_cursor": write.next_cursor(),
                "retrieval_anchor": write.retrieval_anchor(),
                "projection_generation": write.projection_generation(),
                "repository_provenance": write.repository_provenance_attachment(),
            }),
            None,
        ),
        RepositoryWritePayloadV1::RemoteObservation(write) => (
            serde_json::json!({
                "kind": "remote_observation",
                "event": write,
            }),
            Some(write.idempotency_identity()?),
        ),
        RepositoryWritePayloadV1::ObservationCursorAdvance(advance) => (
            serde_json::json!({
                "kind": "observation_cursor_advance",
                "expected_cursor": advance.expected_cursor(),
                "next_cursor": advance.next_cursor(),
                "coverage": advance.coverage(),
                "reason": advance.reason().as_str(),
                "sanitization_receipt": advance.sanitization_receipt(),
            }),
            None,
        ),
        _ => {
            return Err(StorageRuntimeContractErrorV1::InvalidRepositoryPayload {
                payload: "observation runtime command",
            });
        }
    };
    Ok(ObservationRuntimeCommandV1 {
        command,
        idempotency,
    })
}

#[derive(Clone, Debug)]
pub struct RemoteObservationReplayPartsV1 {
    pub event_id: String,
    pub frame_digest: ManifestDigest,
    pub enrollment_id: EntityId,
    pub enrollment_revision: u64,
    pub node_id: BrainNodeId,
    pub policy_revision: u64,
    pub capture_sequence: u64,
    pub previous_event_id: Option<String>,
    pub writer_project_id: ProjectId,
    pub writer_scope: RemoteRepositoryScopeV1,
    pub current_writer: CurrentRemoteAuthorityV1,
    pub captured_at: UtcMicros,
}

/// One authenticated remote event and its exact canonical observation effect.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteObservationReplayWriteV1 {
    event_id: String,
    frame_digest: ManifestDigest,
    enrollment_id: EntityId,
    enrollment_revision: u64,
    node_id: BrainNodeId,
    policy_revision: u64,
    capture_sequence: u64,
    previous_event_id: Option<String>,
    writer_project_id: ProjectId,
    writer_scope: RemoteRepositoryScopeV1,
    current_writer: CurrentRemoteAuthorityV1,
    captured_at: UtcMicros,
    anchored_write: AnchoredObservationWrite,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteObservationReplayWriteWireV1 {
    event_id: String,
    frame_digest: ManifestDigest,
    enrollment_id: EntityId,
    enrollment_revision: u64,
    node_id: BrainNodeId,
    policy_revision: u64,
    capture_sequence: u64,
    previous_event_id: Option<String>,
    writer_project_id: ProjectId,
    writer_scope: RemoteRepositoryScopeV1,
    current_writer: CurrentRemoteAuthorityV1,
    captured_at: UtcMicros,
    anchored_write: AnchoredObservationWrite,
}

impl RemoteObservationReplayWriteV1 {
    pub fn new(
        parts: RemoteObservationReplayPartsV1,
        anchored_write: AnchoredObservationWrite,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        let value = Self {
            event_id: parts.event_id,
            frame_digest: parts.frame_digest,
            enrollment_id: parts.enrollment_id,
            enrollment_revision: parts.enrollment_revision,
            node_id: parts.node_id,
            policy_revision: parts.policy_revision,
            capture_sequence: parts.capture_sequence,
            previous_event_id: parts.previous_event_id,
            writer_project_id: parts.writer_project_id,
            writer_scope: parts.writer_scope,
            current_writer: parts.current_writer,
            captured_at: parts.captured_at,
            anchored_write,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.frame_digest.validate().map_err(|_| invalid())?;
        self.enrollment_id.validate().map_err(|_| invalid())?;
        self.node_id.validate().map_err(|_| invalid())?;
        self.writer_project_id.validate().map_err(|_| invalid())?;
        self.writer_scope.validate().map_err(|_| invalid())?;
        self.current_writer.validate().map_err(|_| invalid())?;
        if self.event_id != event_id_for_digest(&self.frame_digest)
            || self.enrollment_revision == 0
            || self.policy_revision == 0
            || self.capture_sequence == 0
            || (self.capture_sequence == 1) != self.previous_event_id.is_none()
            || self
                .previous_event_id
                .as_ref()
                .is_some_and(|event_id| !valid_event_id(event_id))
            || self.writer_project_id != self.writer_scope.project_id
        {
            return Err(invalid());
        }
        if let tracedecay_domain::ObservationScopeV1::Project { project_id } =
            self.anchored_write.observation().scope()
            && project_id != &self.writer_project_id
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn validate_for_metadata(
        &self,
        metadata: &StoreOperationMetadataV1,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        self.validate()?;
        if metadata.idempotency != self.idempotency_identity()?
            || metadata.shard_id.brain_id != self.current_writer.fence.brain_id
            || metadata.authority_epoch.get() != self.current_writer.fence.authority_epoch.0
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn idempotency_identity(
        &self,
    ) -> Result<IdempotencyIdentityV1, StorageRuntimeContractErrorV1> {
        Ok(IdempotencyIdentityV1 {
            key: StoreIdempotencyKeyV1::new(self.event_id.clone())?,
            command_digest: CommandDigestV1::new(self.frame_digest.as_str())?,
        })
    }

    pub fn event_id(&self) -> &str {
        &self.event_id
    }

    pub fn frame_digest(&self) -> &ManifestDigest {
        &self.frame_digest
    }

    pub fn enrollment_id(&self) -> &EntityId {
        &self.enrollment_id
    }

    pub const fn enrollment_revision(&self) -> u64 {
        self.enrollment_revision
    }

    pub fn node_id(&self) -> &BrainNodeId {
        &self.node_id
    }

    pub const fn policy_revision(&self) -> u64 {
        self.policy_revision
    }

    pub const fn capture_sequence(&self) -> u64 {
        self.capture_sequence
    }

    pub fn previous_event_id(&self) -> Option<&str> {
        self.previous_event_id.as_deref()
    }

    pub fn writer_project_id(&self) -> &ProjectId {
        &self.writer_project_id
    }

    pub fn writer_scope(&self) -> &RemoteRepositoryScopeV1 {
        &self.writer_scope
    }

    pub fn current_writer(&self) -> &CurrentRemoteAuthorityV1 {
        &self.current_writer
    }

    pub const fn captured_at(&self) -> UtcMicros {
        self.captured_at
    }

    pub fn anchored_write(&self) -> &AnchoredObservationWrite {
        &self.anchored_write
    }
}

impl<'de> Deserialize<'de> for RemoteObservationReplayWriteV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = RemoteObservationReplayWriteWireV1::deserialize(deserializer)?;
        Self::new(
            RemoteObservationReplayPartsV1 {
                event_id: wire.event_id,
                frame_digest: wire.frame_digest,
                enrollment_id: wire.enrollment_id,
                enrollment_revision: wire.enrollment_revision,
                node_id: wire.node_id,
                policy_revision: wire.policy_revision,
                capture_sequence: wire.capture_sequence,
                previous_event_id: wire.previous_event_id,
                writer_project_id: wire.writer_project_id,
                writer_scope: wire.writer_scope,
                current_writer: wire.current_writer,
                captured_at: wire.captured_at,
            },
            wire.anchored_write,
        )
        .map_err(serde::de::Error::custom)
    }
}

fn event_id_for_digest(digest: &ManifestDigest) -> String {
    format!(
        "remote.event.{}",
        digest
            .as_str()
            .strip_prefix("sha256:")
            .unwrap_or(digest.as_str())
    )
}

fn valid_event_id(event_id: &str) -> bool {
    let Some(digest) = event_id.strip_prefix("remote.event.") else {
        return false;
    };
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn invalid() -> StorageRuntimeContractErrorV1 {
    StorageRuntimeContractErrorV1::InvalidRepositoryPayload {
        payload: "commit remote observation",
    }
}
