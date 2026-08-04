//! Sole released-to-final-V2 migration contract.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tracedecay_store::{StoreRuntimeBindingV1, StoreShardScopeV1, VerifiedStoreLocatorV1};

pub const V0067_RELEASE_TAG: &str = "v0.0.67";
pub const V0067_RELEASE_COMMIT: &str = "b3eace523cedeaa8e2d1c8d3f7a669167ec6858d";
pub const LAST_RELEASED_SCHEMA_ID: &str = "tracedecay.release.v0.0.67";
pub const FINAL_V2_SCHEMA_ID: &str = "tracedecay.storage.final-v2";
pub const FINAL_PROJECT_SCHEMA_VERSION: u32 = 25;
pub const FINAL_LCM_SCHEMA_VERSION: u32 = 7;
pub const FINAL_STORE_MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const FINAL_REPOSITORY_IDENTITY_SCHEMA_VERSION: u32 = 1;
pub const FINAL_PROFILE_IDENTITY_SCHEMA_VERSION: u32 = 1;

const PROJECT_SCHEMA_SOURCE_SHA256: &str =
    "bcabee4ab7fd10ba8d5644fc0c3e1e6d66b37c9b8b0d11f3bb7a03040a962bbb";
const GLOBAL_SCHEMA_SOURCE_SHA256: &str =
    "090d46fcba0d9dc7a10ac77d49fd4dfcf953d46821c1f92f97b616a430da01c3";
const LCM_SCHEMA_SOURCE_SHA256: &str =
    "c0d633edf89b7e7cfc3183dc1b3db80c7cbff1b177ab42603cdd1b3261c654e1";
