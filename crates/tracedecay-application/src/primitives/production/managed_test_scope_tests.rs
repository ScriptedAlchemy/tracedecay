use std::{path::PathBuf, sync::Arc};

use tracedecay_contracts::{ApplicationContractError, FreshnessState, ResolvedScope};
use tracedecay_domain::{
    CodeGenerationId, ContentDigest, ManifestDigest, ProjectId, RefId, RepositoryId, WorktreeId,
};
use tracedecay_lsp::{LspRuntimeFailure, LspRuntimeFuture};

use super::{ManagedTestRunCurrentScopePort, ProductionManagedTestRunCurrentScope};
use crate::lsp_runtime::{LspCodeIndexProjectionIdentity, LspCodeIndexProjectionIdentityPort};

use tracedecay_domain::test_fixtures::id;

use tracedecay_domain::test_fixtures::repeated_sha256_text as digest;

struct SealedIdentity(FreshnessState);

impl LspCodeIndexProjectionIdentityPort for SealedIdentity {
    fn current_identity(
        &self,
        _project_root: PathBuf,
        _document_relative_path: Option<String>,
    ) -> LspRuntimeFuture<Result<LspCodeIndexProjectionIdentity, LspRuntimeFailure>> {
        let source_freshness = self.0;
        Box::pin(async move {
            Ok(LspCodeIndexProjectionIdentity {
                project: id("project.managed-test"),
                repository: id("repository.managed-test"),
                worktree: Some(id("worktree.managed-test")),
                reference: Some(id("ref.main")),
                head_commit_id: Some(id("commit.head")),
                source_freshness,
                source_revision: None,
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
async fn managed_test_currentness_uses_live_head_for_dirty_sealed_generation() {
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
        Arc::new(SealedIdentity(FreshnessState::Current)),
    );

    let current = authority
        .current_identity()
        .await
        .expect("dirty sealed identity is current under the live HEAD");

    assert_eq!(current.head_commit_id, id("commit.head"));
    assert_eq!(
        current.code_generation_id,
        id::<CodeGenerationId>("generation.managed.test.9")
    );
}

#[tokio::test]
async fn managed_test_currentness_refuses_unproven_retained_source() {
    let scope = ResolvedScope::new(
        id::<ProjectId>("project.managed-test"),
        id::<RepositoryId>("repository.managed-test"),
        id::<WorktreeId>("worktree.managed-test"),
        Some(id::<RefId>("ref.main")),
    )
    .unwrap();
    for freshness in [FreshnessState::Unknown, FreshnessState::Stale] {
        let authority = ProductionManagedTestRunCurrentScope::new(
            PathBuf::from("/managed-test"),
            scope.clone(),
            Arc::new(SealedIdentity(freshness)),
        );
        assert_eq!(
            authority.current_identity().await.unwrap_err(),
            ApplicationContractError::Inconsistent {
                field: "managed test result code generation",
            }
        );
    }
}
