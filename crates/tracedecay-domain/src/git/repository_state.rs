//! Immutable repository-state snapshots for exact Git preconditions.
//!
//! Native Git captures these values. This module does not open repositories,
//! parse Git configuration, mutate an index, or infer a clean state from
//! partial evidence.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::git::{GitCoverageV1, GitHeadStateV1, GitObjectFormatV1, GitOidV1, GitOperationStateV1};
use crate::research::{
    DomainError, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId, canonical_sha256,
};

const REPOSITORY_STATE_ID_DOMAIN: &str = "tracedecay.repository-state.v1";

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct RepositoryStateSnapshotId(String);

fn validate_repository_state_snapshot_id(value: &str) -> Result<(), DomainError> {
    crate::canonical_text::validate_canonical_identity(value, "repository state snapshot id")
}

impl RepositoryStateSnapshotId {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        validate_repository_state_snapshot_id(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        validate_repository_state_snapshot_id(&self.0)
    }
}

impl<'de> Deserialize<'de> for RepositoryStateSnapshotId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for RepositoryStateSnapshotId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryIndexStateV1 {
    Clean,
    Staged,
    Unmerged,
    IntentToAdd,
    Split,
    Sparse,
    Unreadable,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryWorkingTreeStateV1 {
    Clean,
    TrackedDirty,
    UntrackedOnly,
    Mixed,
    Conflicted,
    Unreadable,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepositoryIndexSnapshotV1 {
    pub checksum: ManifestDigest,
    pub tree_id: Option<GitOidV1>,
    pub state: RepositoryIndexStateV1,
    pub unmerged_stage_digest: Option<ManifestDigest>,
}

impl RepositoryIndexSnapshotV1 {
    pub fn validate(&self, object_format: GitObjectFormatV1) -> Result<(), DomainError> {
        self.checksum.validate()?;
        if let Some(tree_id) = &self.tree_id {
            tree_id.validate()?;
            if tree_id.format() != object_format {
                return Err(DomainError::NonCanonical {
                    field: "repository index tree object format",
                });
            }
        }
        self.unmerged_stage_digest
            .as_ref()
            .map_or(Ok(()), ManifestDigest::validate)?;
        match self.state {
            RepositoryIndexStateV1::Unmerged if self.unmerged_stage_digest.is_none() => {
                Err(DomainError::Empty {
                    field: "repository unmerged index stage digest",
                })
            }
            RepositoryIndexStateV1::Clean
            | RepositoryIndexStateV1::Staged
            | RepositoryIndexStateV1::IntentToAdd
            | RepositoryIndexStateV1::Split
            | RepositoryIndexStateV1::Sparse
            | RepositoryIndexStateV1::Unreadable
            | RepositoryIndexStateV1::Unmerged => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepositoryWorkingTreeSnapshotV1 {
    pub state: RepositoryWorkingTreeStateV1,
    pub tracked_digest: ManifestDigest,
    pub untracked_name_digest: Option<ManifestDigest>,
    pub ignored_collision_digest: Option<ManifestDigest>,
}

impl RepositoryWorkingTreeSnapshotV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.tracked_digest.validate()?;
        self.untracked_name_digest
            .as_ref()
            .map_or(Ok(()), ManifestDigest::validate)?;
        self.ignored_collision_digest
            .as_ref()
            .map_or(Ok(()), ManifestDigest::validate)?;
        if self.state == RepositoryWorkingTreeStateV1::UntrackedOnly
            && self.untracked_name_digest.is_none()
        {
            return Err(DomainError::Empty {
                field: "untracked working tree name digest",
            });
        }
        Ok(())
    }
}

/// Immutable content-addressed native repository state. Missing/partial
/// evidence remains typed by fields and coverage instead of being upgraded to
/// a guessed clean snapshot.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepositoryStateSnapshotV1 {
    pub snapshot_id: RepositoryStateSnapshotId,
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub worktree_id: Option<WorktreeId>,
    pub observation_epoch: u64,
    pub object_format: GitObjectFormatV1,
    /// Exact native Git implementation observed by the fixed adapter. A
    /// read-only partial snapshot may omit this, but omitted native evidence
    /// is never mutation eligible.
    pub git_version: Option<String>,
    /// Revision of the fixed native adapter that interpreted this state.
    pub adapter_revision: Option<String>,
    /// Digest of the complete native ref namespace at capture time.
    pub refs_digest: Option<ManifestDigest>,
    pub head: GitHeadStateV1,
    pub index: RepositoryIndexSnapshotV1,
    pub working_tree: RepositoryWorkingTreeSnapshotV1,
    pub operation_state: GitOperationStateV1,
    pub configuration_digest: Option<ManifestDigest>,
    pub attributes_digest: Option<ManifestDigest>,
    pub sparse_digest: Option<ManifestDigest>,
    pub submodule_digest: Option<ManifestDigest>,
    pub filesystem_capabilities_digest: Option<ManifestDigest>,
    pub captured_at: UtcMicros,
    pub coverage: GitCoverageV1,
}

impl RepositoryStateSnapshotV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_id: ProjectId,
        repository_id: RepositoryId,
        worktree_id: Option<WorktreeId>,
        observation_epoch: u64,
        object_format: GitObjectFormatV1,
        head: GitHeadStateV1,
        index: RepositoryIndexSnapshotV1,
        working_tree: RepositoryWorkingTreeSnapshotV1,
        operation_state: GitOperationStateV1,
        configuration_digest: Option<ManifestDigest>,
        attributes_digest: Option<ManifestDigest>,
        sparse_digest: Option<ManifestDigest>,
        submodule_digest: Option<ManifestDigest>,
        filesystem_capabilities_digest: Option<ManifestDigest>,
        captured_at: UtcMicros,
        coverage: GitCoverageV1,
    ) -> Result<Self, DomainError> {
        let mut snapshot = Self {
            snapshot_id: RepositoryStateSnapshotId::new("repository.state.pending")?,
            project_id,
            repository_id,
            worktree_id,
            observation_epoch,
            object_format,
            git_version: None,
            adapter_revision: None,
            refs_digest: None,
            head,
            index,
            working_tree,
            operation_state,
            configuration_digest,
            attributes_digest,
            sparse_digest,
            submodule_digest,
            filesystem_capabilities_digest,
            captured_at,
            coverage,
        };
        snapshot.validate_fields()?;
        snapshot.snapshot_id = snapshot.derive_snapshot_id()?;
        Ok(snapshot)
    }

