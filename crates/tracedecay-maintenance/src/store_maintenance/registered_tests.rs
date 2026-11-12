#![allow(clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use tracedecay_code_index_retention::code_index_generations::fixture::write_generation_store_fixture;
use tracedecay_code_index_retention::code_index_generations::{
    code_generation_graph_replay_release_page, code_index_store_root, record_scope_root,
};
use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::resident_memory::{
    DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1, ProcessResidentMemoryV1,
};
use tracedecay_runtime_core::storage::profile_sharded_data_root;

use super::{
    CodeGenerationRetentionOutcomeV1, RegisteredProjectStoreV1,
    run_registered_code_generation_retention,
};
use crate::telemetry::StoreTelemetrySamplingRegistry;

/// A scheduler registry with nothing mounted, through the production
/// constructor.
pub(crate) fn unmounted_scheduler_registry() -> CodeIndexSchedulerRegistryV1 {
    CodeIndexSchedulerRegistryV1::with_resident_memory_and_progress_producer_incarnation(
        1,
        Arc::new(ProcessResidentMemoryV1::new(
            DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
        )),
        1,
    )
}

fn generation_file_count(store_root: &Path) -> usize {
    std::fs::read_dir(store_root.join("code-generations-v1"))
        .expect("list generations")
        .count()
}

/// The unmounted primary scope is collected while the linked worktree's scope,
/// which a mounted graph owns, is left to that graph's own pass. The retired
/// generations wait in the release queue for the next mount.
#[tokio::test]
async fn registered_pass_collects_unmounted_scopes_and_leaves_mounted_ones() {
    let temp = tempfile::TempDir::new().expect("temp root");
    let profile_root = temp.path().join("profile");
    let checkout = temp.path().join("checkout");
    let linked = temp.path().join("linked");
    std::fs::create_dir_all(&checkout).expect("checkout");
    std::fs::create_dir_all(&linked).expect("linked worktree");
    let checkout = checkout.canonicalize().expect("canonical checkout");
    let linked = linked.canonicalize().expect("canonical linked worktree");
    let runtime = RegisteredGlobalDbTestRuntime::profile(&profile_root)
        .await
        .expect("profile database");
    let profile_database = runtime.profile_database_arc();
    let data_root = profile_sharded_data_root(&profile_root, "proj_registered");
    let primary = code_index_store_root(&data_root, &checkout);
    let mounted = code_index_store_root(&data_root, &linked);
    write_generation_store_fixture(&primary, 6);
    write_generation_store_fixture(&mounted, 4);
    record_scope_root(&mounted, &linked).expect("record linked scope root");
    let store = RegisteredProjectStoreV1 {
        data_root,
        canonical_root: checkout,
    };
    let schedulers = unmounted_scheduler_registry();
    let observations = StoreTelemetrySamplingRegistry::default();
    let mounted_store_roots = BTreeSet::from([mounted.clone()]);

    let first = run_registered_code_generation_retention(
        &store,
        &mounted_store_roots,
        &schedulers,
        &profile_database,
        &observations,
        &CancellationToken::new(),
    )
    .await;

    assert_eq!(first, CodeGenerationRetentionOutcomeV1::MoreWork);
    assert_eq!(generation_file_count(&primary), 1);
    assert_eq!(generation_file_count(&mounted), 4);
    assert_eq!(
        code_generation_graph_replay_release_page(&primary, None)
            .expect("release queue")
            .releases
            .len(),
        5
    );

    let second = run_registered_code_generation_retention(
        &store,
        &mounted_store_roots,
        &schedulers,
        &profile_database,
        &observations,
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(second, CodeGenerationRetentionOutcomeV1::Complete);

    let unmounted_everywhere = run_registered_code_generation_retention(
        &store,
        &BTreeSet::new(),
        &schedulers,
        &profile_database,
        &observations,
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(
        unmounted_everywhere,
        CodeGenerationRetentionOutcomeV1::MoreWork
    );
    assert_eq!(generation_file_count(&mounted), 1);
}