const STORAGE_SCHEMA_SOURCE_SHA256: &str =
    "eff245be30b0c23587caef2dee87be080eb24f03d3c93722b49c8100331c153b";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleasedStoreKind {
    Project,
    GlobalSession,
    Lcm,
    StoreManifest,
    RepositoryIdentity,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleasedV0067Fixture {
    release_tag: String,
    release_commit: String,
    project_schema: u32,
    lcm_schema: u32,
    store_manifest_schema: u32,
    repository_identity_schema: u32,
    target_project_schema: u32,
    target_lcm_schema: u32,
    profile_identity_present: bool,
    store_manifest: ReleasedStoreManifestFixture,
    repository_identity: ReleasedRepositoryIdentityFixture,
    registry_reconstruction: ReleasedRegistryReconstructionFixture,
    preservation_probes: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ReleasedStoreManifestFixture {
    schema_version: u32,
    project_id: String,
    store_kind: String,
    storage_mode: String,
    project_root: String,
    data_root: String,
    graph_db_relpath: String,
    sessions_db_relpath: String,
    branch_meta_relpath: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ReleasedRepositoryIdentityFixture {
    schema_version: u32,
    project_id: String,
    git_common_dir: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ReleasedRegistryReconstructionFixture {
    plans: usize,
    eligible: usize,
    stale: usize,
    issues: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadOnlyReleasedSchemaInspection {
    pub source: ExactMigrationSourceIdentity,
    pub project_schema: Option<u32>,
    pub lcm_schema: Option<u32>,
    pub store_manifest_schema: Option<u32>,
    pub repository_identity_schema: Option<u32>,
    pub project_structural_members: BTreeSet<String>,
    pub lcm_structural_members: BTreeSet<String>,
    pub durable_families: BTreeSet<ReleasedDurableFamily>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleasedDurableFamily {
    Memory,
    LcmMessagePayloadAndSummary,
    Configuration,
    RegistryAliasesAndIdentity,
}

impl ReleasedDurableFamily {
    pub fn all() -> BTreeSet<Self> {
        [
            Self::Memory,
            Self::LcmMessagePayloadAndSummary,
            Self::Configuration,
            Self::RegistryAliasesAndIdentity,
        ]
        .into_iter()
        .collect()
    }
}

impl ReleasedV0067Fixture {
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|error| error.to_string())
    }

    pub fn validate(&self) -> Result<(), MigrationContractError> {
        if self.release_tag != V0067_RELEASE_TAG
            || self.release_commit != V0067_RELEASE_COMMIT
            || self.project_schema != 18
            || self.lcm_schema != 5
            || self.store_manifest_schema != 1
            || self.repository_identity_schema != 1
            || self.target_project_schema != 25
            || self.target_lcm_schema != 7
            || self.profile_identity_present
            || self.store_manifest.schema_version != self.store_manifest_schema
            || self.store_manifest.project_id != self.project_id()
            || self.store_manifest.store_kind != "code_project"
            || self.store_manifest.storage_mode != "profile_sharded"
            || self.store_manifest.project_root.is_empty()
            || self.store_manifest.data_root.is_empty()
            || self.store_manifest.graph_db_relpath.is_empty()
            || self.store_manifest.sessions_db_relpath.is_empty()
            || self.store_manifest.branch_meta_relpath.is_empty()
            || self.repository_identity.schema_version != self.repository_identity_schema
            || self.repository_identity.project_id != self.project_id()
            || self.repository_identity.git_common_dir.is_empty()
            || self.registry_reconstruction.plans != 52
            || self.registry_reconstruction.eligible != 35
            || self.registry_reconstruction.stale != 17
            || self.registry_reconstruction.issues != 0
            || self
                .registry_reconstruction
                .eligible
                .checked_add(self.registry_reconstruction.stale)
                != Some(self.registry_reconstruction.plans)
            || [
                "memory_fact",
                "lcm_raw_message_id",
                "lcm_summary_node_id",
                "lcm_payload_ref",
                "registry_alias",
                "profile_id",
                "repository_id",
                "project_id",
                "store_id",
            ]
            .into_iter()
            .any(|key| {
                self.preservation_probes
                    .get(key)
                    .is_none_or(String::is_empty)
            })
        {
            return Err(MigrationContractError::SourceSchemaMismatch);
        }
        Ok(())
    }

    pub fn admit_read_only_inspection(
        &self,
        inspection: &ReadOnlyReleasedSchemaInspection,
    ) -> Result<(), MigrationContractError> {
        self.validate()?;
        let project = ReleasedSchemaFixture::for_kind(ReleasedStoreKind::Project);
        let lcm = ReleasedSchemaFixture::for_kind(ReleasedStoreKind::Lcm);
        if inspection.project_schema != Some(self.project_schema)
            || inspection.lcm_schema != Some(self.lcm_schema)
            || inspection.store_manifest_schema != Some(self.store_manifest_schema)
            || inspection.repository_identity_schema != Some(self.repository_identity_schema)
            || inspection.project_structural_members != project.structural_members
            || inspection.lcm_structural_members != lcm.structural_members
            || inspection.durable_families != ReleasedDurableFamily::all()
            || inspection.source.profile_id != self.profile_id()
            || inspection.source.repository_id != self.repository_id()
            || inspection.source.project_id != self.project_id()
            || inspection.source.store_id != self.store_id()
        {
            return Err(MigrationContractError::SourceSchemaMismatch);
        }
        Ok(())
    }

    pub fn project_schema(&self) -> u32 {
        self.project_schema
    }

    pub fn lcm_schema(&self) -> u32 {
        self.lcm_schema
    }

    pub fn store_manifest_schema(&self) -> u32 {
        self.store_manifest_schema
    }

    pub fn repository_identity_schema(&self) -> u32 {
        self.repository_identity_schema
    }

    pub fn target_project_schema(&self) -> u32 {
        self.target_project_schema
    }

    pub fn target_lcm_schema(&self) -> u32 {
        self.target_lcm_schema
    }

    pub fn profile_id(&self) -> &str {
        self.preservation_probes["profile_id"].as_str()
    }

    pub fn project_id(&self) -> &str {
        self.preservation_probes["project_id"].as_str()
    }

    pub fn repository_id(&self) -> &str {
        self.preservation_probes["repository_id"].as_str()
    }

    pub fn store_id(&self) -> &str {
        self.preservation_probes["store_id"].as_str()
    }
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
                    "branch_meta_relpath",
                    "data_root",
                    "graph_db_relpath",
                    "project_id",
                    "project_root",
                    "schema_version",
                    "sessions_db_relpath",
                    "store_kind",
                    "storage_mode",
                ],
            ),
            ReleasedStoreKind::RepositoryIdentity => (
                None,
                Some(1),
                STORAGE_SCHEMA_SOURCE_SHA256,
                &["git_common_dir", "project_id", "schema_version"],
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
    ExternalSourceAuthorityRevisions,
    ExternalSourceProjectionPublications,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FinalSchemaInvariant {
    ExactProjectAndSourceGeneration,
    CompareAndSwap,
    OneUse,
    DurableReplayDeduplication,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalSchemaFamilyManifest {
    pub family: FinalSchemaFamily,
    pub invariants: BTreeSet<FinalSchemaInvariant>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalTargetSchemaManifest {
    pub schema_id: &'static str,
    pub project_schema_version: u32,
    pub lcm_schema_version: u32,
    pub store_manifest_schema_version: u32,
    pub repository_identity_schema_version: u32,
    pub profile_identity_schema_version: u32,
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
            ExternalSourceAuthorityRevisions,
            ExternalSourceProjectionPublications,
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
                | BranchStackQuarantine => {
                    BTreeSet::from([FinalSchemaInvariant::ExactProjectAndSourceGeneration])
                }
                BranchStackConsumedApprovals => BTreeSet::from([
                    FinalSchemaInvariant::ExactProjectAndSourceGeneration,
                    FinalSchemaInvariant::OneUse,
                ]),
                ExternalSourceAuthorityRevisions => BTreeSet::from([
                    FinalSchemaInvariant::ExactProjectAndSourceGeneration,
                    FinalSchemaInvariant::CompareAndSwap,
                    FinalSchemaInvariant::DurableReplayDeduplication,
                ]),
                ExternalSourceProjectionPublications => BTreeSet::from([
                    FinalSchemaInvariant::ExactProjectAndSourceGeneration,
                    FinalSchemaInvariant::CompareAndSwap,
                    FinalSchemaInvariant::DurableReplayDeduplication,
                ]),
            };
            FinalSchemaFamilyManifest { family, invariants }
        })
        .collect();
        Self {
            schema_id: FINAL_V2_SCHEMA_ID,
            project_schema_version: FINAL_PROJECT_SCHEMA_VERSION,
            lcm_schema_version: FINAL_LCM_SCHEMA_VERSION,
            store_manifest_schema_version: FINAL_STORE_MANIFEST_SCHEMA_VERSION,
            repository_identity_schema_version: FINAL_REPOSITORY_IDENTITY_SCHEMA_VERSION,
            profile_identity_schema_version: FINAL_PROFILE_IDENTITY_SCHEMA_VERSION,
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
        if self.schema_id != FINAL_V2_SCHEMA_ID
            || self.project_schema_version != FINAL_PROJECT_SCHEMA_VERSION
            || self.lcm_schema_version != FINAL_LCM_SCHEMA_VERSION
            || self.store_manifest_schema_version != FINAL_STORE_MANIFEST_SCHEMA_VERSION
            || self.repository_identity_schema_version != FINAL_REPOSITORY_IDENTITY_SCHEMA_VERSION
            || self.profile_identity_schema_version != FINAL_PROFILE_IDENTITY_SCHEMA_VERSION
            || self.families.len() != 15
        {
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

/// Construction inputs for [`ExactMigrationSourceIdentity`].
///
/// This is an intentional break from the previous eight-argument
/// [`ExactMigrationSourceIdentity::new`] signature. The type is crate-public
/// and re-exported by the root package, but every production and test caller
/// lived inside this workspace; callers now pass one typed request. Persisted
/// serde shapes remain those of [`ExactMigrationSourceIdentity`] itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactMigrationSourceIdentityRequest {
    pub profile_id: String,
    pub repository_id: String,
    pub project_id: String,
    pub store_id: String,
    pub runtime_binding: StoreRuntimeBindingV1,
    pub verified_locator: VerifiedStoreLocatorV1,
    pub material_digest: [u8; 32],
    pub schema_id: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExactMigrationSourceIdentity {
    pub profile_id: String,
    pub repository_id: String,
    pub project_id: String,
    pub store_id: String,
    pub runtime_binding: StoreRuntimeBindingV1,
    pub verified_locator: VerifiedStoreLocatorV1,
    pub material_digest: [u8; 32],
    pub schema_id: String,
}

impl ExactMigrationSourceIdentity {
    pub fn new(
        request: ExactMigrationSourceIdentityRequest,
    ) -> Result<Self, MigrationContractError> {
        Self::try_from(request)
    }

    pub fn validate(&self) -> Result<(), MigrationContractError> {
        let StoreShardScopeV1::Code {
            project_id,
            repository_id,
            ..
        } = &self.runtime_binding.shard_id.scope
        else {
            return Err(MigrationContractError::IdentityMismatch);
        };
        if self.profile_id.is_empty()
            || self.repository_id.is_empty()
            || self.project_id.is_empty()
            || self.store_id.is_empty()
            || self.runtime_binding.shard_id.profile_id.as_str() != self.profile_id
            || project_id.as_str() != self.project_id
            || repository_id.as_str() != self.repository_id
            || self.verified_locator.shard_id != self.runtime_binding.shard_id
            || self.verified_locator.incarnation != self.runtime_binding.incarnation
            || self.material_digest == [0; 32]
            || self.schema_id != LAST_RELEASED_SCHEMA_ID
        {
            return Err(MigrationContractError::SourceSchemaMismatch);
        }
        Ok(())
    }
}

impl TryFrom<ExactMigrationSourceIdentityRequest> for ExactMigrationSourceIdentity {
    type Error = MigrationContractError;

    fn try_from(request: ExactMigrationSourceIdentityRequest) -> Result<Self, Self::Error> {
        let identity = Self {
            profile_id: request.profile_id,
            repository_id: request.repository_id,
            project_id: request.project_id,
            store_id: request.store_id,
            runtime_binding: request.runtime_binding,
            verified_locator: request.verified_locator,
            material_digest: request.material_digest,
            schema_id: request.schema_id,
        };
        identity.validate()?;
        Ok(identity)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FinalV2SchemaEvidence {
    pub source: ExactMigrationSourceIdentity,
    pub schema_id: String,
    pub project_schema_version: u32,
    pub lcm_schema_version: u32,
    pub store_manifest_schema_version: u32,
    pub repository_identity_schema_version: u32,
    pub profile_identity_schema_version: u32,
    pub durable_families: BTreeSet<ReleasedDurableFamily>,
}

impl FinalV2SchemaEvidence {
    pub fn validate(&self) -> Result<(), MigrationContractError> {
        self.source.validate()?;
        if self.schema_id != FINAL_V2_SCHEMA_ID
            || self.project_schema_version != FINAL_PROJECT_SCHEMA_VERSION
            || self.lcm_schema_version != FINAL_LCM_SCHEMA_VERSION
            || self.store_manifest_schema_version != FINAL_STORE_MANIFEST_SCHEMA_VERSION
            || self.repository_identity_schema_version != FINAL_REPOSITORY_IDENTITY_SCHEMA_VERSION
            || self.profile_identity_schema_version != FINAL_PROFILE_IDENTITY_SCHEMA_VERSION
            || self.durable_families != ReleasedDurableFamily::all()
        {
            return Err(MigrationContractError::TargetSchemaMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivedRebuildFamily {
    Graph,
    Vector,
    FullTextSearch,
    CodeGeneration,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FinalV2PreservationReceipt {
    pub source: ExactMigrationSourceIdentity,
    pub preserved_families: BTreeSet<ReleasedDurableFamily>,
    pub before_digest: [u8; 32],
    pub after_digest: [u8; 32],
}

impl FinalV2PreservationReceipt {
    pub fn validate(&self) -> Result<(), MigrationContractError> {
        self.source.validate()?;
        if self.preserved_families != ReleasedDurableFamily::all()
            || self.before_digest == [0; 32]
            || self.after_digest == [0; 32]
            || self.before_digest != self.after_digest
        {
            return Err(MigrationContractError::PreservationInvalid);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FinalV2TransformReceipt {
    pub schema: FinalV2SchemaEvidence,
    pub preservation: FinalV2PreservationReceipt,
    pub rebuilt_derived_families: BTreeSet<DerivedRebuildFamily>,
}

impl FinalV2TransformReceipt {
    pub fn validate(&self) -> Result<(), MigrationContractError> {
        self.schema.validate()?;
        self.preservation.validate()?;
        if self.schema.source != self.preservation.source {
            return Err(MigrationContractError::IdentityMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifiedBackupRole {
    PristineSourceCopy,
    RelocatedIdentityCutover,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedBackupIdentity {
    pub backup_id: String,
    pub source: ExactMigrationSourceIdentity,
    pub archive_id: String,
    pub digest: [u8; 32],
    pub source_material_digest: [u8; 32],
    pub role: VerifiedBackupRole,
    pub reactivation_authorized: bool,
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
            source_material_digest: [0; 32],
            role: VerifiedBackupRole::PristineSourceCopy,
            reactivation_authorized: false,
            verified_at,
        };
        let mut backup = backup;
        backup.source_material_digest = backup.source.material_digest;
        if backup.backup_id.is_empty()
            || backup.archive_id.is_empty()
            || backup.digest == [0; 32]
            || backup.source_material_digest != backup.source.material_digest
            || backup.reactivation_authorized
            || backup.verified_at <= 0
        {
            return Err(MigrationContractError::BackupNotVerified);
        }
        Ok(backup)
    }

    pub fn validate(&self) -> Result<(), MigrationContractError> {
        self.source.validate()?;
        if self.backup_id.is_empty()
            || self.archive_id.is_empty()
            || self.digest == [0; 32]
            || self.source_material_digest != self.source.material_digest
            || self.reactivation_authorized
            || self.verified_at <= 0
        {
            return Err(MigrationContractError::BackupNotVerified);
        }
        Ok(())
    }

    pub fn relocated_identity_cutover(
        backup_id: impl Into<String>,
        source: ExactMigrationSourceIdentity,
        archive_id: impl Into<String>,
        digest: [u8; 32],
        verified_at: i64,
    ) -> Result<Self, MigrationContractError> {
        let mut backup = Self::new(backup_id, source, archive_id, digest, verified_at)?;
        backup.role = VerifiedBackupRole::RelocatedIdentityCutover;
        backup.validate()?;
        Ok(backup)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationCasGrant {
    pub grant_id: String,
    pub migration_id: String,
    pub checkpoint_id: String,
    pub source: ExactMigrationSourceIdentity,
    pub target_evidence: FinalV2SchemaEvidence,
    pub expected_authority_revision: u64,
    pub authority_fence: u64,
}

impl PublicationCasGrant {
    pub fn new(
        grant_id: impl Into<String>,
        migration_id: impl Into<String>,
        checkpoint_id: impl Into<String>,
        source: ExactMigrationSourceIdentity,
        target_evidence: FinalV2SchemaEvidence,
        expected_authority_revision: u64,
        authority_fence: u64,
    ) -> Result<Self, MigrationContractError> {
        let grant = Self {
            grant_id: grant_id.into(),
            migration_id: migration_id.into(),
            checkpoint_id: checkpoint_id.into(),
            source,
            target_evidence,
            expected_authority_revision,
            authority_fence,
        };
        grant.validate()?;
        Ok(grant)
    }

    pub fn validate(&self) -> Result<(), MigrationContractError> {
        self.source.validate()?;
        self.target_evidence.validate()?;
        if self.grant_id.is_empty()
            || self.migration_id.is_empty()
            || self.checkpoint_id.is_empty()
            || self.target_evidence.source != self.source
            || self.expected_authority_revision == u64::MAX
            || self.authority_fence == 0
        {
            return Err(MigrationContractError::PublicationInvalid);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CutoverPublicationReceipt {
    pub receipt_id: String,
    pub source: ExactMigrationSourceIdentity,
    pub target_schema_id: String,
    pub target_evidence: FinalV2SchemaEvidence,
    pub authority_cas_receipt_id: String,
    pub previous_authority_revision: u64,
    pub published_authority_revision: u64,
    pub authority_fence: u64,
    pub published_at: i64,
}

impl CutoverPublicationReceipt {
    pub fn from_cas_grant(
        receipt_id: impl Into<String>,
        source: ExactMigrationSourceIdentity,
        target_schema_id: impl Into<String>,
        grant: &PublicationCasGrant,
        published_at: i64,
    ) -> Result<Self, MigrationContractError> {
        grant.validate()?;
        if grant.source != source {
            return Err(MigrationContractError::IdentityMismatch);
        }
        let receipt = Self {
            receipt_id: receipt_id.into(),
            source,
            target_schema_id: target_schema_id.into(),
            target_evidence: grant.target_evidence.clone(),
            authority_cas_receipt_id: grant.grant_id.clone(),
            previous_authority_revision: grant.expected_authority_revision,
            published_authority_revision: grant
                .expected_authority_revision
                .checked_add(1)
                .ok_or(MigrationContractError::PublicationInvalid)?,
            authority_fence: grant.authority_fence,
            published_at,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    pub fn validate(&self) -> Result<(), MigrationContractError> {
        self.source.validate()?;
        self.target_evidence.validate()?;
        if self.receipt_id.is_empty()
            || self.target_schema_id != FINAL_V2_SCHEMA_ID
            || self.target_evidence.source != self.source
            || self.target_evidence.schema_id != self.target_schema_id
            || self.authority_cas_receipt_id.is_empty()
            || self.published_authority_revision
                != self.previous_authority_revision.saturating_add(1)
            || self.authority_fence == 0
            || self.published_at <= 0
        {
            return Err(MigrationContractError::PublicationInvalid);
        }
        Ok(())
    }

    pub fn validate_for_grant(
        &self,
        grant: &PublicationCasGrant,
    ) -> Result<(), MigrationContractError> {
        self.validate()?;
        grant.validate()?;
        if self.source != grant.source
            || self.target_evidence != grant.target_evidence
            || self.authority_cas_receipt_id != grant.grant_id
            || self.previous_authority_revision != grant.expected_authority_revision
            || self.authority_fence != grant.authority_fence
        {
            return Err(MigrationContractError::PublicationInvalid);
        }
        Ok(())
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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DurableMigrationCheckpoint {
    pub checkpoint_id: String,
    pub migration_id: String,
    pub source: ExactMigrationSourceIdentity,
    pub backup: VerifiedBackupIdentity,
    pub prepared_at: i64,
    pub transformation: Option<FinalV2TransformReceipt>,
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
            transformation: None,
            publication: None,
        };
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    pub fn record_publication(
        &mut self,
        receipt: CutoverPublicationReceipt,
        grant: &PublicationCasGrant,
    ) -> Result<(), MigrationContractError> {
        receipt.validate_for_grant(grant)?;
        if receipt.source != self.source
            || grant.source != self.source
            || self.transformation.is_none()
            || self.publication.is_some()
        {
            return Err(MigrationContractError::IdentityMismatch);
        }
        self.publication = Some(receipt);
        Ok(())
    }

    pub fn record_transformation(
        &mut self,
        receipt: FinalV2TransformReceipt,
    ) -> Result<(), MigrationContractError> {
        receipt.validate()?;
        if receipt.schema.source != self.source
            || self.publication.is_some()
            || self.transformation.is_some()
        {
            return Err(MigrationContractError::IdentityMismatch);
        }
        self.transformation = Some(receipt);
        Ok(())
    }

    pub const fn is_published(&self) -> bool {
        self.publication.is_some()
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
        self.source.validate()?;
        self.backup.validate()?;
        if let Some(transformation) = &self.transformation {
            transformation.validate()?;
        }
        if let Some(publication) = &self.publication {
            publication.validate()?;
        }
        if self.checkpoint_id.is_empty()
            || self.migration_id.is_empty()
            || self.prepared_at <= 0
            || self.prepared_at > self.backup.verified_at
            || self.backup.source != self.source
            || self.transformation.as_ref().is_some_and(|transformation| {
                transformation.schema.source != self.source
                    || transformation.preservation.source != self.source
            })
            || self.publication.as_ref().is_some_and(|publication| {
                publication.source != self.source
                    || publication.published_at < self.backup.verified_at
            })
            || (self.publication.is_some() && self.transformation.is_none())
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
    PreservationInvalid,
    ArchivePolicyMismatch,
    ArchiveNotYetEligible,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_store::{
        BrainId, CodeShardScopeV1, LocatorDigest, ProjectId, RepositoryId, StoreAuthorityEpochV1,
        StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1, UserProfileId,
        VerifiedStoreLocatorV1, WorktreeId,
    };

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn valid_source_request() -> ExactMigrationSourceIdentityRequest {
        let shard_id = StoreShardIdV1::code(
            id::<BrainId>("brain.source"),
            id::<UserProfileId>("profile.source"),
            id::<ProjectId>("project.source"),
            id::<RepositoryId>("repository.source"),
            CodeShardScopeV1::Worktree {
                worktree_id: id::<WorktreeId>("worktree.source"),
            },
        );
        let incarnation = StoreIncarnationV1::new(1).unwrap();
        let runtime_binding = StoreRuntimeBindingV1::new(
            shard_id.clone(),
            incarnation,
            StoreAuthorityEpochV1::new(1).unwrap(),
        );
        let verified_locator = VerifiedStoreLocatorV1::new(
            shard_id,
            incarnation,
            LocatorDigest::new(format!("sha256:{:064x}", 9)).unwrap(),
        );
        ExactMigrationSourceIdentityRequest {
            profile_id: "profile.source".to_owned(),
            repository_id: "repository.source".to_owned(),
            project_id: "project.source".to_owned(),
            store_id: "store.source".to_owned(),
            runtime_binding,
            verified_locator,
            material_digest: [9; 32],
            schema_id: LAST_RELEASED_SCHEMA_ID.to_owned(),
        }
    }

    #[test]
    fn source_identity_constructs_from_typed_request() {
        let request = valid_source_request();
        let identity = ExactMigrationSourceIdentity::new(request.clone()).unwrap();
        assert_eq!(identity.profile_id, request.profile_id);
        assert_eq!(identity.repository_id, request.repository_id);
        assert_eq!(identity.project_id, request.project_id);
        assert_eq!(identity.store_id, request.store_id);
        assert_eq!(identity.runtime_binding, request.runtime_binding);
        assert_eq!(identity.verified_locator, request.verified_locator);
        assert_eq!(identity.material_digest, request.material_digest);
        assert_eq!(identity.schema_id, LAST_RELEASED_SCHEMA_ID);
        identity.validate().unwrap();
    }

    #[test]
    fn source_identity_request_rejects_invalid_inputs() {
        let mut empty_profile = valid_source_request();
        empty_profile.profile_id.clear();
        assert_eq!(
            ExactMigrationSourceIdentity::new(empty_profile),
            Err(MigrationContractError::SourceSchemaMismatch)
        );

        let mut zero_digest = valid_source_request();
        zero_digest.material_digest = [0; 32];
        assert_eq!(
            ExactMigrationSourceIdentity::new(zero_digest),
            Err(MigrationContractError::SourceSchemaMismatch)
        );

        let mut wrong_schema = valid_source_request();
        wrong_schema.schema_id = "tracedecay.not-released".to_owned();
        assert_eq!(
            ExactMigrationSourceIdentity::new(wrong_schema),
            Err(MigrationContractError::SourceSchemaMismatch)
        );

        let mut mismatched_project = valid_source_request();
        mismatched_project.project_id = "project.other".to_owned();
        assert_eq!(
            ExactMigrationSourceIdentity::new(mismatched_project),
            Err(MigrationContractError::SourceSchemaMismatch)
        );
    }

    #[test]
    fn source_identity_round_trips_persisted_shape() {
        let identity = ExactMigrationSourceIdentity::new(valid_source_request()).unwrap();
        let encoded = serde_json::to_string(&identity).unwrap();
        let decoded: ExactMigrationSourceIdentity = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, identity);
        decoded.validate().unwrap();
        assert!(
            !encoded.contains("ExactMigrationSourceIdentityRequest"),
            "request type must not appear in persisted JSON"
        );
    }

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
