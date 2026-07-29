//! Payload-free bindings from retrieval anchors to immutable Git topology.

use serde::{Deserialize, Serialize};

use crate::code_intelligence::identity::CodeGenerationId;
use crate::feedback::{
    CiFailureGenerationEvidenceV1, CiFailureLocalizationResultV1, CiFailureRunIdentityV1,
    GitHubPullRequestIdV1, GitHubReviewCommentIdV1, GitHubReviewIdV1,
    GitHubReviewImmutableAnchorV1, GitHubReviewIngressResultV1, GitHubReviewItemV1,
    GitHubReviewThreadIdV1,
};
use crate::git::{
    GitIndexPreviewId, GitIndexPreviewV1, GitIndexReceiptId, GitIndexReceiptOutcomeV1,
    GitIndexTransactionId, GitIndexTransactionOperationV1, GitIndexTransactionReceiptV1,
    GitObjectFormatV1, GitOidV1, RepositoryIndexStateV1, RepositoryStateSnapshotId,
    RepositoryStateSnapshotV1, RepositoryWorkingTreeStateV1,
};
use crate::repository::GenerationBoundRepositoryProvenanceV1;

use super::canonical::canonical_sha256;
use super::error::DomainError;
use super::id::{
    CommitId, ManifestDigest, ProjectId, ProjectionGenerationId, ProviderId, RefId,
    RepositoryCaptureId, RepositoryId, RetrievalAnchorId, WorktreeId,
};
use super::retrieval::PrivacyDomainBoundLocatorDigest;
use super::time::UtcMicros;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "binding",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum GitTopologyGenerationRefV1 {
    RepositorySnapshot {
        generation_id: ProjectionGenerationId,
        capture_id: RepositoryCaptureId,
        snapshot_id: RepositoryStateSnapshotId,
        head_commit: Option<GitOidV1>,
    },
    ProviderCommit {
        source_anchor_id: RetrievalAnchorId,
        commit_id: CommitId,
    },
    CodeGeneration {
        generation_id: CodeGenerationId,
        retrieval_anchor_id: RetrievalAnchorId,
        commit_id: CommitId,
    },
    GitPreview {
        preview_id: GitIndexPreviewId,
        snapshot_id: RepositoryStateSnapshotId,
        head_commit: Option<GitOidV1>,
    },
    GitReceipt {
        receipt_id: GitIndexReceiptId,
        preview_id: GitIndexPreviewId,
        commit_id: Option<GitOidV1>,
    },
    #[serde(rename = "github_stack_capability")]
    GitHubStackCapability {
        generation_id: ProjectionGenerationId,
        source_anchor_id: RetrievalAnchorId,
        content_digest: ManifestDigest,
    },
    #[serde(rename = "github_stack_snapshot")]
    GitHubStackSnapshot {
        generation_id: ProjectionGenerationId,
        source_anchor_id: RetrievalAnchorId,
        content_digest: ManifestDigest,
        final_target_commit_id: CommitId,
    },
}

impl GitTopologyGenerationRefV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::RepositorySnapshot {
                generation_id,
                capture_id,
                snapshot_id,
                head_commit,
            } => {
                generation_id.validate()?;
                capture_id.validate()?;
                snapshot_id.validate()?;
                head_commit.as_ref().map_or(Ok(()), GitOidV1::validate)
            }
            Self::ProviderCommit {
                source_anchor_id,
                commit_id,
            } => {
                source_anchor_id.validate()?;
                commit_id.validate()
            }
            Self::CodeGeneration {
                generation_id,
                retrieval_anchor_id,
                commit_id,
            } => {
                generation_id.validate()?;
                retrieval_anchor_id.validate()?;
                commit_id.validate()
            }
            Self::GitPreview {
                preview_id,
                snapshot_id,
                head_commit,
            } => {
                preview_id.validate()?;
                snapshot_id.validate()?;
                head_commit.as_ref().map_or(Ok(()), GitOidV1::validate)
            }
            Self::GitReceipt {
                receipt_id,
                preview_id,
                commit_id,
            } => {
                receipt_id.validate()?;
                preview_id.validate()?;
                commit_id.as_ref().map_or(Ok(()), GitOidV1::validate)
            }
            Self::GitHubStackCapability {
                generation_id,
                source_anchor_id,
                content_digest,
            } => {
                generation_id.validate()?;
                source_anchor_id.validate()?;
                content_digest.validate()
            }
            Self::GitHubStackSnapshot {
                generation_id,
                source_anchor_id,
                content_digest,
                final_target_commit_id,
            } => {
                generation_id.validate()?;
                source_anchor_id.validate()?;
                content_digest.validate()?;
                final_target_commit_id.validate()
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitTopologySourceRoleV1 {
    PullRequestObservation,
    ReviewOriginal,
    ReviewAuthor,
    ReviewBody,
    ReviewSafeUrl,
    CiFailure,
    CiGeneration,
    CiSymbol,
    CiCaller,
    CiTest,
    CiRerunHint,
    Preflight,
    ApplyReceipt,
    Decision,
    RuntimeReceipt,
    #[serde(rename = "github_stack_capability")]
    GitHubStackCapability,
    #[serde(rename = "github_stack_snapshot")]
    GitHubStackSnapshot,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OrderedGitTopologySourceV1 {
    pub source_ordinal: u32,
    pub role: GitTopologySourceRoleV1,
    pub anchor_id: RetrievalAnchorId,
}

impl OrderedGitTopologySourceV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.anchor_id.validate()
    }
}

