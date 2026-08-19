//! Direct persistent-activation coverage for sealed code generations.
//!
//! Production code-index activation flows through
//! [`super::RetainedCodeGraphRuntimeV1::publish_verified_snapshot`], while the
//! scheduler test registry mounts the in-memory activation authority, so this
//! path had no direct regression. The live failure it now covers: every
//! sealed publication answered `code graph database conflict` immediately
//! after its own journal append, the retained-seat and reconcile retries
//! looped on the same conflict, and the served census stayed
//! `exact_scope_generation_not_ready` until the store was reset.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tracedecay_domain::ProjectId;
use tracedecay_graph_db::GraphProjectorRevision;
use tracedecay_usecases::retention::code_index_generations::DurablePublicationPointerV1;

use super::super::DaemonSessionRuntimeRegistryV1;
use crate::daemon::code_index_scheduler::{
    CodeGraphReplayBindingV1, CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1,
    scoped_code_index_store_root,
};
use crate::daemon::profile_identity;

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git fixture command");
    assert!(
        output.status.success(),
        "git fixture command failed: {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sealed_generation_publishes_and_republishes_as_the_verified_code_graph() {
    let temporary = tempfile::tempdir().expect("temporary fixture parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let project_root = root.join("project");
    std::fs::create_dir_all(project_root.join("src")).expect("project source directory");
    git(&project_root, &["init", "-q", "-b", "main"]);
    git(&project_root, &["config", "user.name", "TraceDecay Test"]);
    git(
        &project_root,
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    std::fs::write(
        project_root.join("src/lib.rs"),
        "pub fn sealed_publication_value() -> usize { 41 }\n",
    )
    .expect("project source");
    git(&project_root, &["add", "."]);
    git(&project_root, &["commit", "-qm", "sealed fixture"]);
    let project_id = ProjectId::new("project.sealed-code-publication").expect("project id");
    crate::storage::pin_fixture_repository_identity(&project_root, project_id.as_str())
        .expect("project enrollment");
    let canonical_project = project_root.canonicalize().expect("canonical project root");

    // Seal one real generation through the production worktree scheduler.
    let store_root = root.join("code-index-store");
    let scoped_store = scoped_code_index_store_root(&store_root, &canonical_project);
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        project_id.clone(),
        &canonical_project,
        scoped_store.clone(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open worktree scheduler");
    scheduler.reconcile_now().expect("seal the generation");
    let latest = scheduler.latest_complete().expect("complete generation");
    let repository_id = latest.generation().snapshot().repository.clone();
    let reference = latest.generation().snapshot().reference.clone();
    let worktree_id = scheduler.identity().worktree_id().clone();
    let generation_id = latest.generation().manifest().generation_id.clone();
    drop(scheduler);
    let pointer: DurablePublicationPointerV1 = serde_json::from_slice(
        &std::fs::read(scoped_store.join("active-code-generation-v1.json"))
            .expect("active generation pointer"),
    )
    .expect("decode active generation pointer");
    assert_eq!(pointer.generation_id, generation_id.as_str());
    let replay_binding = || CodeGraphReplayBindingV1 {
        generations_root: scoped_store.join("code-generations-v1"),
        sealed_state_digest: tracedecay_graph_db::SealedGraphStateDigest::try_from(
            pointer.state_digest.clone(),
        )
        .expect("sealed state digest"),
    };

    let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 43, "sealed code publication")
            .expect("daemon database scope");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    let project_database = registry
        .project_memory(project_id.clone(), [canonical_project.clone()])
        .await
        .expect("project graph database");

    let runtime = registry
        .retain_code_graph_runtime(
            project_id.clone(),
            repository_id.clone(),
            worktree_id.clone(),
            reference.clone(),
            generation_id.clone(),
            Arc::clone(&project_database),
            replay_binding(),
        )
        .await
        .expect("retain code graph runtime");
    let snapshot = runtime
        .publish_verified_snapshot(latest.generation(), Arc::new(AtomicBool::new(false)))
        .expect("the sealed generation must publish as the verified code graph");
    let expected_generation = tracedecay_code_index::graph_projection::code_graph_generation_id(
        &generation_id,
        &GraphProjectorRevision::try_from(
            tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
        )
        .expect("projector revision"),
    )
    .expect("code graph generation id");
    assert_eq!(snapshot.generation(), &expected_generation);
    let head = snapshot.verified_head().clone();
    drop(snapshot);

    // A retained-seat retry of the same sealed artifact resumes to the same
    // verified head instead of conflicting with its own journaled replay.
    let retried = registry
        .retain_code_graph_runtime(
            project_id,
            repository_id,
            worktree_id,
            reference,
            generation_id,
            Arc::clone(&project_database),
            replay_binding(),
        )
        .await
        .expect("retain code graph runtime again");
    let resumed = retried
        .publish_verified_snapshot(latest.generation(), Arc::new(AtomicBool::new(false)))
        .expect("a repeated activation must resume the exact publication");
    assert_eq!(resumed.generation(), &expected_generation);
    assert_eq!(resumed.verified_head(), &head);
}
