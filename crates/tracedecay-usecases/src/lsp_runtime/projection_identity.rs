use std::path::PathBuf;

use tracedecay_application::ResolvedScope;
use tracedecay_domain::{
    CodeGenerationId, CommitId, ContentDigest, ManifestDigest, ProjectId, RefId, RepositoryId,
    WorktreeId,
};
use tracedecay_lsp::{LspRuntimeFailure, LspRuntimeFuture};

use super::LspFeedbackProjectionScope;

/// Exact immutable code-index identity resolved by the daemon-owned mounted
/// worktree scheduler. No mutable graph or path-derived value can satisfy this
/// authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspCodeIndexProjectionIdentity {
    pub project: ProjectId,
    pub repository: RepositoryId,
    pub worktree: Option<WorktreeId>,
    pub reference: Option<RefId>,
    pub source_revision: Option<CommitId>,
    pub code_generation_id: CodeGenerationId,
    pub snapshot_digest: ManifestDigest,
    pub invalidation_digest: ManifestDigest,
    pub snapshot_content_digest: ContentDigest,
    pub document_content_digest: Option<ContentDigest>,
}

impl LspCodeIndexProjectionIdentity {
    pub fn admit_for_scope(
        self,
        scope: &ResolvedScope,
    ) -> Result<LspFeedbackProjectionScope, LspRuntimeFailure> {
        scope
            .validate()
            .map_err(|_| LspRuntimeFailure::new("registered-project-scope-invalid"))?;
        if self.project != scope.project_id {
            return Err(LspRuntimeFailure::new("lsp-code-index-project-mismatch"));
        }
        if self.repository != scope.repository_id {
            return Err(LspRuntimeFailure::new("lsp-code-index-repository-mismatch"));
        }
        if self.worktree.as_ref() != Some(&scope.worktree_id) {
            return Err(LspRuntimeFailure::new("lsp-code-index-worktree-mismatch"));
        }
        if self.reference != scope.reference {
            return Err(LspRuntimeFailure::new("lsp-code-index-reference-mismatch"));
        }
        let head_commit_id = self
            .source_revision
            .ok_or_else(|| LspRuntimeFailure::new("lsp-code-index-source-revision-unavailable"))?;
        let generation = generation_sequence(&self.code_generation_id)
            .ok_or_else(|| LspRuntimeFailure::new("current-generation-invalid"))?;
        Ok(LspFeedbackProjectionScope {
            head_commit_id,
            code_generation_id: self.code_generation_id,
            snapshot_digest: self.snapshot_digest,
            invalidation_digest: self.invalidation_digest,
            snapshot_content_digest: self.snapshot_content_digest,
            document_content_digest: self.document_content_digest,
            generation,
        })
    }
}

pub trait LspCodeIndexProjectionIdentityPort: Send + Sync {
    fn current_identity(
        &self,
        project_root: PathBuf,
        document_relative_path: Option<String>,
    ) -> LspRuntimeFuture<Result<LspCodeIndexProjectionIdentity, LspRuntimeFailure>>;
}

fn generation_sequence(generation: &CodeGenerationId) -> Option<u64> {
    generation.as_str().split('.').nth(3)?.parse().ok()
}
