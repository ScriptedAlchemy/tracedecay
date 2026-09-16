use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::{ALPHA_LIB_V1, GitFixture, wait_for_initial_generation};
use crate::code_index_scheduler::query_runtime::{
    DeferredMountAttemptV1, retry_deferred_query_authority_until_serving,
};
use crate::code_index_scheduler::CodeIndexSchedulerRegistryV1;

/// Cold deferred mount must wake on publication / serving watches, not a
/// standing 1 Hz `ready_poll`. After the first generation seats, the waiter
/// finishes well under one second.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deferred_query_authority_wakes_without_ready_poll() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let project_root = fixture.path().to_path_buf();
    let attempts = Arc::new(AtomicUsize::new(0));

    let waiter = {
        let registry = registry.clone();
        let project_root = project_root.clone();
        let attempts = Arc::clone(&attempts);
        tokio::spawn(async move {
            retry_deferred_query_authority_until_serving(&registry, project_root, || {
                attempts.fetch_add(1, Ordering::SeqCst);
                async { DeferredMountAttemptV1::Terminal }
            })
            .await;
        })
    };

    // Let the waiter miss the empty slot before mount publishes.
    tokio::task::yield_now().await;

    registry
        .mount_worktree(
            super::test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
        )
        .await
        .expect("mount worktree");
    let _ = wait_for_initial_generation(&registry, fixture.path()).await;

    let started = Instant::now();
    tokio::time::timeout(Duration::from_millis(750), waiter)
        .await
        .expect("deferred mount woke without a 1s ready_poll")
        .expect("deferred mount task");
    assert!(
        started.elapsed() < Duration::from_millis(750),
        "event-driven wake must not wait out a 1 Hz poll"
    );
    assert!(
        attempts.load(Ordering::SeqCst) >= 1,
        "mount attempt must run once a retained text owner is seated"
    );
    registry.shutdown().await;
}
