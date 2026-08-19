//! Shutdown contract for the retained graph runtimes: a healthy daemon
//! shutdown joins every reconciliation worker and then closes every retained
//! Grafeo runtime without a Conflict. This is the registry seam behind the
//! daemon's terminal `memory_graph_reconciliation` shutdown owner; a
//! regression back to close-before-join (or to closing while the registry
//! maps still hold their standing owner attachments) fails the close below
//! with `graph database conflict (operation: close graph runtime for
//! shutdown)` — the exact receipt detail this contract forbids.

use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tempfile::TempDir;
use tracedecay_domain::{ActorId, Confidence, FactCategoryV1, FactOwnerV1, ProjectId};
use tracedecay_store::{FactWriteControl, ProjectMemoryFactProjectionV1};
use tracedecay_usecases::memory::{
    MemoryApplication, ProjectMemoryFactAddRequest, ProjectMemoryFactAddRequestOutcome,
};

use super::DaemonSessionRuntimeRegistryV1;
use crate::daemon::profile_identity;
use crate::store::DatabaseFactStore;

fn enrolled_root(base: &Path, project_id: &ProjectId) -> PathBuf {
    let root = base.join(project_id.as_str());
    std::fs::create_dir_all(&root).expect("project root");
    crate::storage::pin_fixture_repository_identity(&root, project_id.as_str())
        .expect("project enrollment");
    root
}

fn write_control() -> FactWriteControl {
    let commit_granted = Arc::new(AtomicBool::new(false));
    FactWriteControl::new(
        Arc::new(|| false),
        Arc::new(move || {
            commit_granted
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        }),
    )
}

async fn add_fact(database: &crate::db::Database, owner: &FactOwnerV1, label: &str) {
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(database))
        .expect("owner-bound memory application");
    let actor = ActorId::new("actor.graph-shutdown-contract").expect("contract actor identity");
    let preflight = memory
        .preflight_project_memory_fact_add(
            ProjectMemoryFactAddRequest {
                content: format!("{label}: shutdown contract payload with distinct identity"),
                category: FactCategoryV1::Project,
                source_label: Some("graph-shutdown-contract".to_owned()),
                tags: vec![label.to_owned()],
                entities: Vec::new(),
                trust: Some(Confidence::new(0.9).expect("fact trust")),
                metadata: serde_json::json!({"fixture": label}),
            },
            Some(actor),
        )
        .expect("preflight shutdown contract fact");
    let outcome = memory
        .add_preflighted_project_memory_fact(preflight, &write_control())
        .await
        .expect("commit shutdown contract fact");
    let ProjectMemoryFactAddRequestOutcome::Applied(outcome) = outcome else {
        panic!("shutdown contract fact was rejected by the privacy boundary");
    };
    assert!(matches!(
        outcome.fact(),
        ProjectMemoryFactProjectionV1::Available(_)
    ));
}

async fn wait_for_reconciliation(database: &crate::db::Database) {
    let owner = database
        .memory_graph_reconciliation_task_owner()
        .expect("mounted graph reconciliation owner");
    for _ in 0..4_096 {
        if let Ok(reservation) = owner.reserve_retirement() {
            drop(reservation);
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("mounted graph reconciliation did not settle");
}

#[tokio::test]
async fn healthy_shutdown_joins_workers_then_closes_every_retained_graph_without_conflict() {
    let temp = TempDir::new().expect("shutdown contract fixture root");
    let profile_root = temp.path().join("profile");
    let project_id = ProjectId::new("project.graph-shutdown-contract").expect("project id");
    let project_root = enrolled_root(temp.path(), &project_id);
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 53, "graph shutdown contract")
            .expect("daemon database scope");

    let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("daemon registry");

    // Mount every retained graph flavor the daemon holds at shutdown: the
    // profile and project memory graphs (standing owner attachments in the
    // registry maps) and the profile and project session relation graphs.
    let project_memory = registry
        .project_memory(project_id.clone(), [project_root.clone()])
        .await
        .expect("project memory authority");
    let profile_memory = registry
        .profile_memory()
        .await
        .expect("profile memory authority");
    let profile_sessions = registry
        .profile_sessions()
        .await
        .expect("profile sessions authority");
    let project_sessions = registry
        .project_sessions(project_id.clone(), [project_root])
        .await
        .expect("project sessions authority");
    // Exercise a real reconciliation pass so the shutdown below joins a
    // worker that actually published into the memory graph.
    add_fact(
        &project_memory,
        &FactOwnerV1::Project { project_id },
        "shutdown-contract-fact",
    )
    .await;
    wait_for_reconciliation(&project_memory).await;

    // The daemon's terminal shutdown owner ordering under test: cancel, join
    // the workers while their runtimes are alive, then drain the retained
    // owners and close the graphs.
    registry.cancel_memory_graph_reconciliation_tasks();
    registry
        .shutdown_memory_graph_reconciliation_tasks()
        .await
        .expect("reconciliation workers join cleanly on a healthy shutdown");

    // The daemon holds no session leases when the terminal owner runs; the
    // graph clients bound into these leases must drop before the close.
    drop((
        project_memory,
        profile_memory,
        profile_sessions,
        project_sessions,
    ));
    registry
        .close_retained_graph_runtimes_for_shutdown()
        .await
        .expect("retained graph runtimes close without a shutdown Conflict");

    // The drain is exhaustive: a second pass finds nothing left to close.
    registry
        .close_retained_graph_runtimes_for_shutdown()
        .await
        .expect("shutdown close is idempotent after the drain");
}
