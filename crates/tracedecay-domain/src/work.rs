//! Canonical Work authority and runtime evidence identities.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{
    ActorId, ManifestDigest, ProjectId, ProjectionGenerationId, RepositoryId, RunId, WorktreeId,
    canonical_sha256,
};

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkContractError {
    #[error("work version must be non-zero")]
    InvalidVersion,
    #[error("work projection generation could not be derived from authority")]
    InvalidProjectionGeneration,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct WorkVersion(u64);

impl WorkVersion {
    pub fn new(value: u64) -> Result<Self, WorkContractError> {
        if value == 0 {
            return Err(WorkContractError::InvalidVersion);
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for WorkVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(
    Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(deny_unknown_fields)]
pub struct WorkAuthority {
    project_id: ProjectId,
    repository_id: RepositoryId,
    worktree_id: WorktreeId,
    actor_id: ActorId,
    policy_digest: ManifestDigest,
}

impl WorkAuthority {
    pub fn new(
        project_id: ProjectId,
        repository_id: RepositoryId,
        worktree_id: WorktreeId,
        actor_id: ActorId,
        policy_digest: ManifestDigest,
    ) -> Result<Self, WorkContractError> {
        Ok(Self {
            project_id,
            repository_id,
            worktree_id,
            actor_id,
            policy_digest,
        })
    }

    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    pub fn repository_id(&self) -> &RepositoryId {
        &self.repository_id
    }

    pub fn worktree_id(&self) -> &WorktreeId {
        &self.worktree_id
    }

    pub fn actor_id(&self) -> &ActorId {
        &self.actor_id
    }

    pub fn policy_digest(&self) -> &ManifestDigest {
        &self.policy_digest
    }

    pub fn projection_generation_id(&self) -> Result<ProjectionGenerationId, WorkContractError> {
        let digest = canonical_sha256(&("tracedecay.work.projection.generation.v1", self))
            .map_err(|_| WorkContractError::InvalidProjectionGeneration)?;
        let hex = digest.hex_suffix().unwrap_or(digest.as_str());
        ProjectionGenerationId::try_from(format!("generation.work.{hex}"))
            .map_err(|_| WorkContractError::InvalidProjectionGeneration)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEvidenceRef {
    run_id: RunId,
    evidence_digest: ManifestDigest,
    terminal: bool,
}

impl RuntimeEvidenceRef {
    pub fn new(
        run_id: RunId,
        evidence_digest: ManifestDigest,
        terminal: bool,
    ) -> Result<Self, WorkContractError> {
        Ok(Self {
            run_id,
            evidence_digest,
            terminal,
        })
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub fn evidence_digest(&self) -> &ManifestDigest {
        &self.evidence_digest
    }

    pub const fn is_terminal(&self) -> bool {
        self.terminal
    }
}
