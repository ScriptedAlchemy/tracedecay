//! The project memory mount is the sole retirable map owner of the project
//! graph shard; the code graph only leases it. When code-graph activation
//! reaches the shard before the mount's background graph open has run, the
//! project must still retire once its clients are idle. Capacity-driven
//! project-server reuse depends on exactly that retirement.

use std::sync::Arc;

use tracedecay_code_index_runtime::CodeGraphReplayBindingV1;
use tracedecay_domain::{CodeGenerationId, ProjectId, RepositoryId, WorktreeId};
use tracedecay_graph_db::SealedGraphStateDigest;

use super::super::DaemonSessionRuntimeRegistryV1;
use tracedecay_daemon_identity::profile_identity;

// A current-thread runtime does not poll the memory-graph-open task the
// mount spawns until the test yields, so the code-graph seat below
// deterministically reaches the unregistered shard first.
#[tokio::test(flavor = "current_thread")]
async fn code_graph_seat_ahead_of_the_memory_graph_open_leaves_the_project_retirable() {
    let temporary = tempfile::tempdir().expect("temporary fixture parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let project_root = root.join("project");
    std::fs::create_dir_all(&project_root).expect("project root");
    let project_id = ProjectId::new("project.code-graph-seat-first").expect("project id");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        project_id.as_str(),
    )
    .expect("project enrollment");
    let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        61,
        "code graph seat ahead of memory graph open",
    )
    .expect("daemon database scope");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");

    let project_database = registry
        .project_memory(project_id.clone(), [project_root])
        .await
        .expect("project memory database");
    let seat = registry
        .retain_code_graph_runtime(
            project_id.clone(),
            RepositoryId::new("repo:seat-first").expect("repository id"),
            WorktreeId::new("worktree:seat-first").expect("worktree id"),
            None,
            CodeGenerationId::new("generation:seat-first").expect("generation id"),
            Arc::clone(&project_database),
            CodeGraphReplayBindingV1 {
                generations_root: project_database
                    .database_path()
                    .with_extension("generations"),
                sealed_state_digest: SealedGraphStateDigest::try_from(format!(
                    "sha256:{}",
                    "7".repeat(64)
                ))
                .expect("synthetic sealed digest"),
            },
        )
        .await
        .expect("code graph seat on the project shard");
    drop(seat);
    drop(project_database);

    registry
        .retire_project_memory_graph(&project_id)
        .await
        .expect("an idle project retires after its code graph seated first");
}
