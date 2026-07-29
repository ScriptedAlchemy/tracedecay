//! Sole released-to-final-V2 migration contract.

use std::collections::BTreeSet;

pub const V0067_RELEASE_TAG: &str = "v0.0.67";
pub const V0067_RELEASE_COMMIT: &str = "b3eace523cedeaa8e2d1c8d3f7a669167ec6858d";
pub const LAST_RELEASED_SCHEMA_ID: &str = "tracedecay.release.v0.0.67";
pub const FINAL_V2_SCHEMA_ID: &str = "tracedecay.storage.final-v2";

const PROJECT_SCHEMA_SOURCE_SHA256: &str =
    "bcabee4ab7fd10ba8d5644fc0c3e1e6d66b37c9b8b0d11f3bb7a03040a962bbb";
const GLOBAL_SCHEMA_SOURCE_SHA256: &str =
    "090d46fcba0d9dc7a10ac77d49fd4dfcf953d46821c1f92f97b616a430da01c3";
const LCM_SCHEMA_SOURCE_SHA256: &str =
    "c0d633edf89b7e7cfc3183dc1b3db80c7cbff1b177ab42603cdd1b3261c654e1";
const STORAGE_SCHEMA_SOURCE_SHA256: &str =
    "eff245be30b0c23587caef2dee87be080eb24f03d3c93722b49c8100331c153b";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleasedStoreKind {
    Project,
    GlobalSession,
    Lcm,
    StoreManifest,
    RepositoryIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleasedSchemaEvidence {
    pub kind: ReleasedStoreKind,
    pub user_version: Option<u32>,
    pub schema_version: Option<u32>,
    pub source_fixture_sha256: String,
    pub structural_members: BTreeSet<String>,
}

impl ReleasedSchemaEvidence {
    pub fn recognize_v0067(&self) -> Result<RecognizedReleasedSchema, MigrationContractError> {
        let expected = ReleasedSchemaFixture::for_kind(self.kind);
        if self.user_version != expected.user_version
            || self.schema_version != expected.schema_version
            || self.source_fixture_sha256 != expected.source_fixture_sha256
            || self.structural_members != expected.structural_members
        {
            return Err(MigrationContractError::SourceSchemaMismatch);
        }
        Ok(RecognizedReleasedSchema {
            kind: self.kind,
            schema_id: LAST_RELEASED_SCHEMA_ID,
            release_tag: V0067_RELEASE_TAG,
            release_commit: V0067_RELEASE_COMMIT,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecognizedReleasedSchema {
    pub kind: ReleasedStoreKind,
    pub schema_id: &'static str,
    pub release_tag: &'static str,
    pub release_commit: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleasedSchemaFixture {
    pub kind: ReleasedStoreKind,
    pub user_version: Option<u32>,
    pub schema_version: Option<u32>,
    pub source_fixture_sha256: &'static str,
    pub structural_members: BTreeSet<String>,
}

impl ReleasedSchemaFixture {
    pub fn for_kind(kind: ReleasedStoreKind) -> Self {
        let (user_version, schema_version, source_fixture_sha256, members): (
            Option<u32>,
            Option<u32>,
            &str,
            &[&str],
        ) = match kind {
            ReleasedStoreKind::Project => (
                Some(18),
                None,
                PROJECT_SCHEMA_SOURCE_SHA256,
                &[
                    "edges",
                    "edges_dedup",
                    "files",
                    "memory_bank_dirty",
                    "memory_banks",
                    "memory_code_areas",
                    "memory_decisions",
                    "memory_decisions_fts",
                    "memory_entities",
                    "memory_fact_entities",
                    "memory_fact_relations",
                    "memory_facts",
                    "memory_facts_fts",
                    "memory_feedback_events",
                    "memory_oplog",
                    "metadata",
                    "node_fingerprints",
                    "nodes",
                    "nodes_fts",
                    "read_cache",
                    "redundancy_pairs",
                    "unresolved_refs",
                    "vectors",
                ],
            ),
            ReleasedStoreKind::GlobalSession => (
                None,
                None,
                GLOBAL_SCHEMA_SOURCE_SHA256,
                &[
                    "analytics_events",
                    "code_projects",
                    "dashboard_token_counts",
                    "graph_scopes",
                    "parse_offsets",
                    "project_aliases",
                    "projects",
                    "savings_ledger",
                    "session_messages",
                    "session_messages_fts",
                    "sessions",
                    "store_artifacts",
                    "store_instances",
                    "turns",
                ],
            ),
            ReleasedStoreKind::Lcm => (
                None,
                Some(5),
                LCM_SCHEMA_SOURCE_SHA256,
                &[
                    "lcm_external_payloads",
                    "lcm_gc_marks",
                    "lcm_gc_meta",
                    "lcm_lifecycle_state",
                    "lcm_maintenance_debt",
                    "lcm_raw_messages",
                    "lcm_raw_messages_fts",
                    "lcm_summary_nodes",
                    "lcm_summary_nodes_fts",
                    "lcm_summary_sources",
                    "session_messages",
                    "session_schema_migrations",
                    "sessions",
                ],
            ),
            ReleasedStoreKind::StoreManifest => (
                None,
                Some(1),
                STORAGE_SCHEMA_SOURCE_SHA256,
                &[
                    "artifacts",
                    "created_at",
                    "mode",
                    "profile_id",
                    "project_id",
                    "schema_version",
                    "store_kind",
                    "store_uuid",
                ],
            ),
            ReleasedStoreKind::RepositoryIdentity => (
                None,
                Some(1),
                STORAGE_SCHEMA_SOURCE_SHA256,
                &[
                    "canonical_common_dir",
                    "created_at",
                    "repository_id",
                    "schema_version",
                ],
            ),
        };
        Self {
            kind,
            user_version,
            schema_version,
            source_fixture_sha256,
            structural_members: members.iter().map(|member| (*member).to_owned()).collect(),
        }
    }

    pub fn evidence(&self) -> ReleasedSchemaEvidence {
        ReleasedSchemaEvidence {
            kind: self.kind,
            user_version: self.user_version,
            schema_version: self.schema_version,
            source_fixture_sha256: self.source_fixture_sha256.to_owned(),
            structural_members: self.structural_members.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FinalSchemaFamily {
    BranchStackRevisions,
    BranchStackRevisionNodes,
    BranchStackRevisionEdges,
    BranchStackPreviews,
    BranchStackConsumedApprovals,
    BranchStackJournals,
    BranchStackReceipts,
    BranchStackQuarantine,
    RemoteObservationTransactions,
    RemoteAdmittedEncryptionMetadata,
    RemoteReplayDeduplication,
    RemoteBackupStaging,
    RemoteAuthorityCas,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FinalSchemaInvariant {
    ExactProjectAndSourceGeneration,
    CompareAndSwap,
    OneUse,
    VerifiedBackupBeforeDestruction,
    DurableReplayDeduplication,
    AdmittedEncryptionOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalSchemaFamilyManifest {
    pub family: FinalSchemaFamily,
    pub invariants: BTreeSet<FinalSchemaInvariant>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalTargetSchemaManifest {
    pub schema_id: &'static str,
    pub families: Vec<FinalSchemaFamilyManifest>,
}

impl FinalTargetSchemaManifest {
    pub fn authoritative() -> Self {
        use FinalSchemaFamily::*;
        let families = [
            BranchStackRevisions,
            BranchStackRevisionNodes,
            BranchStackRevisionEdges,
            BranchStackPreviews,
            BranchStackConsumedApprovals,
            BranchStackJournals,
            BranchStackReceipts,
            BranchStackQuarantine,
            RemoteObservationTransactions,
            RemoteAdmittedEncryptionMetadata,
            RemoteReplayDeduplication,
            RemoteBackupStaging,
            RemoteAuthorityCas,
        ]
        .into_iter()
        .map(|family| {
            let invariants = match family {
                BranchStackRevisions
                | BranchStackRevisionNodes
                | BranchStackRevisionEdges
                | BranchStackPreviews
                | BranchStackJournals
                | BranchStackReceipts
                | BranchStackQuarantine
                | RemoteObservationTransactions => {
                    BTreeSet::from([FinalSchemaInvariant::ExactProjectAndSourceGeneration])
                }
                BranchStackConsumedApprovals => BTreeSet::from([
                    FinalSchemaInvariant::ExactProjectAndSourceGeneration,
                    FinalSchemaInvariant::OneUse,
                ]),
                RemoteAdmittedEncryptionMetadata => {
                    BTreeSet::from([FinalSchemaInvariant::AdmittedEncryptionOnly])
                }
                RemoteReplayDeduplication => {
                    BTreeSet::from([FinalSchemaInvariant::DurableReplayDeduplication])
                }
                RemoteBackupStaging => {
                    BTreeSet::from([FinalSchemaInvariant::VerifiedBackupBeforeDestruction])
                }
                RemoteAuthorityCas => BTreeSet::from([FinalSchemaInvariant::CompareAndSwap]),
            };
            FinalSchemaFamilyManifest { family, invariants }
        })
        .collect();
        Self {
            schema_id: FINAL_V2_SCHEMA_ID,
            families,
        }
    }

    pub fn family(&self, family: FinalSchemaFamily) -> &FinalSchemaFamilyManifest {
        self.families
            .iter()
            .find(|candidate| candidate.family == family)
            .expect("authoritative final schema family")
    }

    pub fn validate(&self) -> Result<(), MigrationContractError> {
        if self.schema_id != FINAL_V2_SCHEMA_ID || self.families.len() != 13 {
            return Err(MigrationContractError::TargetSchemaMismatch);
        }
        let unique = self
            .families
            .iter()
            .map(|family| family.family)
            .collect::<BTreeSet<_>>();
        if unique.len() != self.families.len()
            || self
                .families
                .iter()
                .any(|family| family.invariants.is_empty())
        {
            return Err(MigrationContractError::TargetSchemaMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastReleasedToFinalV2MigrationContract {
    pub source_schema_id: &'static str,
    pub source_release_tag: &'static str,
    pub source_release_commit: &'static str,
    pub target_schema: FinalTargetSchemaManifest,
}

impl LastReleasedToFinalV2MigrationContract {
    pub fn authoritative() -> Self {
        Self {
            source_schema_id: LAST_RELEASED_SCHEMA_ID,
            source_release_tag: V0067_RELEASE_TAG,
            source_release_commit: V0067_RELEASE_COMMIT,
            target_schema: FinalTargetSchemaManifest::authoritative(),
        }
    }

    pub fn validate(&self) -> Result<(), MigrationContractError> {
        if self.source_schema_id != LAST_RELEASED_SCHEMA_ID
            || self.source_release_tag != V0067_RELEASE_TAG
            || self.source_release_commit != V0067_RELEASE_COMMIT
        {
            return Err(MigrationContractError::SourceSchemaMismatch);
        }
        self.target_schema.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactMigrationSourceIdentity {
    pub project_id: String,
    pub source_generation: String,
    pub schema_id: String,
}

impl ExactMigrationSourceIdentity {
    pub fn new(
        project_id: impl Into<String>,
        source_generation: impl Into<String>,
        schema_id: impl Into<String>,
    ) -> Result<Self, MigrationContractError> {
        let identity = Self {
            project_id: project_id.into(),
            source_generation: source_generation.into(),
            schema_id: schema_id.into(),
        };
        if identity.project_id.is_empty()
            || identity.source_generation.is_empty()
            || identity.schema_id != LAST_RELEASED_SCHEMA_ID
        {
            return Err(MigrationContractError::SourceSchemaMismatch);
        }
        Ok(identity)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedBackupIdentity {
    pub backup_id: String,
    pub source: ExactMigrationSourceIdentity,
    pub archive_id: String,
    pub digest: [u8; 32],
    pub verified_at: i64,
}

impl VerifiedBackupIdentity {
    pub fn new(
        backup_id: impl Into<String>,
        source: ExactMigrationSourceIdentity,
        archive_id: impl Into<String>,
        digest: [u8; 32],
        verified_at: i64,
    ) -> Result<Self, MigrationContractError> {
        let backup = Self {
            backup_id: backup_id.into(),
            source,
            archive_id: archive_id.into(),
            digest,
            verified_at,
        };
        if backup.backup_id.is_empty()
            || backup.archive_id.is_empty()
            || backup.digest == [0; 32]
            || backup.verified_at <= 0
        {
            return Err(MigrationContractError::BackupNotVerified);
        }
        Ok(backup)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CutoverPublicationReceipt {
    pub receipt_id: String,
    pub source: ExactMigrationSourceIdentity,
    pub target_schema_id: String,
    pub authority_cas_receipt_id: String,
    pub published_at: i64,
}

impl CutoverPublicationReceipt {
    pub fn new(
        receipt_id: impl Into<String>,
        source: ExactMigrationSourceIdentity,
        target_schema_id: impl Into<String>,
        authority_cas_receipt_id: impl Into<String>,
        published_at: i64,
    ) -> Result<Self, MigrationContractError> {
        let receipt = Self {
            receipt_id: receipt_id.into(),
            source,
            target_schema_id: target_schema_id.into(),
            authority_cas_receipt_id: authority_cas_receipt_id.into(),
            published_at,
        };
        if receipt.receipt_id.is_empty()
            || receipt.target_schema_id != FINAL_V2_SCHEMA_ID
            || receipt.authority_cas_receipt_id.is_empty()
            || receipt.published_at <= 0
        {
            return Err(MigrationContractError::PublicationInvalid);
        }
        Ok(receipt)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveExpiryPolicyReceipt {
    pub receipt_id: String,
    pub source: ExactMigrationSourceIdentity,
    pub archive_id: String,
    pub declared_at: i64,
    pub expires_at: i64,
}

impl ArchiveExpiryPolicyReceipt {
    pub fn new(
        receipt_id: impl Into<String>,
        source: ExactMigrationSourceIdentity,
        archive_id: impl Into<String>,
        declared_at: i64,
        expires_at: i64,
    ) -> Result<Self, MigrationContractError> {
        let receipt = Self {
            receipt_id: receipt_id.into(),
            source,
            archive_id: archive_id.into(),
            declared_at,
            expires_at,
        };
        if receipt.receipt_id.is_empty()
            || receipt.archive_id.is_empty()
            || receipt.declared_at <= 0
            || receipt.expires_at <= receipt.declared_at
        {
            return Err(MigrationContractError::ArchivePolicyMismatch);
        }
        Ok(receipt)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveExpiryEligibility {
    pub archive_id: String,
    pub policy_receipt_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutoverRecoveryAction {
    RollbackBeforePublication,
    RollForwardAfterPublication,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurableMigrationCheckpoint {
    pub checkpoint_id: String,
    pub migration_id: String,
    pub source: ExactMigrationSourceIdentity,
    pub backup: VerifiedBackupIdentity,
    pub prepared_at: i64,
    pub publication: Option<CutoverPublicationReceipt>,
}

impl DurableMigrationCheckpoint {
    pub fn before_publication(
        checkpoint_id: impl Into<String>,
        migration_id: impl Into<String>,
        source: ExactMigrationSourceIdentity,
        backup: VerifiedBackupIdentity,
        prepared_at: i64,
    ) -> Result<Self, MigrationContractError> {
        let checkpoint = Self {
            checkpoint_id: checkpoint_id.into(),
            migration_id: migration_id.into(),
            source,
            backup,
            prepared_at,
            publication: None,
        };
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    pub fn record_publication(
        &mut self,
        receipt: CutoverPublicationReceipt,
    ) -> Result<(), MigrationContractError> {
        if receipt.source != self.source || self.publication.is_some() {
            return Err(MigrationContractError::IdentityMismatch);
        }
        self.publication = Some(receipt);
        Ok(())
    }

    pub const fn recovery_action(&self) -> CutoverRecoveryAction {
        if self.publication.is_some() {
            CutoverRecoveryAction::RollForwardAfterPublication
        } else {
            CutoverRecoveryAction::RollbackBeforePublication
        }
    }

    pub fn archive_expiry_eligibility(
        &self,
        policy: &ArchiveExpiryPolicyReceipt,
        observed_at: i64,
    ) -> Result<ArchiveExpiryEligibility, MigrationContractError> {
        if self.publication.is_none()
            || policy.source != self.source
            || policy.archive_id != self.backup.archive_id
        {
            return Err(MigrationContractError::ArchivePolicyMismatch);
        }
        if observed_at < policy.expires_at {
            return Err(MigrationContractError::ArchiveNotYetEligible);
        }
        Ok(ArchiveExpiryEligibility {
            archive_id: policy.archive_id.clone(),
            policy_receipt_id: policy.receipt_id.clone(),
        })
    }

    pub fn validate(&self) -> Result<(), MigrationContractError> {
        if self.checkpoint_id.is_empty()
            || self.migration_id.is_empty()
            || self.prepared_at <= self.backup.verified_at
            || self.backup.source != self.source
            || self.publication.as_ref().is_some_and(|publication| {
                publication.source != self.source || publication.published_at < self.prepared_at
            })
        {
            return Err(MigrationContractError::IdentityMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationContractError {
    SourceSchemaMismatch,
    TargetSchemaMismatch,
    BackupNotVerified,
    PublicationInvalid,
    IdentityMismatch,
    ArchivePolicyMismatch,
    ArchiveNotYetEligible,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_released_fixture_is_exact_and_recognizable() {
        for kind in [
            ReleasedStoreKind::Project,
            ReleasedStoreKind::GlobalSession,
            ReleasedStoreKind::Lcm,
            ReleasedStoreKind::StoreManifest,
            ReleasedStoreKind::RepositoryIdentity,
        ] {
            let fixture = ReleasedSchemaFixture::for_kind(kind);
            assert_eq!(
                fixture.evidence().recognize_v0067().unwrap().release_commit,
                V0067_RELEASE_COMMIT
            );
        }
    }

    #[test]
    fn structural_drift_is_not_a_released_schema() {
        let fixture = ReleasedSchemaFixture::for_kind(ReleasedStoreKind::GlobalSession);
        let mut evidence = fixture.evidence();
        evidence
            .structural_members
            .insert("unreleased_table".to_owned());
        assert_eq!(
            evidence.recognize_v0067(),
            Err(MigrationContractError::SourceSchemaMismatch)
        );
    }
}