    /// Bind native implementation and ref-namespace identity to a freshly
    /// captured snapshot. The snapshot ID is re-derived so callers cannot add
    /// this evidence after a preview has been issued.
    pub fn with_native_identity(
        mut self,
        git_version: String,
        adapter_revision: String,
        refs_digest: ManifestDigest,
    ) -> Result<Self, DomainError> {
        validate_native_identity(&git_version, "repository git version")?;
        validate_native_identity(&adapter_revision, "repository git adapter revision")?;
        refs_digest.validate()?;
        self.git_version = Some(git_version);
        self.adapter_revision = Some(adapter_revision);
        self.refs_digest = Some(refs_digest);
        self.validate_fields()?;
        self.snapshot_id = self.derive_snapshot_id()?;
        Ok(self)
    }

    pub fn snapshot_id(&self) -> &RepositoryStateSnapshotId {
        &self.snapshot_id
    }

    /// Whether the snapshot is truthful enough for a caller to ask the native
    /// operation layer for a mutation preview. This does not itself grant or
    /// perform mutation authority.
    pub fn is_mutation_eligible(&self) -> bool {
        self.git_version.is_some()
            && self.adapter_revision.is_some()
            && self.refs_digest.is_some()
            && self.configuration_digest.is_some()
            && self.attributes_digest.is_some()
            && self.sparse_digest.is_some()
            && self.submodule_digest.is_some()
            && self.filesystem_capabilities_digest.is_some()
            && !self.coverage.leaves_state_unread()
            && !matches!(
                self.index.state,
                RepositoryIndexStateV1::Unmerged
                    | RepositoryIndexStateV1::IntentToAdd
                    | RepositoryIndexStateV1::Split
                    | RepositoryIndexStateV1::Sparse
                    | RepositoryIndexStateV1::Unreadable
            )
            && !matches!(
                self.working_tree.state,
                RepositoryWorkingTreeStateV1::Conflicted | RepositoryWorkingTreeStateV1::Unreadable
            )
            && self.operation_state == GitOperationStateV1::None
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.snapshot_id.validate()?;
        self.validate_fields()?;
        if self.snapshot_id != self.derive_snapshot_id()? {
            return Err(DomainError::SnapshotMismatch {
                field: "repository state snapshot id",
            });
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), DomainError> {
        self.project_id.validate()?;
        self.repository_id.validate()?;
        self.worktree_id
            .as_ref()
            .map_or(Ok(()), WorktreeId::validate)?;
        if self.observation_epoch == 0 {
            return Err(DomainError::NonCanonical {
                field: "repository observation epoch",
            });
        }
        if let Some(version) = &self.git_version {
            validate_native_identity(version, "repository git version")?;
        }
        if let Some(revision) = &self.adapter_revision {
            validate_native_identity(revision, "repository git adapter revision")?;
        }
        self.refs_digest
            .as_ref()
            .map_or(Ok(()), ManifestDigest::validate)?;
        if self.git_version.is_some() != self.adapter_revision.is_some()
            || self.git_version.is_some() != self.refs_digest.is_some()
        {
            return Err(DomainError::NonCanonical {
                field: "repository native identity completeness",
            });
        }
        self.head.validate()?;
        if let Some(commit) = self.head.commit() {
            commit.validate()?;
            if commit.format() != self.object_format {
                return Err(DomainError::NonCanonical {
                    field: "repository head object format",
                });
            }
        }
        self.index.validate(self.object_format)?;
        self.working_tree.validate()?;
        self.configuration_digest
            .as_ref()
            .map_or(Ok(()), ManifestDigest::validate)?;
        self.attributes_digest
            .as_ref()
            .map_or(Ok(()), ManifestDigest::validate)?;
        self.sparse_digest
            .as_ref()
            .map_or(Ok(()), ManifestDigest::validate)?;
        self.submodule_digest
            .as_ref()
            .map_or(Ok(()), ManifestDigest::validate)?;
        self.filesystem_capabilities_digest
            .as_ref()
            .map_or(Ok(()), ManifestDigest::validate)?;
        self.coverage.validate()
    }

    fn derive_snapshot_id(&self) -> Result<RepositoryStateSnapshotId, DomainError> {
        #[derive(Serialize)]
        struct SnapshotMaterial<'a> {
            project_id: &'a ProjectId,
            repository_id: &'a RepositoryId,
            worktree_id: Option<&'a WorktreeId>,
            observation_epoch: u64,
            object_format: GitObjectFormatV1,
            git_version: Option<&'a str>,
            adapter_revision: Option<&'a str>,
            refs_digest: Option<&'a ManifestDigest>,
            head: &'a GitHeadStateV1,
            index: &'a RepositoryIndexSnapshotV1,
            working_tree: &'a RepositoryWorkingTreeSnapshotV1,
            operation_state: GitOperationStateV1,
            configuration_digest: Option<&'a ManifestDigest>,
            attributes_digest: Option<&'a ManifestDigest>,
            sparse_digest: Option<&'a ManifestDigest>,
            submodule_digest: Option<&'a ManifestDigest>,
            filesystem_capabilities_digest: Option<&'a ManifestDigest>,
            captured_at: UtcMicros,
            coverage: &'a GitCoverageV1,
        }

        let digest = canonical_sha256(&(
            REPOSITORY_STATE_ID_DOMAIN,
            SnapshotMaterial {
                project_id: &self.project_id,
                repository_id: &self.repository_id,
                worktree_id: self.worktree_id.as_ref(),
                observation_epoch: self.observation_epoch,
                object_format: self.object_format,
                git_version: self.git_version.as_deref(),
                adapter_revision: self.adapter_revision.as_deref(),
                refs_digest: self.refs_digest.as_ref(),
                head: &self.head,
                index: &self.index,
                working_tree: &self.working_tree,
                operation_state: self.operation_state,
                configuration_digest: self.configuration_digest.as_ref(),
                attributes_digest: self.attributes_digest.as_ref(),
                sparse_digest: self.sparse_digest.as_ref(),
                submodule_digest: self.submodule_digest.as_ref(),
                filesystem_capabilities_digest: self.filesystem_capabilities_digest.as_ref(),
                captured_at: self.captured_at,
                coverage: &self.coverage,
            },
        ))?;
        let encoded = crate::canonical_text::sha256_hex_body(
            digest.as_str(),
            "repository state snapshot digest",
        )?;
        RepositoryStateSnapshotId::new(format!("repository.state.v1.{encoded}"))
    }
}