fn validate_ordered_sources(sources: &[OrderedGitTopologySourceV1]) -> Result<(), DomainError> {
    for (index, source) in sources.iter().enumerate() {
        source.validate()?;
        if usize::try_from(source.source_ordinal).ok() != Some(index) {
            return Err(DomainError::NonCanonical {
                field: "git topology source ordinal",
            });
        }
    }
    Ok(())
}

fn push_source(
    sources: &mut Vec<OrderedGitTopologySourceV1>,
    role: GitTopologySourceRoleV1,
    anchor_id: RetrievalAnchorId,
) -> Result<(), DomainError> {
    let source_ordinal = u32::try_from(sources.len()).map_err(|_| DomainError::NonCanonical {
        field: "git topology source count",
    })?;
    sources.push(OrderedGitTopologySourceV1 {
        source_ordinal,
        role,
        anchor_id,
    });
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepositoryCaptureAnchorRefV1 {
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub worktree_id: Option<WorktreeId>,
    pub generation_id: ProjectionGenerationId,
    pub capture_id: RepositoryCaptureId,
    pub snapshot_id: RepositoryStateSnapshotId,
    pub snapshot_digest: ManifestDigest,
    pub object_format: GitObjectFormatV1,
    pub head_commit: Option<GitOidV1>,
}

impl RepositoryCaptureAnchorRefV1 {
    pub fn new(
        provenance: &GenerationBoundRepositoryProvenanceV1,
        snapshot: &RepositoryStateSnapshotV1,
    ) -> Result<Self, DomainError> {
        provenance.validate()?;
        snapshot.validate()?;
        let snapshot_digest = GitIndexPreviewV1::repository_snapshot_digest(snapshot)?;
        let value = Self {
            project_id: snapshot.project_id.clone(),
            repository_id: snapshot.repository_id.clone(),
            worktree_id: snapshot.worktree_id.clone(),
            generation_id: provenance.generation_id().clone(),
            capture_id: provenance.capture_id().clone(),
            snapshot_id: snapshot.snapshot_id.clone(),
            snapshot_digest,
            object_format: snapshot.object_format,
            head_commit: snapshot.head.commit().cloned(),
        };
        if provenance.capture().project_id() != Some(&value.project_id)
            || provenance.capture().repository_id() != &value.repository_id
            || provenance.capture().worktree_id() != value.worktree_id.as_ref()
        {
            return Err(DomainError::SnapshotMismatch {
                field: "repository capture topology binding",
            });
        }
        value.validate()?;
        Ok(value)
    }

    pub fn generation(&self) -> GitTopologyGenerationRefV1 {
        GitTopologyGenerationRefV1::RepositorySnapshot {
            generation_id: self.generation_id.clone(),
            capture_id: self.capture_id.clone(),
            snapshot_id: self.snapshot_id.clone(),
            head_commit: self.head_commit.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.project_id.validate()?;
        self.repository_id.validate()?;
        self.worktree_id
            .as_ref()
            .map_or(Ok(()), WorktreeId::validate)?;
        self.generation_id.validate()?;
        self.capture_id.validate()?;
        self.snapshot_id.validate()?;
        self.snapshot_digest.validate()?;
        if let Some(head) = &self.head_commit {
            head.validate()?;
            if head.format() != self.object_format {
                return Err(DomainError::NonCanonical {
                    field: "repository capture head object format",
                });
            }
        }
        self.generation().validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorktreeCaptureAnchorRefV1 {
    pub repository: RepositoryCaptureAnchorRefV1,
    pub worktree_id: WorktreeId,
}

impl WorktreeCaptureAnchorRefV1 {
    pub fn new(repository: RepositoryCaptureAnchorRefV1) -> Result<Self, DomainError> {
        let worktree_id = repository
            .worktree_id
            .clone()
            .ok_or(DomainError::UnknownReference {
                field: "worktree capture identity",
            })?;
        let value = Self {
            repository,
            worktree_id,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.repository.validate()?;
        self.worktree_id.validate()?;
        if self.repository.worktree_id.as_ref() != Some(&self.worktree_id) {
            return Err(DomainError::SnapshotMismatch {
                field: "worktree capture identity",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeGitObjectKindV1 {
    Commit,
    Tree,
    Blob,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeGitObjectAnchorRefV1 {
    pub repository: RepositoryCaptureAnchorRefV1,
    pub object_kind: NativeGitObjectKindV1,
    pub object_id: GitOidV1,
}

impl NativeGitObjectAnchorRefV1 {
    pub fn new(
        repository: RepositoryCaptureAnchorRefV1,
        object_kind: NativeGitObjectKindV1,
        object_id: GitOidV1,
    ) -> Result<Self, DomainError> {
        let value = Self {
            repository,
            object_kind,
            object_id,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.repository.validate()?;
        self.object_id.validate()?;
        if self.object_id.format() != self.repository.object_format {
            return Err(DomainError::NonCanonical {
                field: "native git object format",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RefSnapshotKindV1 {
    Direct,
    Symbolic,
    UnbornSymbolic,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RefSnapshotAnchorRefV1 {
    pub repository: RepositoryCaptureAnchorRefV1,
    pub ref_id: RefId,
    pub ref_kind: RefSnapshotKindV1,
    pub target_object: Option<NativeGitObjectAnchorRefV1>,
    pub ref_snapshot_digest: ManifestDigest,
}

impl RefSnapshotAnchorRefV1 {
    pub fn new(
        repository: RepositoryCaptureAnchorRefV1,
        ref_id: RefId,
        ref_kind: RefSnapshotKindV1,
        target_object: Option<NativeGitObjectAnchorRefV1>,
        ref_snapshot_digest: ManifestDigest,
    ) -> Result<Self, DomainError> {
        let value = Self {
            repository,
            ref_id,
            ref_kind,
            target_object,
            ref_snapshot_digest,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.repository.validate()?;
        self.ref_id.validate()?;
        self.ref_snapshot_digest.validate()?;
        match (self.ref_kind, &self.target_object) {
            (RefSnapshotKindV1::UnbornSymbolic, None) => Ok(()),
            (RefSnapshotKindV1::Direct | RefSnapshotKindV1::Symbolic, Some(target)) => {
                target.validate()?;
                if target.repository != self.repository {
                    return Err(DomainError::SnapshotMismatch {
                        field: "ref snapshot repository capture",
                    });
                }
                Ok(())
            }
            _ => Err(DomainError::NonCanonical {
                field: "ref snapshot target object",
            }),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PullRequestSnapshotAnchorRefV1 {
    pub provider: ProviderId,
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub worktree_id: WorktreeId,
    pub pull_request_id: GitHubPullRequestIdV1,
    pub base_commit_id: CommitId,
    pub head_commit_id: CommitId,
    pub merge_base_commit_id: CommitId,
    pub source_anchor_id: RetrievalAnchorId,
    pub snapshot_digest: ManifestDigest,
    pub sources: Vec<OrderedGitTopologySourceV1>,
}

impl PullRequestSnapshotAnchorRefV1 {
    pub fn from_ingress(
        result: &GitHubReviewIngressResultV1,
        source_anchor_id: RetrievalAnchorId,
    ) -> Result<Self, DomainError> {
        result.validate()?;
        let mut sources = Vec::new();
        push_source(
            &mut sources,
            GitTopologySourceRoleV1::PullRequestObservation,
            source_anchor_id.clone(),
        )?;
        let value = Self {
            provider: result.provider.clone(),
            project_id: result.scope.project_id.clone(),
            repository_id: result.scope.repository_id.clone(),
            worktree_id: result.scope.worktree_id.clone(),
            pull_request_id: result.pull_request_id.clone(),
            base_commit_id: result.provider_base_commit_id.clone(),
            head_commit_id: result.provider_head_commit_id.clone(),
            merge_base_commit_id: result.merge_base_commit_id.clone(),
            source_anchor_id,
            snapshot_digest: canonical_sha256(result)?,
            sources,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn generation(&self) -> GitTopologyGenerationRefV1 {
        GitTopologyGenerationRefV1::ProviderCommit {
            source_anchor_id: self.source_anchor_id.clone(),
            commit_id: self.head_commit_id.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.provider.validate()?;
        self.project_id.validate()?;
        self.repository_id.validate()?;
        self.worktree_id.validate()?;
        self.pull_request_id.validate()?;
        self.base_commit_id.validate()?;
        self.head_commit_id.validate()?;
        self.merge_base_commit_id.validate()?;
        self.source_anchor_id.validate()?;
        self.snapshot_digest.validate()?;
        validate_ordered_sources(&self.sources)?;
        if self.sources.len() != 1
            || self.sources[0].role != GitTopologySourceRoleV1::PullRequestObservation
            || self.sources[0].anchor_id != self.source_anchor_id
        {
            return Err(DomainError::NonCanonical {
                field: "pull request snapshot source",
            });
        }
        self.generation().validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewSnapshotAnchorRefV1 {
    pub pull_request: PullRequestSnapshotAnchorRefV1,
    pub review_id: Option<GitHubReviewIdV1>,
    pub thread_id: Option<GitHubReviewThreadIdV1>,
    pub comment_id: GitHubReviewCommentIdV1,
    pub reply_to_comment_id: Option<GitHubReviewCommentIdV1>,
    pub original: GitHubReviewImmutableAnchorV1,
    pub item_digest: ManifestDigest,
    pub sources: Vec<OrderedGitTopologySourceV1>,
}

impl ReviewSnapshotAnchorRefV1 {
    pub fn from_item(
        pull_request: PullRequestSnapshotAnchorRefV1,
        item: &GitHubReviewItemV1,
    ) -> Result<Self, DomainError> {
        item.validate()?;
        let mut sources = Vec::new();
        push_source(
            &mut sources,
            GitTopologySourceRoleV1::PullRequestObservation,
            pull_request.source_anchor_id.clone(),
        )?;
        push_source(
            &mut sources,
            GitTopologySourceRoleV1::ReviewOriginal,
            item.remap.original.retrieval_anchor_id.clone(),
        )?;
        push_source(
            &mut sources,
            GitTopologySourceRoleV1::ReviewAuthor,
            item.author_anchor.clone(),
        )?;
        push_source(
            &mut sources,
            GitTopologySourceRoleV1::ReviewBody,
            item.body_anchor.clone(),
        )?;
        if let Some(anchor_id) = &item.safe_url_anchor {
            push_source(
                &mut sources,
                GitTopologySourceRoleV1::ReviewSafeUrl,
                anchor_id.clone(),
            )?;
        }
        let value = Self {
            pull_request,
            review_id: item.review_id.clone(),
            thread_id: item.thread_id.clone(),
            comment_id: item.comment_id.clone(),
            reply_to_comment_id: item.reply_to_comment_id.clone(),
            original: item.remap.original.clone(),
            item_digest: canonical_sha256(item)?,
            sources,
        };
        if item.repository_id != value.pull_request.repository_id
            || item.pull_request_id != value.pull_request.pull_request_id
        {
            return Err(DomainError::SnapshotMismatch {
                field: "review pull request snapshot",
            });
        }
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.pull_request.validate()?;
        self.review_id
            .as_ref()
            .map_or(Ok(()), GitHubReviewIdV1::validate)?;
        self.thread_id
            .as_ref()
            .map_or(Ok(()), GitHubReviewThreadIdV1::validate)?;
        self.comment_id.validate()?;
        self.reply_to_comment_id
            .as_ref()
            .map_or(Ok(()), GitHubReviewCommentIdV1::validate)?;
        self.original.validate()?;
        self.item_digest.validate()?;
        validate_ordered_sources(&self.sources)?;
        if self.original.repository_id != self.pull_request.repository_id {
            return Err(DomainError::SnapshotMismatch {
                field: "review immutable repository linkage",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckSnapshotAnchorRefV1 {
    pub provider: ProviderId,
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub worktree_id: WorktreeId,
    pub run: CiFailureRunIdentityV1,
    pub head_commit_id: CommitId,
    pub generation: Option<CiFailureGenerationEvidenceV1>,
    pub result_digest: ManifestDigest,
    pub sources: Vec<OrderedGitTopologySourceV1>,
}

impl CheckSnapshotAnchorRefV1 {
    pub fn from_localization(result: &CiFailureLocalizationResultV1) -> Result<Self, DomainError> {
        result.validate()?;
        let mut sources = Vec::new();
        push_source(
            &mut sources,
            GitTopologySourceRoleV1::CiFailure,
            result.failure_anchor.clone(),
        )?;
        if let Some(generation) = &result.generation {
            push_source(
                &mut sources,
                GitTopologySourceRoleV1::CiGeneration,
                generation.retrieval_anchor_id.clone(),
            )?;
        }
        if let Some(symbol) = &result.symbol {
            push_source(
                &mut sources,
                GitTopologySourceRoleV1::CiSymbol,
                symbol.retrieval_anchor_id.clone(),
            )?;
        }
        for caller in &result.callers {
            push_source(
                &mut sources,
                GitTopologySourceRoleV1::CiCaller,
                caller.retrieval_anchor_id.clone(),
            )?;
        }
        for test in &result.tests {
            push_source(
                &mut sources,
                GitTopologySourceRoleV1::CiTest,
                test.retrieval_anchor_id.clone(),
            )?;
        }
        for hint in &result.rerun_hints {
            if let Some(anchor_id) = &hint.retrieval_anchor_id {
                push_source(
                    &mut sources,
                    GitTopologySourceRoleV1::CiRerunHint,
                    anchor_id.clone(),
                )?;
            }
        }
        let value = Self {
            provider: result.provider.clone(),
            project_id: result.branch.scope.project_id.clone(),
            repository_id: result.branch.scope.repository_id.clone(),
            worktree_id: result.branch.scope.worktree_id.clone(),
            run: result.run.clone(),
            head_commit_id: result.branch.provider_head_commit_id.clone(),
            generation: result.generation.clone(),
            result_digest: canonical_sha256(result)?,
            sources,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn generation_ref(&self) -> GitTopologyGenerationRefV1 {
        match &self.generation {
            Some(generation) => GitTopologyGenerationRefV1::CodeGeneration {
                generation_id: generation.generation_id.clone(),
                retrieval_anchor_id: generation.retrieval_anchor_id.clone(),
                commit_id: self.head_commit_id.clone(),
            },
            None => GitTopologyGenerationRefV1::ProviderCommit {
                source_anchor_id: self.sources[0].anchor_id.clone(),
                commit_id: self.head_commit_id.clone(),
            },
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.provider.validate()?;
        self.project_id.validate()?;
        self.repository_id.validate()?;
        self.worktree_id.validate()?;
        self.run.validate()?;
        self.head_commit_id.validate()?;
        self.generation
            .as_ref()
            .map_or(Ok(()), CiFailureGenerationEvidenceV1::validate)?;
        self.result_digest.validate()?;
        validate_ordered_sources(&self.sources)?;
        if self.sources.first().map(|source| source.role)
            != Some(GitTopologySourceRoleV1::CiFailure)
        {
            return Err(DomainError::NonCanonical {
                field: "check failure source",
            });
        }
        self.generation_ref().validate()
    }
}

/// Exact read-only provider capability observation for GitHub stacked pull
/// requests. The content digest is recomputed from every identity-bearing
/// field; mutable provider permissions or ambient configuration cannot be
/// substituted during resolution.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubStackCapabilitySnapshotV1 {
    pub provider: ProviderId,
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub worktree_id: WorktreeId,
    pub state: GitHubStackCapabilityStateV1,
    pub generation_id: ProjectionGenerationId,
    pub source_anchor_id: RetrievalAnchorId,
    pub content_digest: ManifestDigest,
    pub sources: Vec<OrderedGitTopologySourceV1>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHubStackCapabilityStateV1 {
    Unavailable,
    PrivatePreviewDisabled,
    Enabled,
    Degraded,
}

impl GitHubStackCapabilitySnapshotV1 {
    pub fn new(
        provider: ProviderId,
        project_id: ProjectId,
        repository_id: RepositoryId,
        worktree_id: WorktreeId,
        state: GitHubStackCapabilityStateV1,
        generation_id: ProjectionGenerationId,
        source_anchor_id: RetrievalAnchorId,
    ) -> Result<Self, DomainError> {
        let content_digest = canonical_sha256(&(
            "tracedecay.github-stack.capability.v1",
            &provider,
            &project_id,
            &repository_id,
            &worktree_id,
            state,
            &generation_id,
            &source_anchor_id,
        ))?;
        let mut sources = Vec::new();
        push_source(
            &mut sources,
            GitTopologySourceRoleV1::GitHubStackCapability,
            source_anchor_id.clone(),
        )?;
        let value = Self {
            provider,
            project_id,
            repository_id,
            worktree_id,
            state,
            generation_id,
            source_anchor_id,
            content_digest,
            sources,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn generation(&self) -> GitTopologyGenerationRefV1 {
        GitTopologyGenerationRefV1::GitHubStackCapability {
            generation_id: self.generation_id.clone(),
            source_anchor_id: self.source_anchor_id.clone(),
            content_digest: self.content_digest.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.provider.validate()?;
        self.project_id.validate()?;
        self.repository_id.validate()?;
        self.worktree_id.validate()?;
        self.generation_id.validate()?;
        self.source_anchor_id.validate()?;
        self.content_digest.validate()?;
        validate_ordered_sources(&self.sources)?;
        if self.sources.len() != 1
            || self.sources[0].role != GitTopologySourceRoleV1::GitHubStackCapability
            || self.sources[0].anchor_id != self.source_anchor_id
        {
            return Err(DomainError::NonCanonical {
                field: "GitHub stack capability source",
            });
        }
        let expected = canonical_sha256(&(
            "tracedecay.github-stack.capability.v1",
            &self.provider,
            &self.project_id,
            &self.repository_id,
            &self.worktree_id,
            self.state,
            &self.generation_id,
            &self.source_anchor_id,
        ))?;
        if expected != self.content_digest {
            return Err(DomainError::DigestMismatch);
        }
        self.generation().validate()
    }
}

/// One immutable layer in a provider-proven strictly linear GitHub stack.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubStackLayerSnapshotV1 {
    pub provider_position: u32,
    pub pull_request: PullRequestSnapshotAnchorRefV1,
    pub base_ref_id: RefId,
    pub head_ref_id: RefId,
    pub protection_digest: ManifestDigest,
    pub ci_digest: ManifestDigest,
    pub merge_queue_digest: ManifestDigest,
}

impl GitHubStackLayerSnapshotV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.pull_request.validate()?;
        self.base_ref_id.validate()?;
        self.head_ref_id.validate()?;
        self.protection_digest.validate()?;
        self.ci_digest.validate()?;
        self.merge_queue_digest.validate()
    }
}

/// Exact, payload-free Plan 37 GitHub stack observation. A snapshot exists
/// only for an enabled capability and retains the complete linear topology.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubStackSnapshotV1 {
    pub capability: GitHubStackCapabilitySnapshotV1,
    pub provider_stack_id_digest: PrivacyDomainBoundLocatorDigest,
    pub generation_id: ProjectionGenerationId,
    pub final_target_ref_id: RefId,
    pub final_target_commit_id: CommitId,
    pub layers: Vec<GitHubStackLayerSnapshotV1>,
    pub source_anchor_id: RetrievalAnchorId,
    pub content_digest: ManifestDigest,
    pub sources: Vec<OrderedGitTopologySourceV1>,
}

impl GitHubStackSnapshotV1 {
    pub fn new(
        capability: GitHubStackCapabilitySnapshotV1,
        provider_stack_id_digest: PrivacyDomainBoundLocatorDigest,
        generation_id: ProjectionGenerationId,
        final_target_ref_id: RefId,
        final_target_commit_id: CommitId,
        layers: Vec<GitHubStackLayerSnapshotV1>,
        source_anchor_id: RetrievalAnchorId,
    ) -> Result<Self, DomainError> {
        let content_digest = canonical_sha256(&(
            "tracedecay.github-stack.snapshot.v1",
            &capability,
            &provider_stack_id_digest,
            &generation_id,
            &final_target_ref_id,
            &final_target_commit_id,
            &layers,
            &source_anchor_id,
        ))?;
        let mut sources = Vec::new();
        push_source(
            &mut sources,
            GitTopologySourceRoleV1::GitHubStackCapability,
            capability.source_anchor_id.clone(),
        )?;
        push_source(
            &mut sources,
            GitTopologySourceRoleV1::GitHubStackSnapshot,
            source_anchor_id.clone(),
        )?;
        for layer in &layers {
            push_source(
                &mut sources,
                GitTopologySourceRoleV1::PullRequestObservation,
                layer.pull_request.source_anchor_id.clone(),
            )?;
        }
        let value = Self {
            capability,
            provider_stack_id_digest,
            generation_id,
            final_target_ref_id,
            final_target_commit_id,
            layers,
            source_anchor_id,
            content_digest,
            sources,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn generation(&self) -> GitTopologyGenerationRefV1 {
        GitTopologyGenerationRefV1::GitHubStackSnapshot {
            generation_id: self.generation_id.clone(),
            source_anchor_id: self.source_anchor_id.clone(),
            content_digest: self.content_digest.clone(),
            final_target_commit_id: self.final_target_commit_id.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.capability.validate()?;
        if self.capability.state != GitHubStackCapabilityStateV1::Enabled {
            return Err(DomainError::NonCanonical {
                field: "GitHub stack snapshot capability",
            });
        }
        self.provider_stack_id_digest.validate()?;
        self.generation_id.validate()?;
        self.final_target_ref_id.validate()?;
        self.final_target_commit_id.validate()?;
        self.source_anchor_id.validate()?;
        self.content_digest.validate()?;
        validate_ordered_sources(&self.sources)?;
        let first = self.layers.first().ok_or(DomainError::NonCanonical {
            field: "GitHub stack layers",
        })?;
        if first.base_ref_id != self.final_target_ref_id
            || first.pull_request.base_commit_id != self.final_target_commit_id
        {
            return Err(DomainError::SnapshotMismatch {
                field: "GitHub stack final target",
            });
        }
        for (index, layer) in self.layers.iter().enumerate() {
            layer.validate()?;
            if usize::try_from(layer.provider_position).ok() != Some(index)
                || layer.pull_request.provider != self.capability.provider
                || layer.pull_request.project_id != self.capability.project_id
                || layer.pull_request.repository_id != self.capability.repository_id
                || layer.pull_request.worktree_id != self.capability.worktree_id
            {
                return Err(DomainError::SnapshotMismatch {
                    field: "GitHub stack layer authority",
                });
            }
            if let Some(previous) = index
                .checked_sub(1)
                .and_then(|prior| self.layers.get(prior))
                && (layer.base_ref_id != previous.head_ref_id
                    || layer.pull_request.base_commit_id != previous.pull_request.head_commit_id)
            {
                return Err(DomainError::SnapshotMismatch {
                    field: "GitHub stack linear topology",
                });
            }
        }
        let mut expected_sources = Vec::new();
        push_source(
            &mut expected_sources,
            GitTopologySourceRoleV1::GitHubStackCapability,
            self.capability.source_anchor_id.clone(),
        )?;
        push_source(
            &mut expected_sources,
            GitTopologySourceRoleV1::GitHubStackSnapshot,
            self.source_anchor_id.clone(),
        )?;
        for layer in &self.layers {
            push_source(
                &mut expected_sources,
                GitTopologySourceRoleV1::PullRequestObservation,
                layer.pull_request.source_anchor_id.clone(),
            )?;
        }
        if self.sources != expected_sources {
            return Err(DomainError::NonCanonical {
                field: "GitHub stack snapshot sources",
            });
        }
        let expected = canonical_sha256(&(
            "tracedecay.github-stack.snapshot.v1",
            &self.capability,
            &self.provider_stack_id_digest,
            &self.generation_id,
            &self.final_target_ref_id,
            &self.final_target_commit_id,
            &self.layers,
            &self.source_anchor_id,
        ))?;
        if expected != self.content_digest {
            return Err(DomainError::DigestMismatch);
        }
        self.generation().validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConflictEvidenceAnchorRefV1 {
    pub repository: RepositoryCaptureAnchorRefV1,
    pub index_checksum: ManifestDigest,
    pub unmerged_stage_digest: Option<ManifestDigest>,
    pub conflict_digest: ManifestDigest,
}

impl ConflictEvidenceAnchorRefV1 {
    pub fn new(
        repository: RepositoryCaptureAnchorRefV1,
        snapshot: &RepositoryStateSnapshotV1,
    ) -> Result<Self, DomainError> {
        snapshot.validate()?;
        let value = Self {
            repository,
            index_checksum: snapshot.index.checksum.clone(),
            unmerged_stage_digest: snapshot.index.unmerged_stage_digest.clone(),
            conflict_digest: canonical_sha256(snapshot)?,
        };
        if value.repository.snapshot_id != snapshot.snapshot_id {
            return Err(DomainError::SnapshotMismatch {
                field: "conflict repository snapshot",
            });
        }
        if snapshot.index.state != RepositoryIndexStateV1::Unmerged
            && snapshot.working_tree.state != RepositoryWorkingTreeStateV1::Conflicted
        {
            return Err(DomainError::NonCanonical {
                field: "conflict evidence state",
            });
        }
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.repository.validate()?;
        self.index_checksum.validate()?;
        self.unmerged_stage_digest
            .as_ref()
            .map_or(Ok(()), ManifestDigest::validate)?;
        self.conflict_digest.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreflightPreviewAnchorRefV1 {
    pub repository: RepositoryCaptureAnchorRefV1,
    pub preview_id: GitIndexPreviewId,
    pub preview_digest: ManifestDigest,
    pub operation: GitIndexTransactionOperationV1,
    pub candidate_index_tree: Option<GitOidV1>,
    pub commit_intent_digest: Option<ManifestDigest>,
    pub expires_at: UtcMicros,
}

impl PreflightPreviewAnchorRefV1 {
    pub fn new(
        repository: RepositoryCaptureAnchorRefV1,
        preview: &GitIndexPreviewV1,
    ) -> Result<Self, DomainError> {
        preview.validate()?;
        let value = Self {
            repository,
            preview_id: preview.preview_id.clone(),
            preview_digest: preview.preview_digest.clone(),
            operation: preview.operation,
            candidate_index_tree: preview.candidate_index_tree.clone(),
            commit_intent_digest: preview.commit_intent_digest.clone(),
            expires_at: preview.expires_at,
        };
        if value.repository.snapshot_id != preview.repository_snapshot.snapshot_id
            || value.repository.snapshot_digest != preview.repository_snapshot_digest
        {
            return Err(DomainError::SnapshotMismatch {
                field: "preflight repository snapshot",
            });
        }
        value.validate()?;
        Ok(value)
    }

    pub fn generation(&self) -> GitTopologyGenerationRefV1 {
        GitTopologyGenerationRefV1::GitPreview {
            preview_id: self.preview_id.clone(),
            snapshot_id: self.repository.snapshot_id.clone(),
            head_commit: self.repository.head_commit.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.repository.validate()?;
        self.preview_id.validate()?;
        self.preview_digest.validate()?;
        if let Some(tree) = &self.candidate_index_tree {
            tree.validate()?;
            if tree.format() != self.repository.object_format {
                return Err(DomainError::NonCanonical {
                    field: "preflight candidate tree format",
                });
            }
        }
        self.commit_intent_digest
            .as_ref()
            .map_or(Ok(()), ManifestDigest::validate)?;
        self.generation().validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplyReceiptAnchorRefV1 {
    pub preflight: PreflightPreviewAnchorRefV1,
    pub receipt_id: GitIndexReceiptId,
    pub transaction_id: GitIndexTransactionId,
    pub receipt_digest: ManifestDigest,
    pub outcome: GitIndexReceiptOutcomeV1,
    pub final_snapshot_digest: ManifestDigest,
    pub final_snapshot_captured: bool,
    pub created_commit: Option<GitOidV1>,
    pub sources: Vec<OrderedGitTopologySourceV1>,
}

impl ApplyReceiptAnchorRefV1 {
    pub fn new(
        preflight: PreflightPreviewAnchorRefV1,
        preflight_anchor_id: RetrievalAnchorId,
        receipt: &GitIndexTransactionReceiptV1,
    ) -> Result<Self, DomainError> {
        receipt.validate()?;
        let mut sources = Vec::new();
        push_source(
            &mut sources,
            GitTopologySourceRoleV1::Preflight,
            preflight_anchor_id,
        )?;
        let value = Self {
            preflight,
            receipt_id: receipt.receipt_id.clone(),
            transaction_id: receipt.transaction_id.clone(),
            receipt_digest: receipt.receipt_digest.clone(),
            outcome: receipt.outcome,
            final_snapshot_digest: receipt.final_snapshot_digest.clone(),
            final_snapshot_captured: receipt.final_snapshot_captured,
            created_commit: receipt.created_commit.clone(),
            sources,
        };
        if value.preflight.preview_id != receipt.preview_id
            || value.preflight.repository.snapshot_digest != receipt.old_snapshot_digest
            || value.preflight.operation != receipt.operation
        {
            return Err(DomainError::SnapshotMismatch {
                field: "apply receipt preflight binding",
            });
        }
        value.validate()?;
        Ok(value)
    }

    pub fn generation(&self) -> GitTopologyGenerationRefV1 {
        GitTopologyGenerationRefV1::GitReceipt {
            receipt_id: self.receipt_id.clone(),
            preview_id: self.preflight.preview_id.clone(),
            commit_id: self.created_commit.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.preflight.validate()?;
        self.receipt_id.validate()?;
        self.transaction_id.validate()?;
        self.receipt_digest.validate()?;
        self.final_snapshot_digest.validate()?;
        self.created_commit
            .as_ref()
            .map_or(Ok(()), GitOidV1::validate)?;
        validate_ordered_sources(&self.sources)?;
        if self.sources.len() != 1 || self.sources[0].role != GitTopologySourceRoleV1::Preflight {
            return Err(DomainError::NonCanonical {
                field: "apply receipt preflight source",
            });
        }
        if self.outcome == GitIndexReceiptOutcomeV1::Committed
            && (!self.final_snapshot_captured || self.created_commit.is_none())
            && self.preflight.operation == GitIndexTransactionOperationV1::CommitIndex
        {
            return Err(DomainError::NonCanonical {
                field: "commit apply receipt",
            });
        }
        self.generation().validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IntegrationReceiptAnchorRefV1 {
    pub apply: ApplyReceiptAnchorRefV1,
    pub sources: Vec<OrderedGitTopologySourceV1>,
    pub integration_digest: ManifestDigest,
}

impl IntegrationReceiptAnchorRefV1 {
    pub fn new(
        apply: ApplyReceiptAnchorRefV1,
        additional_sources: Vec<(GitTopologySourceRoleV1, RetrievalAnchorId)>,
    ) -> Result<Self, DomainError> {
        apply.validate()?;
        let mut sources = apply.sources.clone();
        for (role, anchor_id) in additional_sources {
            push_source(&mut sources, role, anchor_id)?;
        }
        let integration_digest =
            canonical_sha256(&("tracedecay.git-topology.integration.v1", &apply, &sources))?;
        let value = Self {
            apply,
            sources,
            integration_digest,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.apply.validate()?;
        validate_ordered_sources(&self.sources)?;
        self.integration_digest.validate()?;
        if !self.sources.starts_with(&self.apply.sources) {
            return Err(DomainError::NonCanonical {
                field: "integration receipt apply source",
            });
        }
        let expected = canonical_sha256(&(
            "tracedecay.git-topology.integration.v1",
            &self.apply,
            &self.sources,
        ))?;
        if expected != self.integration_digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "target",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum GitTopologyAnchorTargetV1 {
    RepositoryCapture(RepositoryCaptureAnchorRefV1),
    WorktreeCapture(WorktreeCaptureAnchorRefV1),
    RefSnapshot(RefSnapshotAnchorRefV1),
    NativeObject(NativeGitObjectAnchorRefV1),
    PullRequestSnapshot(PullRequestSnapshotAnchorRefV1),
    ReviewSnapshot(ReviewSnapshotAnchorRefV1),
    CheckSnapshot(CheckSnapshotAnchorRefV1),
    #[serde(rename = "github_stack_capability")]
    GitHubStackCapability(GitHubStackCapabilitySnapshotV1),
    #[serde(rename = "github_stack_snapshot")]
    GitHubStackSnapshot(GitHubStackSnapshotV1),
    ConflictEvidence(ConflictEvidenceAnchorRefV1),
    PreflightPreview(PreflightPreviewAnchorRefV1),
    ApplyReceipt(ApplyReceiptAnchorRefV1),
    IntegrationReceipt(IntegrationReceiptAnchorRefV1),
}

impl GitTopologyAnchorTargetV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::RepositoryCapture(value) => value.validate(),
            Self::WorktreeCapture(value) => value.validate(),
            Self::RefSnapshot(value) => value.validate(),
            Self::NativeObject(value) => value.validate(),
            Self::PullRequestSnapshot(value) => value.validate(),
            Self::ReviewSnapshot(value) => value.validate(),
            Self::CheckSnapshot(value) => value.validate(),
            Self::GitHubStackCapability(value) => value.validate(),
            Self::GitHubStackSnapshot(value) => value.validate(),
            Self::ConflictEvidence(value) => value.validate(),
            Self::PreflightPreview(value) => value.validate(),
            Self::ApplyReceipt(value) => value.validate(),
            Self::IntegrationReceipt(value) => value.validate(),
        }
    }

    pub fn project_id(&self) -> &ProjectId {
        match self {
            Self::RepositoryCapture(value) => &value.project_id,
            Self::WorktreeCapture(value) => &value.repository.project_id,
            Self::RefSnapshot(value) => &value.repository.project_id,
            Self::NativeObject(value) => &value.repository.project_id,
            Self::PullRequestSnapshot(value) => &value.project_id,
            Self::ReviewSnapshot(value) => &value.pull_request.project_id,
            Self::CheckSnapshot(value) => &value.project_id,
            Self::GitHubStackCapability(value) => &value.project_id,
            Self::GitHubStackSnapshot(value) => &value.capability.project_id,
            Self::ConflictEvidence(value) => &value.repository.project_id,
            Self::PreflightPreview(value) => &value.repository.project_id,
            Self::ApplyReceipt(value) => &value.preflight.repository.project_id,
            Self::IntegrationReceipt(value) => &value.apply.preflight.repository.project_id,
        }
    }

    pub fn repository_id(&self) -> &RepositoryId {
        match self {
            Self::RepositoryCapture(value) => &value.repository_id,
            Self::WorktreeCapture(value) => &value.repository.repository_id,
            Self::RefSnapshot(value) => &value.repository.repository_id,
            Self::NativeObject(value) => &value.repository.repository_id,
            Self::PullRequestSnapshot(value) => &value.repository_id,
            Self::ReviewSnapshot(value) => &value.pull_request.repository_id,
            Self::CheckSnapshot(value) => &value.repository_id,
            Self::GitHubStackCapability(value) => &value.repository_id,
            Self::GitHubStackSnapshot(value) => &value.capability.repository_id,
            Self::ConflictEvidence(value) => &value.repository.repository_id,
            Self::PreflightPreview(value) => &value.repository.repository_id,
            Self::ApplyReceipt(value) => &value.preflight.repository.repository_id,
            Self::IntegrationReceipt(value) => &value.apply.preflight.repository.repository_id,
        }
    }

    pub fn generation(&self) -> GitTopologyGenerationRefV1 {
        match self {
            Self::RepositoryCapture(value) => value.generation(),
            Self::WorktreeCapture(value) => value.repository.generation(),
            Self::RefSnapshot(value) => value.repository.generation(),
            Self::NativeObject(value) => value.repository.generation(),
            Self::PullRequestSnapshot(value) => value.generation(),
            Self::ReviewSnapshot(value) => value.pull_request.generation(),
            Self::CheckSnapshot(value) => value.generation_ref(),
            Self::GitHubStackCapability(value) => value.generation(),
            Self::GitHubStackSnapshot(value) => value.generation(),
            Self::ConflictEvidence(value) => value.repository.generation(),
            Self::PreflightPreview(value) => value.generation(),
            Self::ApplyReceipt(value) => value.generation(),
            Self::IntegrationReceipt(value) => value.apply.generation(),
        }
    }

    pub fn ordered_sources(&self) -> &[OrderedGitTopologySourceV1] {
        match self {
            Self::ReviewSnapshot(value) => &value.sources,
            Self::PullRequestSnapshot(value) => &value.sources,
            Self::CheckSnapshot(value) => &value.sources,
            Self::GitHubStackCapability(value) => &value.sources,
            Self::GitHubStackSnapshot(value) => &value.sources,
            Self::ApplyReceipt(value) => &value.sources,
            Self::IntegrationReceipt(value) => &value.sources,
            _ => &[],
        }
    }
}
