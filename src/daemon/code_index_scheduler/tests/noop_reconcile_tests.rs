use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_usecases::semantic_runtime::SavedCodeGenerationScheduleHookV1;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unchanged_reconcile_does_not_reactivate_the_serving_generation() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount");
    let serving_generation = wait_for_initial_generation(&registry, fixture.path()).await;
    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler handle");
    let worktree_id = scheduler
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .latest_complete()
        .expect("sealed generation")
        .generation()
        .snapshot()
        .worktree
        .clone()
        .expect("worktree identity");
    let before_receipts = registry.event_to_ready_receipts().len();

    // Any redundant graph activation now fails. An unchanged reconcile must
    // still reach its Noop receipt by retaining the already-serving graph.
    super::super::graph_activation::set_injected_activation_failures(&worktree_id, usize::MAX);
    assert!(
        registry.notify_hook_overflow(fixture.path()).await,
        "mounted worktree accepts an unchanged reconcile"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let receipts = registry.event_to_ready_receipts();
        if let Some(receipt) = receipts.get(before_receipts) {
            assert!(receipt.is_noop(), "unchanged reconcile must be a no-op");
            break;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "unchanged reconcile retried graph activation instead of settling"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    super::super::graph_activation::set_injected_activation_failures(&worktree_id, 0);
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(serving_generation),
        "the unchanged reconcile retains the exact serving generation"
    );
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unchanged_reconcile_retries_semantic_admission_for_the_serving_generation() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let accepting = Arc::new(AtomicBool::new(false));
    let accepted = Arc::new(AtomicUsize::new(0));
    let semantic_hook = {
        let accepting = Arc::clone(&accepting);
        let accepted = Arc::clone(&accepted);
        Arc::new(move |_: &CodeIndexPublishedGenerationV1| {
            if accepting.load(Ordering::Acquire) {
                accepted.fetch_add(1, Ordering::AcqRel);
                true
            } else {
                false
            }
        }) as SavedCodeGenerationScheduleHookV1
    };
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            Some(semantic_hook),
        )
        .await
        .expect("mount");
    let serving_generation = wait_for_initial_generation(&registry, fixture.path()).await;
    assert_eq!(
        accepted.load(Ordering::Acquire),
        0,
        "the initial bounded semantic admission is refused"
    );

    accepting.store(true, Ordering::Release);
    assert!(
        registry.notify_hook_overflow(fixture.path()).await,
        "mounted worktree accepts an unchanged reconcile"
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while accepted.load(Ordering::Acquire) == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("unchanged reconcile retries semantic admission");

    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(serving_generation),
        "semantic retry keeps the exact serving generation"
    );
    registry.shutdown().await;
}