impl<'de> Deserialize<'de> for RepositoryStateSnapshotV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            snapshot_id: RepositoryStateSnapshotId,
            project_id: ProjectId,
            repository_id: RepositoryId,
            worktree_id: Option<WorktreeId>,
            observation_epoch: u64,
            object_format: GitObjectFormatV1,
            git_version: Option<String>,
            adapter_revision: Option<String>,
            refs_digest: Option<ManifestDigest>,
            head: GitHeadStateV1,
            index: RepositoryIndexSnapshotV1,
            working_tree: RepositoryWorkingTreeSnapshotV1,
            operation_state: GitOperationStateV1,
            configuration_digest: Option<ManifestDigest>,
            attributes_digest: Option<ManifestDigest>,
            sparse_digest: Option<ManifestDigest>,
            submodule_digest: Option<ManifestDigest>,
            filesystem_capabilities_digest: Option<ManifestDigest>,
            captured_at: UtcMicros,
            coverage: GitCoverageV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        let mut snapshot = Self::new(
            wire.project_id,
            wire.repository_id,
            wire.worktree_id,
            wire.observation_epoch,
            wire.object_format,
            wire.head,
            wire.index,
            wire.working_tree,
            wire.operation_state,
            wire.configuration_digest,
            wire.attributes_digest,
            wire.sparse_digest,
            wire.submodule_digest,
            wire.filesystem_capabilities_digest,
            wire.captured_at,
            wire.coverage,
        )
        .map_err(serde::de::Error::custom)?;
        snapshot.git_version = wire.git_version;
        snapshot.adapter_revision = wire.adapter_revision;
        snapshot.refs_digest = wire.refs_digest;
        snapshot
            .validate_fields()
            .map_err(serde::de::Error::custom)?;
        snapshot.snapshot_id = snapshot
            .derive_snapshot_id()
            .map_err(serde::de::Error::custom)?;
        if snapshot.snapshot_id != wire.snapshot_id {
            return Err(serde::de::Error::custom(
                "repository state snapshot id does not match its canonical state",
            ));
        }
        Ok(snapshot)
    }
}

use crate::canonical_text::validate_canonical_identity as validate_native_identity;
