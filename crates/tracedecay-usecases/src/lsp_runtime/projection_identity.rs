use std::path::PathBuf;

use tracedecay_application::ResolvedScope;
use tracedecay_domain::{
    CodeGenerationId, CommitId, ContentDigest, ManifestDigest, RefId, RepositoryId, WorktreeId,
};
use tracedecay_lsp::{LspRuntimeFailure, LspRuntimeFuture};

use super::LspFeedbackProjectionScope;

/// Exact current immutable code-index identity resolved by the daemon-owned
/// mounted worktree scheduler. A mutable graph database or path-derived
/// generation is not a legal implementation of this port.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspCodeIndexProjectionIdentity {
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
    /// Admit one sealed generation only when it exactly matches the registered
    /// project scope. Live repository state is not consulted.
    pub fn admit_for_scope(
        self,
        scope: &ResolvedScope,
    ) -> Result<LspFeedbackProjectionScope, LspRuntimeFailure> {
        scope
            .validate()
            .map_err(|_| LspRuntimeFailure::new("registered-project-scope-invalid"))?;
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

#[cfg(test)]
mod tests {
    use super::LspCodeIndexProjectionIdentity;
    use tracedecay_application::ResolvedScope;
    use tracedecay_domain::{
        CodeGenerationId, CommitId, ContentDigest, ManifestDigest, ProjectId, RefId, RepositoryId,
        WorktreeId,
    };

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn scope() -> ResolvedScope {
        ResolvedScope::new(
            id::<ProjectId>("project.lsp-scope"),
            id::<RepositoryId>("repository.lsp-scope"),
            id::<WorktreeId>("worktree.lsp-scope"),
            Some(id::<RefId>("ref.main")),
        )
        .expect("valid resolved scope")
    }

    fn identity() -> LspCodeIndexProjectionIdentity {
        LspCodeIndexProjectionIdentity {
            repository: id("repository.lsp-scope"),
            worktree: Some(id("worktree.lsp-scope")),
            reference: Some(id("ref.main")),
            source_revision: Some(id("commit.lsp-scope")),
            code_generation_id: id::<CodeGenerationId>("generation.lsp.scope.7"),
            snapshot_digest: id::<ManifestDigest>(&digest('a')),
            invalidation_digest: id::<ManifestDigest>(&digest('b')),
            snapshot_content_digest: id::<ContentDigest>(&digest('c')),
            document_content_digest: Some(id::<ContentDigest>(&digest('d'))),
        }
    }

    #[test]
    fn projection_scope_rejects_repository_worktree_and_reference_mismatch() {
        let cases = [
            (
                {
                    let mut value = identity();
                    value.repository = id("repository.other");
                    value
                },
                "lsp-code-index-repository-mismatch",
            ),
            (
                {
                    let mut value = identity();
                    value.worktree = Some(id("worktree.other"));
                    value
                },
                "lsp-code-index-worktree-mismatch",
            ),
            (
                {
                    let mut value = identity();
                    value.reference = Some(id("ref.other"));
                    value
                },
                "lsp-code-index-reference-mismatch",
            ),
        ];

        for (identity, expected) in cases {
            let error = identity
                .admit_for_scope(&scope())
                .expect_err("foreign generation identity must be rejected");
            assert_eq!(error.class(), expected);
        }
    }

    #[test]
    fn projection_scope_requires_a_sealed_source_revision() {
        let mut identity = identity();
        identity.source_revision = None;

        let error = identity
            .admit_for_scope(&scope())
            .expect_err("an unsealed generation must not fabricate HEAD");

        assert_eq!(error.class(), "lsp-code-index-source-revision-unavailable");
    }

    #[test]
    fn projection_scope_uses_the_sealed_generation_identity() {
        let admitted = identity()
            .admit_for_scope(&scope())
            .expect("exact generation identity is admitted");

        assert_eq!(admitted.head_commit_id, id::<CommitId>("commit.lsp-scope"));
        assert_eq!(admitted.generation, 7);
        assert_eq!(
            admitted.document_content_digest,
            Some(id::<ContentDigest>(&digest('d')))
        );
    }
}
