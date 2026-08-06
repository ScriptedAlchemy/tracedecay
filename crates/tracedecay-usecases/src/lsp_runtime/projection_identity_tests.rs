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
        project: id("project.lsp-scope"),
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
fn projection_scope_rejects_project_repository_worktree_and_reference_mismatch() {
    let cases = [
        (
            {
                let mut value = identity();
                value.project = id("project.other");
                value
            },
            "lsp-code-index-project-mismatch",
        ),
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
fn projection_scope_requires_and_uses_the_sealed_generation_identity() {
    let mut unsealed = identity();
    unsealed.source_revision = None;
    let error = unsealed
        .admit_for_scope(&scope())
        .expect_err("an unsealed generation must not fabricate HEAD");
    assert_eq!(error.class(), "lsp-code-index-source-revision-unavailable");

    let admitted = identity()
        .admit_for_scope(&scope())
        .expect("exact generation identity is admitted");
    assert_eq!(admitted.head_commit_id, id::<CommitId>("commit.lsp-scope"));
    assert_eq!(
        admitted.code_generation_id,
        id::<CodeGenerationId>("generation.lsp.scope.7")
    );
    assert_eq!(
        admitted.document_content_digest,
        Some(id::<ContentDigest>(&digest('d')))
    );
    assert_eq!(admitted.generation, 7);
}
