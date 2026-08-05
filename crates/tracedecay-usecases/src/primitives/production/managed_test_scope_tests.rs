use std::path::PathBuf;
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_application::ResolvedScope;
use tracedecay_domain::{
    CodeGenerationId, CommitId, ContentDigest, ManifestDigest, ProjectId, RefId, RepositoryId,
    WorktreeId,
};
use tracedecay_lsp::{LspRuntimeFailure, LspRuntimeFuture};

use super::{
    LspCodeIndexProjectionIdentityPort, ManagedTestRunCurrentScopePort,
    ProductionManagedTestRunCurrentScope,
};
use crate::lsp_runtime::LspCodeIndexProjectionIdentity;

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
        id::<ProjectId>("project.managed-tests"),
        id::<RepositoryId>("repository.managed-tests"),
        id::<WorktreeId>("worktree.managed-tests"),
        Some(id::<RefId>("ref.managed-tests")),
    )
    .expect("scope")
}

struct SealedGeneration;

impl LspCodeIndexProjectionIdentityPort for SealedGeneration {
    fn current_identity(
        &self,
        _project_root: PathBuf,
        _document_relative_path: Option<String>,
    ) -> LspRuntimeFuture<Result<LspCodeIndexProjectionIdentity, LspRuntimeFailure>> {
        Box::pin(async {
            Ok(LspCodeIndexProjectionIdentity {
                repository: id("repository.managed-tests"),
                worktree: Some(id("worktree.managed-tests")),
                reference: Some(id("ref.managed-tests")),
                source_revision: Some(id("commit.sealed-generation")),
                code_generation_id: id::<CodeGenerationId>("generation.managed.tests.9"),
                snapshot_digest: id::<ManifestDigest>(&digest('a')),
                invalidation_digest: id::<ManifestDigest>(&digest('b')),
                snapshot_content_digest: id::<ContentDigest>(&digest('c')),
                document_content_digest: None,
            })
        })
    }
}

#[tokio::test]
async fn managed_test_currentness_uses_the_sealed_generation_without_live_git() {
    let non_repository = TempDir::new().expect("non-repository root");
    let owner = ProductionManagedTestRunCurrentScope {
        project_root: non_repository.path().to_path_buf(),
        scope: scope(),
        code_index: Arc::new(SealedGeneration),
    };

    let current = owner
        .current_identity()
        .await
        .expect("sealed generation is the only currentness authority");

    assert_eq!(
        current.head_commit_id,
        id::<CommitId>("commit.sealed-generation")
    );
    assert_eq!(
        current.code_generation_id,
        id::<CodeGenerationId>("generation.managed.tests.9")
    );
}
