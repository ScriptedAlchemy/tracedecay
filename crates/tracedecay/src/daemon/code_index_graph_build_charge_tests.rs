//! Sealed graph build admission charges across builds of identical content.

use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_code_index_runtime::code_index_scheduler::{
    CodeGraphActivationPolicyV1, CodeIndexSchedulerRegistryV1,
};
use tracedecay_contracts::code_index_freshness::{
    CodeGraphServingReadinessV1, CodeIndexReadinessTargetV1, CodeIndexReadinessWaitReadV1,
};
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, ProcessResidentSampleV1, ResidentMemoryPressureV1,
};
use tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1;

use super::code_index_runtime_graph_activation_tests::{
    ALPHA_LIB_V1, GitFixture, git, test_project_id,
};

const PROCESS_BYTES: u64 = 64 << 20;
const PEAK_WINDOW_GROWTH_BYTES: u64 = 64 << 30;
const PROCESS_LIMIT_BYTES: u64 = 8 << 30;

/// A first index projects text beside its graph build, under the text
/// build's own reservation. Growth read in that window, here a synthetic
/// RSS series that rises by 64 GiB whenever the build's peak sampler reads
/// it, belongs to the projection. The next build of identical content under
/// an 8 GiB ceiling is therefore charged its structural bound and publishes;
/// carrying the overlapped reading forward would charge it 64 GiB and refuse.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graph_build_beside_a_text_projection_leaves_the_next_charge_at_its_bound() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let profile = TempDir::new().expect("profile root");
    let profile_root = profile.path().join("profile");
    let project_id = test_project_id();
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        fixture.path(),
        project_id.as_str(),
    )
    .expect("project enrollment");
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("profile identity");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        98,
        "graph build charge across identical content",
    )
    .expect("daemon database scope");
    let graph_runtime = Arc::new(
        DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("graph runtime registry"),
    );
    let project_database = graph_runtime
        .project_memory(project_id.clone(), [fixture.path().to_path_buf()])
        .await
        .expect("writable project database");
    tracedecay_project::test_support::host_admission::await_bound_graph_runtime(
        &project_database,
        "bind graph build charge runtime",
    )
    .await
    .expect("bound project graph runtime");

    let limit = NonZeroU64::new(PROCESS_LIMIT_BYTES).expect("process limit");
    let pressure = Arc::new(ResidentMemoryPressureV1::with_sampler(
        limit,
        Arc::new(|| {
            let peak_window = std::thread::current().name() == Some("resident-peak");
            let bytes = PROCESS_BYTES
                + if peak_window {
                    PEAK_WINDOW_GROWTH_BYTES
                } else {
                    0
                };
            Some(ProcessResidentSampleV1 {
                resident_bytes: bytes,
                unreclaimable_bytes: bytes,
            })
        }),
    ));
    let registry =
        CodeIndexSchedulerRegistryV1::with_resident_memory_and_progress_producer_incarnation(
            1,
            Arc::new(ProcessResidentMemoryV1::with_pressure(limit, pressure)),
            1,
        );
    registry
        .mount_worktree_with_graph_runtime(
            project_id,
            fixture.path(),
            store.path().to_path_buf(),
            graph_runtime.code_graph_seat_port(),
            project_database,
            CodeGraphActivationPolicyV1::Enabled,
        )
        .await
        .expect("mount first index");
    let first = wait_for_graph_serving(&registry, &fixture, None).await;

    git(
        fixture.path(),
        &["commit", "--allow-empty", "-qm", "same tree"],
    );
    let second = wait_for_graph_serving(&registry, &fixture, Some(&first.0)).await;
    assert_ne!(
        second.0, first.0,
        "the moved HEAD publishes a new generation"
    );
    assert_eq!(
        second.1, first.1,
        "the new generation carries identical content"
    );

    registry.shutdown().await;
    graph_runtime
        .shutdown_memory_graph_reconciliation_tasks()
        .await
        .expect("join graph reconciliation tasks");
}

/// Wait until a generation other than `previous` serves its graph, and
/// return its id and content identity.
async fn wait_for_graph_serving(
    registry: &CodeIndexSchedulerRegistryV1,
    fixture: &GitFixture,
    previous: Option<&str>,
) -> (String, String) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let reached = registry
            .wait_for_readiness(
                fixture.path(),
                CodeIndexReadinessTargetV1::Fresh,
                deadline.saturating_duration_since(tokio::time::Instant::now()),
            )
            .await
            .expect("readiness read");
        let text = registry
            .retained_text_owner_for_root(fixture.path())
            .await
            .expect("text owner");
        let generation = text.metadata().manifest().generation_id.as_str().to_owned();
        let readiness = text.code_graph_serving_readiness();
        if previous != Some(generation.as_str()) && readiness == CodeGraphServingReadinessV1::Ready
        {
            let content = text
                .metadata()
                .snapshot()
                .content_identity
                .as_str()
                .to_owned();
            return (generation, content);
        }
        let freshness = registry.dashboard_freshness(fixture.path()).await;
        assert_eq!(
            freshness
                .as_ref()
                .and_then(|freshness| freshness.parked.as_ref()),
            None,
            "generation {generation} graph build was refused"
        );
        assert!(
            tokio::time::Instant::now() < deadline
                && !matches!(reached, CodeIndexReadinessWaitReadV1::TimedOut { .. }),
            "generation {generation} graph never served: {readiness:?}, {freshness:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
