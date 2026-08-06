use serde::{Deserialize, Serialize};

use crate::canonical_text::{canonical_framed_sha256, is_canonical_text_within};
use crate::research::DomainError;
use crate::{CodeGenerationId, ContentDigest, GitOidV1, ProjectId, SessionId, TaskId};

const WATERMARK_MAX_BYTES: usize = 512;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum GitGraphEvidenceTarget {
    CodeGeneration(CodeGenerationId),
    Session(SessionId),
    Work(TaskId),
}

impl GitGraphEvidenceTarget {
    fn identity_parts(&self) -> (&'static str, &str) {
        match self {
            Self::CodeGeneration(id) => ("code_generation", id.as_str()),
            Self::Session(id) => ("session", id.as_str()),
            Self::Work(id) => ("work", id.as_str()),
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::CodeGeneration(id) => id.validate(),
            Self::Session(id) => id.validate(),
            Self::Work(id) => id.validate(),
        }
    }
}

/// Immutable cross-domain evidence intent. Source journals commit this value
/// before the daemon-owned Git convergence owner attempts graph publication.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitGraphEvidenceIntent {
    project_id: ProjectId,
    commit: GitOidV1,
    target: GitGraphEvidenceTarget,
    intent_digest: ContentDigest,
}

impl GitGraphEvidenceIntent {
    pub fn new(
        project_id: ProjectId,
        commit: GitOidV1,
        target: GitGraphEvidenceTarget,
    ) -> Result<Self, DomainError> {
        let intent_digest = derive_intent_digest(&project_id, &commit, &target)?;
        let intent = Self {
            project_id,
            commit,
            target,
            intent_digest,
        };
        intent.validate()?;
        Ok(intent)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.project_id.validate()?;
        self.commit.validate()?;
        self.target.validate()?;
        self.intent_digest.validate()?;
        if self.intent_digest != derive_intent_digest(&self.project_id, &self.commit, &self.target)?
        {
            return Err(DomainError::NonCanonical {
                field: "Git graph evidence intent digest",
            });
        }
        Ok(())
    }

    #[must_use]
    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    #[must_use]
    pub fn commit(&self) -> &GitOidV1 {
        &self.commit
    }

    #[must_use]
    pub fn target(&self) -> &GitGraphEvidenceTarget {
        &self.target
    }

    #[must_use]
    pub fn intent_digest(&self) -> &ContentDigest {
        &self.intent_digest
    }
}

/// Exact acknowledgement persisted by a source journal with compare-and-swap
/// after its intent relation is durable in the project graph.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitGraphEvidencePublicationReceipt {
    intent_digest: ContentDigest,
    graph_watermark: String,
    graph_commit_sequence: u64,
}

impl GitGraphEvidencePublicationReceipt {
    pub fn new(
        intent_digest: ContentDigest,
        graph_watermark: String,
        graph_commit_sequence: u64,
    ) -> Result<Self, DomainError> {
        let receipt = Self {
            intent_digest,
            graph_watermark,
            graph_commit_sequence,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.intent_digest.validate()?;
        if !is_canonical_text_within(&self.graph_watermark, WATERMARK_MAX_BYTES) {
            return Err(DomainError::NonCanonical {
                field: "Git graph evidence publication watermark",
            });
        }
        if self.graph_commit_sequence == 0 {
            return Err(DomainError::NonCanonical {
                field: "Git graph evidence publication sequence",
            });
        }
        Ok(())
    }

    #[must_use]
    pub fn intent_digest(&self) -> &ContentDigest {
        &self.intent_digest
    }

    #[must_use]
    pub fn graph_watermark(&self) -> &str {
        &self.graph_watermark
    }

    #[must_use]
    pub const fn graph_commit_sequence(&self) -> u64 {
        self.graph_commit_sequence
    }
}

fn derive_intent_digest(
    project_id: &ProjectId,
    commit: &GitOidV1,
    target: &GitGraphEvidenceTarget,
) -> Result<ContentDigest, DomainError> {
    let (target_kind, target_id) = target.identity_parts();
    let digest = canonical_framed_sha256(
        b"tracedecay.git-graph-evidence-intent",
        &[
            project_id.as_str().as_bytes(),
            commit.as_str().as_bytes(),
            target_kind.as_bytes(),
            target_id.as_bytes(),
        ],
    );
    ContentDigest::new(format!("sha256:{digest}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_identity_is_stable_and_receipt_is_exact() {
        let intent = GitGraphEvidenceIntent::new(
            ProjectId::new("project.graph-evidence").unwrap(),
            GitOidV1::new("a".repeat(40)).unwrap(),
            GitGraphEvidenceTarget::Session(SessionId::new("session.graph-evidence").unwrap()),
        )
        .unwrap();
        assert_eq!(
            intent,
            GitGraphEvidenceIntent::new(
                intent.project_id().clone(),
                intent.commit().clone(),
                intent.target().clone(),
            )
            .unwrap()
        );
        let receipt = GitGraphEvidencePublicationReceipt::new(
            intent.intent_digest().clone(),
            "git-evidence:fixture".to_owned(),
            7,
        )
        .unwrap();
        receipt.validate().unwrap();
    }
}
