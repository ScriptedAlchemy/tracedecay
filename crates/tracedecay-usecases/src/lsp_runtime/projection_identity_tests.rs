use super::LspCodeIndexProjectionIdentity;
use tracedecay_application::ResolvedScope;
use tracedecay_domain::{
    CodeGenerationId, CommitId, ContentDigest, FileOccurrenceId, ManifestDigest, ProjectId, RefId,
    RepositoryId, WorktreeId,
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
        document_file_occurrence_id: Some(id::<FileOccurrenceId>("file.lsp-scope")),
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
            .clone()
            .admit_commit_scope(&scope())
            .expect_err("foreign generation identity must be rejected");
        assert_eq!(error.class(), expected);
        let error = identity
            .admit_worktree_scope(&scope())
            .expect_err("read-only admission must refuse a foreign generation too");
        assert_eq!(error.class(), expected);
    }
}

/// A dirty worktree seals a generation with no commit. Read-only graph queries
/// bind to that generation's exact identity; only commit-bound consumers refuse.
#[test]
fn dirty_worktree_generation_admits_read_only_scope_but_not_commit_scope() {
    let mut dirty = identity();
    dirty.source_revision = None;

    let error = dirty
        .clone()
        .admit_commit_scope(&scope())
        .expect_err("a dirty generation must not fabricate HEAD for commit-bound consumers");
    assert_eq!(error.class(), "lsp-code-index-source-revision-unavailable");

    let admitted = dirty
        .admit_worktree_scope(&scope())
        .expect("read-only queries bind to the sealed dirty generation");
    assert_eq!(admitted.source_revision, None);
    assert_eq!(
        admitted.code_generation_id,
        id::<CodeGenerationId>("generation.lsp.scope.7")
    );
    assert_eq!(admitted.snapshot_digest, id::<ManifestDigest>(&digest('a')));
    assert_eq!(
        admitted.invalidation_digest,
        id::<ManifestDigest>(&digest('b'))
    );
    assert_eq!(
        admitted.snapshot_content_digest,
        id::<ContentDigest>(&digest('c'))
    );
    assert_eq!(admitted.generation, 7);
}

#[test]
fn read_only_scope_still_requires_a_valid_generation_sequence() {
    let mut malformed = identity();
    malformed.source_revision = None;
    malformed.code_generation_id = id::<CodeGenerationId>("generation.lsp.scope.seven");
    let error = malformed
        .admit_worktree_scope(&scope())
        .expect_err("a generation without a sequence cannot be bound");
    assert_eq!(error.class(), "current-generation-invalid");
}

#[test]
fn projection_scope_requires_and_uses_the_sealed_generation_identity() {
    let admitted = identity()
        .admit_commit_scope(&scope())
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
    assert_eq!(
        admitted.document_file_occurrence_id,
        Some(id::<FileOccurrenceId>("file.lsp-scope"))
    );
    assert_eq!(admitted.generation, 7);
}
