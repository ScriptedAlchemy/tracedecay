//! Payload-free, provenance-bound cross-domain graph locators.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    CodeGenerationId, CommitId, ManifestDigest, ProjectionGenerationId, RepositoryId,
    RetrievalAnchorId, SessionId, SymbolOccurrenceId, TaskId, WorkflowDefinitionId,
    WorkflowStepId, canonical_sha256,
};

#[derive(
    Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(tag = "domain", rename_all = "snake_case", deny_unknown_fields)]
pub enum CrossReferenceTargetV1 {
    RetrievalAnchor {
        anchor_id: RetrievalAnchorId,
    },
    CodeSymbol {
        generation_id: CodeGenerationId,
        occurrence_id: SymbolOccurrenceId,
    },
    GitCommit {
        repository_id: RepositoryId,
        commit_id: CommitId,
    },
    WorkTask {
        generation_id: ProjectionGenerationId,
        task_id: TaskId,
    },
    Session {
        session_id: SessionId,
    },
    WorkflowStep {
        definition_id: WorkflowDefinitionId,
        definition_version: u64,
        step_id: WorkflowStepId,
    },
}

impl CrossReferenceTargetV1 {
    pub fn validate(&self) -> Result<(), CrossReferenceContractError> {
        match self {
            Self::RetrievalAnchor { anchor_id } => anchor_id.validate(),
            Self::CodeSymbol {
                generation_id,
                occurrence_id,
            } => generation_id
                .validate()
                .and_then(|()| occurrence_id.validate()),
            Self::GitCommit {
                repository_id,
                commit_id,
            } => repository_id
                .validate()
                .and_then(|()| commit_id.validate()),
            Self::WorkTask {
                generation_id,
                task_id,
            } => generation_id.validate().and_then(|()| task_id.validate()),
            Self::Session { session_id } => session_id.validate(),
            Self::WorkflowStep {
                definition_id,
                definition_version,
                step_id,
            } => {
                if *definition_version == 0 {
                    return Err(CrossReferenceContractError::InvalidDefinitionVersion);
                }
                definition_id
                    .validate()
                    .and_then(|()| step_id.validate())
            }
        }
        .map_err(|error| CrossReferenceContractError::InvalidTarget(error.to_string()))
    }

    pub const fn domain(&self) -> &'static str {
        match self {
            Self::RetrievalAnchor { .. } => "retrieval_anchor",
            Self::CodeSymbol { .. } => "code_symbol",
            Self::GitCommit { .. } => "git_commit",
            Self::WorkTask { .. } => "work_task",
            Self::Session { .. } => "session",
            Self::WorkflowStep { .. } => "workflow_step",
        }
    }
}

#[derive(
    Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum CrossReferenceRelationV1 {
    CodeObservedAtCommit,
    EvidenceSupports,
    SessionProducedCommit,
    WorkSupportedBy,
    WorkflowExecutesWork,
    Related,
}

impl CrossReferenceRelationV1 {
    pub const fn graph_kind(self) -> &'static str {
        match self {
            Self::CodeObservedAtCommit => "CrossReferenceCodeObservedAtCommit",
            Self::EvidenceSupports => "CrossReferenceEvidenceSupports",
            Self::SessionProducedCommit => "CrossReferenceSessionProducedCommit",
            Self::WorkSupportedBy => "CrossReferenceWorkSupportedBy",
            Self::WorkflowExecutesWork => "CrossReferenceWorkflowExecutesWork",
            Self::Related => "CrossReferenceRelated",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CrossReferenceLocatorV1 {
    locator_digest: ManifestDigest,
    scope_digest: ManifestDigest,
    provenance_digest: ManifestDigest,
    evidence_anchor_id: RetrievalAnchorId,
    relation: CrossReferenceRelationV1,
    source: CrossReferenceTargetV1,
    target: CrossReferenceTargetV1,
}

impl CrossReferenceLocatorV1 {
    pub fn new(
        scope_digest: ManifestDigest,
        provenance_digest: ManifestDigest,
        evidence_anchor_id: RetrievalAnchorId,
        relation: CrossReferenceRelationV1,
        source: CrossReferenceTargetV1,
        target: CrossReferenceTargetV1,
    ) -> Result<Self, CrossReferenceContractError> {
        let locator_digest = locator_digest(
            &scope_digest,
            &provenance_digest,
            &evidence_anchor_id,
            relation,
            &source,
            &target,
        )?;
        let locator = Self {
            locator_digest,
            scope_digest,
            provenance_digest,
            evidence_anchor_id,
            relation,
            source,
            target,
        };
        locator.validate()?;
        Ok(locator)
    }

    pub fn locator_digest(&self) -> &ManifestDigest {
        &self.locator_digest
    }

    pub fn scope_digest(&self) -> &ManifestDigest {
        &self.scope_digest
    }

    pub fn provenance_digest(&self) -> &ManifestDigest {
        &self.provenance_digest
    }

    pub fn evidence_anchor_id(&self) -> &RetrievalAnchorId {
        &self.evidence_anchor_id
    }

    pub const fn relation(&self) -> CrossReferenceRelationV1 {
        self.relation
    }

    pub fn source(&self) -> &CrossReferenceTargetV1 {
        &self.source
    }

    pub fn target(&self) -> &CrossReferenceTargetV1 {
        &self.target
    }

    pub fn validate(&self) -> Result<(), CrossReferenceContractError> {
        self.scope_digest
            .validate()
            .and_then(|()| self.provenance_digest.validate())
            .and_then(|()| self.evidence_anchor_id.validate())
            .map_err(|error| CrossReferenceContractError::InvalidLocator(error.to_string()))?;
        self.source.validate()?;
        self.target.validate()?;
        if self.source.domain() == self.target.domain() {
            return Err(CrossReferenceContractError::NotCrossDomain);
        }
        let expected = locator_digest(
            &self.scope_digest,
            &self.provenance_digest,
            &self.evidence_anchor_id,
            self.relation,
            &self.source,
            &self.target,
        )?;
        if expected != self.locator_digest {
            return Err(CrossReferenceContractError::DigestMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CrossReferenceContractError {
    #[error("cross-reference target is invalid: {0}")]
    InvalidTarget(String),
    #[error("cross-reference locator is invalid: {0}")]
    InvalidLocator(String),
    #[error("workflow definition version must be non-zero")]
    InvalidDefinitionVersion,
    #[error("cross-reference locator must connect different domains")]
    NotCrossDomain,
    #[error("cross-reference locator digest does not bind its identity and provenance")]
    DigestMismatch,
}

fn locator_digest(
    scope_digest: &ManifestDigest,
    provenance_digest: &ManifestDigest,
    evidence_anchor_id: &RetrievalAnchorId,
    relation: CrossReferenceRelationV1,
    source: &CrossReferenceTargetV1,
    target: &CrossReferenceTargetV1,
) -> Result<ManifestDigest, CrossReferenceContractError> {
    canonical_sha256(&(
        "tracedecay.cross-reference-locator.v1",
        scope_digest,
        provenance_digest,
        evidence_anchor_id,
        relation,
        source,
        target,
    ))
    .map_err(|error| CrossReferenceContractError::InvalidLocator(error.to_string()))
}
