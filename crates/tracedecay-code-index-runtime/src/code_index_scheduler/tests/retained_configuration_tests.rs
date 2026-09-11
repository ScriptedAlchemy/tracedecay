use std::sync::Arc;

use tempfile::TempDir;

use super::{
    CodeIndexSchedulerRegistryV1, GitFixture, SharedCodeIndexBytePoolV1, published,
    replace_scheduler_chunker_revision, rewrite_active_rust_extractor_revision, scheduler,
    test_project_id, wait_for_queryable_text_generation_change, wait_for_quiescent_owner_pass,
};
use crate::code_index::production::DAEMON_CODE_INDEX_CHUNKER_REVISION;
use crate::code_index_scheduler::scoped_code_index_store_root;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partitioned_restart_rebuilds_incompatible_retained_generation() {
    let fixture = GitFixture::new(&[
        ("src/lib.rs", "mod inner;\npub use inner::*;\n"),
        ("src/inner.rs", "pub fn exported() {}\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let scoped_store = scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let retained_generation = {
        let mut seed = scheduler(
            &fixture,
            scoped_store.clone(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        replace_scheduler_chunker_revision(
            &mut seed,
            &format!("{DAEMON_CODE_INDEX_CHUNKER_REVISION}-retained"),
        );
        let generation =
            published(seed.reconcile_now().expect("seed retained generation")).generation_id;
        assert!(
            seed.servable_retained_text_generation()
                .expect("seeded text generation")
                .uses_partitioned_manifest(),
            "the restart fixture must use the lightweight partitioned restore path"
        );
        generation
    };
    rewrite_active_rust_extractor_revision(&scoped_store, "extractor.rust.v3");
    let segment_path = std::fs::read_dir(scoped_store.join("code-generation-segments-v1"))
        .expect("read retained segment directory")
        .find_map(|entry| {
            let path = entry.expect("read retained segment entry").path();
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("segment-"))
                .then_some(path)
        })
        .expect("partitioned generation segment");
    std::fs::remove_file(segment_path).expect("remove full-decode segment");

    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    let held_admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold the restart worker before its first pass");
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("restart over retained generation");

    let queued_status = registry
        .dashboard_freshness(fixture.path())
        .await
        .expect("mounted freshness");
    let pass_was_running = registry
        .reconcile_in_progress_for_test(fixture.path())
        .await;
    assert!(
        !pass_was_running,
        "held admission must keep the restart pass queued"
    );
    assert!(
        queued_status.rebuild_in_flight,
        "the queued mount remedy is scheduled work, not an idle status"
    );
    drop(held_admission);

    let replacement =
        wait_for_queryable_text_generation_change(&registry, fixture.path(), &retained_generation)
            .await;
    let replacement_id = replacement.metadata().manifest().generation_id.clone();
    assert_ne!(replacement_id, retained_generation);
    assert_eq!(
        replacement.metadata().manifest().chunker_revision.as_str(),
        DAEMON_CODE_INDEX_CHUNKER_REVISION
    );

    wait_for_quiescent_owner_pass(&registry, fixture.path()).await;
    let current_status = registry
        .dashboard_freshness(fixture.path())
        .await
        .expect("replacement freshness");
    assert!(
        current_status.progress.is_some(),
        "the scheduled replacement must publish observable build progress"
    );
    assert!(
        current_status.generation_recovery.is_none(),
        "the compatible replacement must clear retained-generation recovery"
    );
    assert!(
        !current_status.rebuild_in_flight,
        "status must clear rebuild liveness after the replacement becomes current"
    );
    let canonical_root = fixture.path().canonicalize().expect("canonical fixture");
    let scheduler = {
        let mounted = registry.mounted.lock().await;
        Arc::clone(
            &mounted
                .get(&canonical_root)
                .expect("mounted worktree")
                .scheduler,
        )
    };
    let latest = scheduler
        .lock()
        .expect("scheduler lock")
        .latest_complete()
        .expect("replacement generation");
    assert!(
        latest
            .generation
            .imports()
            .iter()
            .any(|row| row.is_public && row.is_glob),
        "replacement must persist the current Rust public-glob import shape"
    );

    registry.shutdown().await;
}
