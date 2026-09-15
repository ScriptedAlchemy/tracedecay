use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_contracts::doctor::DoctorStorageFamilyReadV1;
use tracedecay_contracts::storage::compaction::CompactionThresholdConfig;
use tracedecay_domain::{CodeGenerationId, canonical_text::sha256_hex};

use super::journey_test_support::git;
use super::*;
use crate::daemon::maintenance::project_store_maintenance_lease;
use tracedecay_code_index_retention::code_index_generations::{
    MAX_CODE_GENERATION_RETENTION_BATCH_V1, prepare_next_code_generation_retention_cancellable,
};
use tracedecay_maintenance::tick::{MaintenanceContinuation, MaintenanceTickOutcome};

fn initialize_git_project(root: &Path) {
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.name", "TraceDecay Test"]);
    git(
        root,
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    std::fs::create_dir_all(root.join("src")).expect("source directory");
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn retained() -> usize { 0 }\n",
    )
    .expect("initial source");
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "initial"]);
}

async fn wait_for_changed_generation(
    schedulers: &tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    prior: &CodeGenerationId,
) -> CodeGenerationId {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(current) = schedulers.latest_generation_id(project_root).await
                && &current != prior
            {
                return current;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("changed code generation")
}

async fn publish_code_edit(
    schedulers: &tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    prior: &CodeGenerationId,
    revision: usize,
) -> CodeGenerationId {
    std::fs::write(
        project_root.join("src/lib.rs"),
        format!("pub fn retained() -> usize {{ {revision} }}\n"),
    )
    .expect("edit source");
    assert!(
        schedulers
            .notify_hook_paths(project_root, &["src/lib.rs".to_owned()])
            .await,
        "mounted scheduler accepts the exact worktree hint"
    );
    wait_for_changed_generation(schedulers, project_root, prior).await
}

/// Code-generation retention on a mounted production route: superseded sealed
/// generations are collectable once the serving head moves past them, the
/// maintenance cadence collects them, and the Doctor census reads the same
/// store through the retained daemon-service signature.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mounted_code_generation_retention_continues_capped_segment_reclamation() {
    let compaction = CompactionThresholdConfig::default();
    let isolation = TempDir::new().expect("isolated production composition");
    let project_root = isolation.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project root");
    initialize_git_project(&project_root);

    let harness =
        ProductionProjectCompositionHarnessV1::open(isolation.path(), [project_root.clone()])
            .await
            .expect("mounted production composition");
    let resources = harness.resources.as_ref().expect("live harness resources");
    let schedulers = &resources.invocation.code_index_schedulers;
    let graph = harness
        .server(&project_root)
        .expect("project server")
        .cg()
        .await;
    let canonical_root = graph.project_root().to_path_buf();
    let first_source = schedulers
        .latest_generation_id(&canonical_root)
        .await
        .expect("initial sealed code generation");

    let mut latest = first_source.clone();
    for revision in 1..=4 {
        latest = publish_code_edit(schedulers, &canonical_root, &latest, revision).await;
    }
    assert_ne!(latest, first_source);
    let code_store_root =
        tracedecay_code_index_runtime::code_index_scheduler::scoped_code_index_store_root(
            &graph.store_layout().data_root.join("code-index-v1"),
            &canonical_root,
        );
    let graph_replay_pool_root = graph.db().database_path().with_extension("graph-replay");
    let plan = prepare_next_code_generation_retention_cancellable(
        &code_store_root,
        &BTreeSet::new(),
        &|| false,
        Some(&graph_replay_pool_root),
    )
    .expect("code generation retention plan");
    let first_candidate = plan
        .collectable_generations
        .iter()
        .find(|generation| generation.generation_id == first_source)
        .unwrap_or_else(|| {
            panic!(
                "superseded source is collectable: first_source={first_source} \
                 active={:?} collectable={:?} superseded={:?}",
                plan.active_generation_id,
                plan.collectable_generations
                    .iter()
                    .map(|generation| generation.generation_id.as_str())
                    .collect::<Vec<_>>(),
                plan.superseded_generations
                    .iter()
                    .map(|generation| generation.generation_id.as_str())
                    .collect::<Vec<_>>(),
            )
        });
    let first_source_file = code_store_root
        .join("code-generations-v1")
        .join(&first_candidate.generation_file);
    assert!(first_source_file.is_file());

    let observations = resources.store_administration.store_telemetry_sampling();
    let cancellation = tracedecay_session_memory::context::CancellationToken::new();
    let findings_before =
        tracedecay_daemon_service::doctor_kernel::collect_code_generation_retention_findings(
            schedulers,
            graph.profile_database().as_ref(),
            &code_store_root,
            &canonical_root,
            graph.db(),
        )
        .await;
    assert!(
        matches!(
            findings_before,
            DoctorStorageFamilyReadV1::Observed { .. }
                | DoctorStorageFamilyReadV1::ObservedIncomplete { .. }
        ),
        "the mounted store is censused before retention runs: {findings_before:?}"
    );

    let outcome = tokio::time::timeout(Duration::from_mins(1), async {
        loop {
            let outcome = tracedecay_maintenance::generation::run_project_generation_maintenance(
                &project_store_maintenance_lease(graph.as_ref()),
                schedulers,
                &observations,
                &cancellation,
                Some(&compaction),
                None,
            )
            .await;
            if outcome.is_complete() {
                return outcome;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("generation maintenance converges");
    assert!(outcome.is_complete());
    assert!(
        !first_source_file.exists(),
        "the superseded sealed generation is collected once the serving head moved past it"
    );
    let serving = schedulers
        .latest_generation_id(&canonical_root)
        .await
        .expect("serving code generation survives retention");
    assert_eq!(serving, latest);

    let segment_root = code_store_root.join("code-generation-segments-v1");
    let orphan_segments = (0..=MAX_CODE_GENERATION_RETENTION_BATCH_V1)
        .map(|index| {
            let bytes = format!("unreferenced production segment {index}");
            let path = segment_root.join(format!("segment-{}.json", sha256_hex(bytes.as_bytes())));
            std::fs::write(&path, bytes).expect("write unreferenced segment");
            path
        })
        .collect::<Vec<_>>();
    let outcome = tracedecay_maintenance::generation::run_project_generation_maintenance(
        &project_store_maintenance_lease(graph.as_ref()),
        schedulers,
        &observations,
        &cancellation,
        Some(&compaction),
        None,
    )
    .await;
    assert_eq!(
        outcome,
        MaintenanceTickOutcome::Continue(MaintenanceContinuation::CodeGenerationRetention),
        "a capped segment-only pass must keep the production maintenance cadence short"
    );
    assert_eq!(
        orphan_segments.iter().filter(|path| path.exists()).count(),
        1,
        "one bounded maintenance pass must reclaim exactly one segment batch"
    );

    let findings_after =
        tracedecay_daemon_service::doctor_kernel::collect_code_generation_retention_findings(
            schedulers,
            graph.profile_database().as_ref(),
            &code_store_root,
            &canonical_root,
            graph.db(),
        )
        .await;
    assert!(
        matches!(
            findings_after,
            DoctorStorageFamilyReadV1::Observed { .. }
                | DoctorStorageFamilyReadV1::ObservedIncomplete { .. }
        ),
        "the Doctor census still reads the retained store: {findings_after:?}"
    );

    drop(graph);
    harness.shutdown().await;
}
