use super::*;

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
        )
        .await
        .expect("mount");
    wait_for_initial_generation(&registry, fixture.path()).await;
    // The seat is published mid-pass and the seat no longer waits for the
    // clone successor, so the mount's own receipt lands after the in-progress
    // guard drops and the leftover backfill drains on later wakes that post
    // receipts of their own. Settle that whole chain first: a wake still
    // pending when the overflow arrives keeps its earlier arrival instant, and
    // the pass would then answer for both.
    drain_clone_backfill(&registry, fixture.path()).await;
    wait_for_settled_owner(&registry, fixture.path()).await;
    wait_for_event_to_ready(&registry).await;
    let admission = quiesced_background_reconcile_admission(&registry, fixture.path()).await;
    let serving_generation = registry
        .latest_generation_id(fixture.path())
        .await
        .expect("serving generation");
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
    // Receipts are attributed by the arrival the pass claimed, not by list
    // position: a mount-era receipt that lands after this instant still
    // belongs to the mount. Only a wake accepted from here on is this
    // reconcile's.
    let overflow_at = tracedecay_contracts::now_micros().0;

    // Any redundant graph activation now fails. An unchanged reconcile must
    // still reach its Noop receipt by retaining the already-serving graph.
    super::super::graph_activation::set_injected_activation_failures(&worktree_id, usize::MAX);
    assert!(
        matches!(
            registry.notify_hook_overflow(fixture.path()).await,
            super::super::CodeIndexDemandAdmissionV1::Queued
        ),
        "mounted worktree accepts an unchanged reconcile"
    );
    drop(admission);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let receipts = registry.event_to_ready_receipts();
        if let Some(receipt) = receipts.iter().find(|receipt| {
            receipt
                .arrival
                .wake_micros()
                .is_some_and(|wake_micros| wake_micros >= overflow_at)
        }) {
            assert!(
                receipt.is_noop(),
                "unchanged reconcile must be a no-op: {receipts:#?}"
            );
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
