use std::{path::PathBuf, sync::Arc};

use tracedecay_application::ResolvedScope;
use tracedecay_domain::{
    CodeGenerationId, ContentDigest, ManifestDigest, ProjectId, RefId, RepositoryId, WorktreeId,
};
use tracedecay_lsp::{LspRuntimeFailure, LspRuntimeFuture};

use super::{ManagedTestRunCurrentScopePort, ProductionManagedTestRunCurrentScope};
use crate::lsp_runtime::{LspCodeIndexProjectionIdentity, LspCodeIndexProjectionIdentityPort};

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

struct SealedIdentity;

impl LspCodeIndexProjectionIdentityPort for SealedIdentity {
    fn current_identity(
        &self,
        _project_root: PathBuf,
        _document_relative_path: Option<String>,
    ) -> LspRuntimeFuture<Result<LspCodeIndexProjectionIdentity, LspRuntimeFailure>> {
        Box::pin(async {
            Ok(LspCodeIndexProjectionIdentity {
                project: id("project.managed-test"),
                repository: id("repository.managed-test"),
                worktree: Some(id("worktree.managed-test")),
                reference: Some(id("ref.main")),
                source_revision: Some(id("commit.sealed")),
                code_generation_id: id::<CodeGenerationId>("generation.managed.test.9"),
                snapshot_digest: id::<ManifestDigest>(&digest('a')),
                invalidation_digest: id::<ManifestDigest>(&digest('b')),
                snapshot_content_digest: id::<ContentDigest>(&digest('c')),
                document_file_occurrence_id: None,
                document_content_digest: None,
            })
        })
    }
}

#[tokio::test]
async fn managed_test_currentness_uses_sealed_scope_without_live_git() {
    let directory = tempfile::tempdir().expect("temporary non-git directory");
    let scope = ResolvedScope::new(
        id::<ProjectId>("project.managed-test"),
        id::<RepositoryId>("repository.managed-test"),
        id::<WorktreeId>("worktree.managed-test"),
        Some(id::<RefId>("ref.main")),
    )
    .expect("valid scope");
    let authority = ProductionManagedTestRunCurrentScope::new(
        directory.path().to_path_buf(),
        scope,
        Arc::new(SealedIdentity),
    );

    let current = authority
        .current_identity()
        .await
        .expect("sealed identity is current without a Git repository");

    assert_eq!(current.head_commit_id, id("commit.sealed"));
    assert_eq!(
        current.code_generation_id,
        id::<CodeGenerationId>("generation.managed.test.9")
    );
}
