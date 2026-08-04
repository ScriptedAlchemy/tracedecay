//! Driver-neutral contracts for authenticated remote reads and fenced recovery.
//!
//! These records deliberately contain no paths, database handles, credentials,
//! or transport addresses. Authentication is an admission fact supplied by the
//! daemon; content digests detect corruption but are not signatures.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::canonical_text::{CANONICAL_TEXT_MAX_BYTES, is_canonical_text_within};
use tracedecay_domain::{
    EnrollmentCredentialRecordV1, EnrollmentCredentialStateV1, EntityId, RemoteWriterFenceV1,
    UtcMicros,
};

use crate::{ShardWatermarkV1, StoreRuntimeBindingV1, StoreShardIdV1};

pub type ContentDigestV1 = [u8; 32];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthenticatedManifestContextV1 {
    pub enrollment: EnrollmentCredentialRecordV1,
    pub authorization_revision: u64,
    pub authentication_receipt_id: EntityId,
    pub authenticated_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReplicaCacheManifestV1 {
    pub authentication: AuthenticatedManifestContextV1,
    pub writer: RemoteWriterFenceV1,
    pub runtime: StoreRuntimeBindingV1,
    pub schema_digest: ContentDigestV1,
    pub watermark: ShardWatermarkV1,
    pub material_digest: ContentDigestV1,
    pub material_bytes: u64,
    pub observed_at_micros: i64,
    pub expires_at_micros: i64,
}

impl ReplicaCacheManifestV1 {
    pub fn validate_for(
        &self,
        expected_writer: &RemoteWriterFenceV1,
        expected_runtime: &StoreRuntimeBindingV1,
        expected_schema_digest: ContentDigestV1,
        now_micros: i64,
    ) -> Result<(), RemoteRecoveryContractErrorV1> {
        validate_authentication(&self.authentication, now_micros)?;
        validate_writer_binding(&self.writer, &self.runtime)?;
        validate_binding_watermark(&self.runtime, &self.watermark)?;
        if &self.writer != expected_writer || &self.runtime != expected_runtime {
            return Err(RemoteRecoveryContractErrorV1::BindingMismatch);
        }
        if self.schema_digest != expected_schema_digest {
            return Err(RemoteRecoveryContractErrorV1::SchemaMismatch);
        }
        if self.material_bytes == 0 || self.material_digest == [0; 32] {
            return Err(RemoteRecoveryContractErrorV1::InvalidArtifact);
        }
        if self.expires_at_micros <= self.observed_at_micros {
            return Err(RemoteRecoveryContractErrorV1::InvalidExpiry);
        }
        if now_micros >= self.expires_at_micros {
            return Err(RemoteRecoveryContractErrorV1::Expired);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackupCoverageV1 {
    Complete,
    Partial,
    Stale,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackupArtifactKindV1 {
    SqliteDatabase,
    ImmutablePayload,
    ImmutableGeneration,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BackupArtifactV1 {
    pub artifact_id: String,
    pub family: String,
    pub kind: BackupArtifactKindV1,
    pub bytes: u64,
    pub sha256: ContentDigestV1,
    pub references: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BackupManifestV1 {
    pub backup_id: String,
    pub authentication: AuthenticatedManifestContextV1,
    pub writer: RemoteWriterFenceV1,
    pub runtime: StoreRuntimeBindingV1,
    pub schema_digest: ContentDigestV1,
    pub source_frontier: ShardWatermarkV1,
    pub parent_backup_id: Option<String>,
    pub lineage_digest: ContentDigestV1,
    pub created_at_micros: i64,
    pub expires_at_micros: i64,
    pub coverage: BackupCoverageV1,
    pub artifacts: Vec<BackupArtifactV1>,
    pub total_bytes: u64,
    pub artifact_count: u64,
}

impl BackupManifestV1 {
    pub fn validate(&self, now_micros: i64) -> Result<(), RemoteRecoveryContractErrorV1> {
        validate_identifier(&self.backup_id)?;
        validate_authentication(&self.authentication, now_micros)?;
        validate_writer_binding(&self.writer, &self.runtime)?;
        validate_binding_watermark(&self.runtime, &self.source_frontier)?;
        if self.schema_digest == [0; 32] || self.lineage_digest == [0; 32] {
            return Err(RemoteRecoveryContractErrorV1::InvalidArtifact);
        }
        if self.expires_at_micros <= self.created_at_micros {
            return Err(RemoteRecoveryContractErrorV1::InvalidExpiry);
        }
        if now_micros >= self.expires_at_micros {
            return Err(RemoteRecoveryContractErrorV1::Expired);
        }
        if self.artifacts.is_empty() {
            return Err(RemoteRecoveryContractErrorV1::EmptyInventory);
        }
        let mut ids = BTreeSet::new();
        let mut bytes = 0_u64;
        for artifact in &self.artifacts {
            validate_identifier(&artifact.artifact_id)?;
            validate_identifier(&artifact.family)?;
            if artifact.bytes == 0 || artifact.sha256 == [0; 32] {
                return Err(RemoteRecoveryContractErrorV1::InvalidArtifact);
            }
            if !ids.insert(artifact.artifact_id.as_str()) {
                return Err(RemoteRecoveryContractErrorV1::DuplicateArtifact);
            }
            bytes = bytes
                .checked_add(artifact.bytes)
                .ok_or(RemoteRecoveryContractErrorV1::InventoryOverflow)?;
        }
        if self.artifact_count != self.artifacts.len() as u64 || self.total_bytes != bytes {
            return Err(RemoteRecoveryContractErrorV1::InventoryMismatch);
        }
        for artifact in &self.artifacts {
            for reference in &artifact.references {
                if !ids.contains(reference.as_str()) {
                    return Err(RemoteRecoveryContractErrorV1::ReferenceClosure);
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CurrentPolicyReplayV1 {
    pub tombstone_revision: u64,
    pub deletion_revision: u64,
    pub quarantine_revision: u64,
    pub retention_revision: u64,
    pub authorization_revision: u64,
    pub project_scope_digest: ContentDigestV1,
}

impl CurrentPolicyReplayV1 {
    pub fn validate(&self) -> Result<(), RemoteRecoveryContractErrorV1> {
        if self.tombstone_revision == 0
            || self.deletion_revision == 0
            || self.quarantine_revision == 0
            || self.retention_revision == 0
            || self.authorization_revision == 0
            || self.project_scope_digest == [0; 32]
        {
            return Err(RemoteRecoveryContractErrorV1::PolicyReplayMissing);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum StagedRestoreStateV1 {
    Allocated,
    BytesVerified,
    ReferenceClosureVerified,
    PolicyReplayed { replay: CurrentPolicyReplayV1 },
    ReadyForPublication,
    RolledBack { reason_code: String },
    RecoveryRequired { reason_code: String },
    Published { publication_receipt_id: String },
}

impl StagedRestoreStateV1 {
    pub fn may_serve(&self) -> bool {
        matches!(self, Self::Published { .. })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestorePreviewV1 {
    pub preview_id: String,
    pub backup_id: String,
    pub expected_binding: StoreRuntimeBindingV1,
    pub expected_placement_revision: u64,
    pub expected_manifest_digest: ContentDigestV1,
    pub current_policy: CurrentPolicyReplayV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreConfirmationV1 {
    pub preview_id: String,
    pub expected_manifest_digest: ContentDigestV1,
    pub expected_authority_epoch: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorityCasV1 {
    pub shard_id: StoreShardIdV1,
    pub expected_binding: StoreRuntimeBindingV1,
    pub replacement_binding: StoreRuntimeBindingV1,
    pub expected_placement_revision: u64,
    pub replacement_placement_revision: u64,
}

impl AuthorityCasV1 {
    pub fn validate(&self) -> Result<(), RemoteRecoveryContractErrorV1> {
        if self.shard_id != self.expected_binding.shard_id
            || self.shard_id != self.replacement_binding.shard_id
        {
            return Err(RemoteRecoveryContractErrorV1::BindingMismatch);
        }
        if self.expected_binding.incarnation != self.replacement_binding.incarnation {
            return Err(RemoteRecoveryContractErrorV1::ImmutableGenerationChanged);
        }
        if self.replacement_binding.authority_epoch <= self.expected_binding.authority_epoch {
            return Err(RemoteRecoveryContractErrorV1::EpochNotAdvanced);
        }
        if self.expected_placement_revision == 0
            || self.replacement_placement_revision <= self.expected_placement_revision
        {
            return Err(RemoteRecoveryContractErrorV1::PlacementMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PromotionPreviewV1 {
    pub preview_id: String,
    pub cas: AuthorityCasV1,
    pub required_frontier: ShardWatermarkV1,
    pub required_sink_ids: Vec<String>,
}

impl PromotionPreviewV1 {
    pub fn validate(&self) -> Result<(), RemoteRecoveryContractErrorV1> {
        validate_identifier(&self.preview_id)?;
        self.cas.validate()?;
        if self.required_frontier.shard_id != self.cas.shard_id
            || self.required_frontier.incarnation != self.cas.expected_binding.incarnation
            || self.required_frontier.authority_epoch != self.cas.expected_binding.authority_epoch
        {
            return Err(RemoteRecoveryContractErrorV1::FrontierMismatch);
        }
        let mut sinks = BTreeSet::new();
        if self.required_sink_ids.is_empty()
            || self
                .required_sink_ids
                .iter()
                .any(|sink| validate_identifier(sink).is_err() || !sinks.insert(sink))
        {
            return Err(RemoteRecoveryContractErrorV1::SinkInventoryInvalid);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PromotionConfirmationV1 {
    pub preview_id: String,
    pub expected_authority_epoch: u64,
    pub expected_placement_revision: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum PromotionRecoveryStateV1 {
    Previewed,
    CasCommitted,
    InstallingSinks { installed_sink_ids: Vec<String> },
    ReadyToPublish,
    Serving,
    RolledBackBeforePublication,
    ForwardRecoveryRequired { missing_sink_ids: Vec<String> },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PromotionReceiptV1 {
    pub receipt_id: String,
    pub preview_id: String,
    pub replacement_binding: StoreRuntimeBindingV1,
    pub replacement_placement_revision: u64,
    pub installed_sink_epochs: BTreeMap<String, u64>,
    pub published_frontier: ShardWatermarkV1,
    pub old_authority_read_only: bool,
    pub state: PromotionRecoveryStateV1,
}

impl PromotionReceiptV1 {
    pub fn validate_against(
        &self,
        preview: &PromotionPreviewV1,
    ) -> Result<(), RemoteRecoveryContractErrorV1> {
        preview.validate()?;
        if self.preview_id != preview.preview_id
            || self.replacement_binding != preview.cas.replacement_binding
            || self.replacement_placement_revision != preview.cas.replacement_placement_revision
            || !self.old_authority_read_only
        {
            return Err(RemoteRecoveryContractErrorV1::PromotionReceiptMismatch);
        }
        let epoch = self.replacement_binding.authority_epoch.get();
        if preview
            .required_sink_ids
            .iter()
            .any(|sink| self.installed_sink_epochs.get(sink).copied() != Some(epoch))
        {
            return Err(RemoteRecoveryContractErrorV1::SinkFenceMissing);
        }
        if self.published_frontier.shard_id != preview.cas.shard_id
            || self.published_frontier.incarnation != preview.cas.replacement_binding.incarnation
            || self.published_frontier.authority_epoch
                != preview.cas.replacement_binding.authority_epoch
            || self.published_frontier.commit_sequence < preview.required_frontier.commit_sequence
        {
            return Err(RemoteRecoveryContractErrorV1::FrontierMismatch);
        }
        if !matches!(self.state, PromotionRecoveryStateV1::Serving) {
            return Err(RemoteRecoveryContractErrorV1::NotServing);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RemoteRecoveryContractErrorV1 {
    #[error("manifest authentication context is invalid")]
    AuthenticationInvalid,
    #[error("runtime binding does not match")]
    BindingMismatch,
    #[error("placement revision does not match")]
    PlacementMismatch,
    #[error("schema digest does not match")]
    SchemaMismatch,
    #[error("manifest or backup has expired")]
    Expired,
    #[error("expiry interval is invalid")]
    InvalidExpiry,
    #[error("artifact inventory is empty")]
    EmptyInventory,
    #[error("artifact is invalid")]
    InvalidArtifact,
    #[error("artifact identity is duplicated")]
    DuplicateArtifact,
    #[error("artifact inventory totals do not match")]
    InventoryMismatch,
    #[error("artifact inventory overflowed")]
    InventoryOverflow,
    #[error("artifact reference closure is incomplete")]
    ReferenceClosure,
    #[error("current policy replay is missing")]
    PolicyReplayMissing,
    #[error("immutable generation identity changed")]
    ImmutableGenerationChanged,
    #[error("authority epoch did not advance")]
    EpochNotAdvanced,
    #[error("required durable frontier does not match")]
    FrontierMismatch,
    #[error("required sink inventory is invalid")]
    SinkInventoryInvalid,
    #[error("promotion receipt does not match its preview")]
    PromotionReceiptMismatch,
    #[error("not every durable sink has the replacement fence")]
    SinkFenceMissing,
    #[error("promotion has not reached serving state")]
    NotServing,
    #[error("identifier is not canonical")]
    InvalidIdentifier,
}

fn validate_binding_watermark(
    binding: &StoreRuntimeBindingV1,
    watermark: &ShardWatermarkV1,
) -> Result<(), RemoteRecoveryContractErrorV1> {
    if binding.shard_id != watermark.shard_id
        || binding.incarnation != watermark.incarnation
        || binding.authority_epoch != watermark.authority_epoch
    {
        return Err(RemoteRecoveryContractErrorV1::BindingMismatch);
    }
    Ok(())
}

fn validate_writer_binding(
    writer: &RemoteWriterFenceV1,
    runtime: &StoreRuntimeBindingV1,
) -> Result<(), RemoteRecoveryContractErrorV1> {
    writer
        .validate()
        .map_err(|_| RemoteRecoveryContractErrorV1::BindingMismatch)?;
    if !runtime.shard_id.is_mutable()
        || writer.brain_id != runtime.shard_id.brain_id
        || writer.authority_epoch.0 != runtime.authority_epoch.get()
    {
        return Err(RemoteRecoveryContractErrorV1::BindingMismatch);
    }
    Ok(())
}

fn validate_authentication(
    authentication: &AuthenticatedManifestContextV1,
    now_micros: i64,
) -> Result<(), RemoteRecoveryContractErrorV1> {
    let observed_at = UtcMicros(now_micros);
    if authentication.enrollment.validate().is_err()
        || authentication
            .enrollment
            .state_at(authentication.authenticated_at)
            != EnrollmentCredentialStateV1::Active
        || authentication.enrollment.state_at(observed_at) != EnrollmentCredentialStateV1::Active
        || authentication.authorization_revision == 0
        || authentication.authentication_receipt_id.validate().is_err()
    {
        return Err(RemoteRecoveryContractErrorV1::AuthenticationInvalid);
    }
    Ok(())
}

fn validate_identifier(value: &str) -> Result<(), RemoteRecoveryContractErrorV1> {
    if is_canonical_text_within(value, CANONICAL_TEXT_MAX_BYTES) {
        Ok(())
    } else {
        Err(RemoteRecoveryContractErrorV1::InvalidIdentifier)
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::{BrainId, RemoteWriterFenceV1, UserProfileId};

    use super::*;
    use crate::{CommitSequenceV1, StoreAuthorityEpochV1, StoreIncarnationV1, StoreShardIdV1};

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn binding(epoch: u64) -> StoreRuntimeBindingV1 {
        StoreRuntimeBindingV1::new(
            StoreShardIdV1::profile(
                id::<BrainId>("brain.remote"),
                id::<UserProfileId>("profile.remote"),
            ),
            StoreIncarnationV1::new(7).unwrap(),
            StoreAuthorityEpochV1::new(epoch).unwrap(),
        )
    }

    fn watermark(epoch: u64, sequence: u64) -> ShardWatermarkV1 {
        let binding = binding(epoch);
        ShardWatermarkV1 {
            shard_id: binding.shard_id,
            incarnation: binding.incarnation,
            authority_epoch: binding.authority_epoch,
            commit_sequence: CommitSequenceV1(sequence),
        }
    }

    fn writer(epoch: u64) -> RemoteWriterFenceV1 {
        serde_json::from_value(serde_json::json!({
            "brain_id": "brain.remote",
            "shard_id": "shard.remote",
            "generation_id": "generation.remote",
            "placement_revision": 1,
            "authority_epoch": epoch,
            "authority_node_id": "node.authority"
        }))
        .unwrap()
    }

    fn authentication() -> AuthenticatedManifestContextV1 {
        serde_json::from_value(serde_json::json!({
            "enrollment": {
                "enrollment_id": "enrollment.remote",
                "brain_id": "brain.remote",
                "node_id": "node.standby",
                "fingerprint": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "revision": 2,
                "issued_at": 1,
                "expires_at": 100,
                "revoked_at": null,
                "capabilities": ["read_backup"],
                "scope": {
                    "project_id": "project.remote",
                    "repository_id": "repository.remote",
                    "worktree_id": "worktree.remote",
                    "reference": "refs/heads/main",
                    "snapshot_id": "snapshot.remote"
                }
            },
            "authorization_revision": 3,
            "authentication_receipt_id": "authentication.remote",
            "authenticated_at": 10
        }))
        .unwrap()
    }

    #[test]
    fn replica_manifest_rejects_a_different_epoch() {
        let manifest = ReplicaCacheManifestV1 {
            authentication: authentication(),
            writer: writer(8),
            runtime: binding(8),
            schema_digest: [1; 32],
            watermark: watermark(8, 12),
            material_digest: [2; 32],
            material_bytes: 99,
            observed_at_micros: 10,
            expires_at_micros: 20,
        };
        assert_eq!(
            manifest.validate_for(&writer(9), &binding(9), [1; 32], 11),
            Err(RemoteRecoveryContractErrorV1::BindingMismatch)
        );
    }

    #[test]
    fn backup_requires_exact_inventory_and_reference_closure() {
        let manifest = BackupManifestV1 {
            backup_id: "backup.1".into(),
            authentication: authentication(),
            writer: writer(8),
            runtime: binding(8),
            schema_digest: [3; 32],
            source_frontier: watermark(8, 12),
            parent_backup_id: None,
            lineage_digest: [4; 32],
            created_at_micros: 10,
            expires_at_micros: 30,
            coverage: BackupCoverageV1::Complete,
            artifacts: vec![BackupArtifactV1 {
                artifact_id: "db".into(),
                family: "profile".into(),
                kind: BackupArtifactKindV1::SqliteDatabase,
                bytes: 42,
                sha256: [5; 32],
                references: vec!["missing".into()],
            }],
            total_bytes: 42,
            artifact_count: 1,
        };
        assert_eq!(
            manifest.validate(20),
            Err(RemoteRecoveryContractErrorV1::ReferenceClosure)
        );
    }

    #[test]
    fn promotion_preserves_generation_and_requires_higher_epoch() {
        let invalid = AuthorityCasV1 {
            shard_id: binding(8).shard_id,
            expected_binding: binding(8),
            replacement_binding: binding(8),
            expected_placement_revision: 2,
            replacement_placement_revision: 3,
        };
        assert_eq!(
            invalid.validate(),
            Err(RemoteRecoveryContractErrorV1::EpochNotAdvanced)
        );
    }

    #[test]
    fn staged_restore_never_serves_before_publication() {
        assert!(!StagedRestoreStateV1::ReadyForPublication.may_serve());
        assert!(
            StagedRestoreStateV1::Published {
                publication_receipt_id: "publication.1".into()
            }
            .may_serve()
        );
    }
}
