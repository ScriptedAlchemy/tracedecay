use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tempfile::TempDir;
use tracedecay_contracts::ResolvedScope;
use tracedecay_contracts::code_index_freshness::CodeIndexFreshnessReadFailureV1;
use tracedecay_domain::ProjectId;

use super::{CodeGraphActivationAuthorityV1, CodeIndexSchedulerRegistryV1};
use crate::code_index_scheduler::tests::{GitFixture, wait_for_queryable_text_generation};
use crate::code_index_scheduler::{
    CodeIndexSourcePendingChangedFutureV1, CodeIndexSourcePendingReaderV1,
};

struct UnavailablePendingSource;

impl CodeIndexSourcePendingReaderV1 for UnavailablePendingSource {
    fn read_pending(&self) -> Result<bool, CodeIndexFreshnessReadFailureV1> {
        Err(CodeIndexFreshnessReadFailureV1::ReadFailed)
    }

    fn read_pending_blocking(&self) -> Result<bool, CodeIndexFreshnessReadFailureV1> {
        self.read_pending()
    }

    fn revision(&self) -> u64 {
        0
    }

    fn changed(&self, _observed: u64) -> CodeIndexSourcePendingChangedFutureV1<'_> {
        Box::pin(std::future::pending())
    }
}

#[tokio::test]
async fn unreadable_pending_source_preserves_the_last_complete_text_owner() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_probe() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree_inner(
            ProjectId::new("project.pending-source-read").unwrap(),
            fixture.path(),
            store.path().to_path_buf(),
            CodeGraphActivationAuthorityV1::Memory {
                policy: Arc::new(AtomicBool::new(true)),
            },
            crate::config::registry_default_index_path_policy(),
            Some(Arc::new(UnavailablePendingSource)),
        )
        .await
        .expect("mount worktree");
    let published = wait_for_queryable_text_generation(&registry, fixture.path()).await;
    let snapshot = published.metadata().snapshot();
    let scope = ResolvedScope::new(
        published.metadata().manifest().project_id.clone(),
        snapshot.repository.clone(),
        snapshot.worktree.clone().expect("worktree identity"),
        snapshot.reference.clone(),
    )
    .expect("published scope");
    for (retained, current) in [
        registry
            .retained_text_owner_freshness_for_scope(&scope)
            .await
            .expect("graph reads retain the immutable owner"),
        registry
            .latest_text_serving_freshness_for_scope(&scope)
            .await
            .expect("text reads retain the immutable owner"),
    ] {
        assert_eq!(
            retained.metadata().manifest().generation_id,
            published.metadata().manifest().generation_id
        );
        assert!(!current, "unreadable evidence cannot prove source currency");
    }
    assert!(matches!(
        registry.dashboard_freshness_read(fixture.path()).await,
        Err(CodeIndexFreshnessReadFailureV1::ReadFailed)
    ));
    registry.shutdown().await;
}
