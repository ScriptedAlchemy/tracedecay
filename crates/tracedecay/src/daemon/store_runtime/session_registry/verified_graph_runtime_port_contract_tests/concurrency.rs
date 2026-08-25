use std::sync::{Arc, Barrier};

use tracedecay_graph_db::{GraphDbError, GraphGenerationManifest, VerifiedGraphSnapshot};

use super::{
    ContractFixture, key, manifest, project_id, projection, reconcile_through_trait,
    snapshot_through_trait,
};

fn reconcile_pair(
    database: Arc<crate::db::Database>,
    left: GraphGenerationManifest,
    right: GraphGenerationManifest,
    publication_key: &str,
) -> [Result<VerifiedGraphSnapshot, GraphDbError>; 2] {
    std::thread::scope(|scope| {
        let barrier = Arc::new(Barrier::new(3));
        let left_database = Arc::clone(&database);
        let left_barrier = Arc::clone(&barrier);
        let left_key = publication_key.to_owned();
        let left = scope.spawn(move || {
            left_barrier.wait();
            let operation = left_database
                .issue_memory_graph_runtime_operation()
                .expect("mounted left graph operation");
            reconcile_through_trait(operation.runtime(), &left, key(&left_key))
        });
        let right_database = Arc::clone(&database);
        let right_barrier = Arc::clone(&barrier);
        let right_key = publication_key.to_owned();
        let right = scope.spawn(move || {
            right_barrier.wait();
            let operation = right_database
                .issue_memory_graph_runtime_operation()
                .expect("mounted right graph operation");
            reconcile_through_trait(operation.runtime(), &right, key(&right_key))
        });
        barrier.wait();
        [
            left.join().expect("left graph publication thread"),
            right.join().expect("right graph publication thread"),
        ]
    })
}

fn assert_concurrent_replay_and_conflict(database: Arc<crate::db::Database>, scope_label: &str) {
    let replay_projection = projection(&format!("{scope_label}-concurrent-replay"));
    let replay_manifest = manifest(&replay_projection, "concurrent-replay", "1");
    let [first, second] = reconcile_pair(
        Arc::clone(&database),
        replay_manifest.clone(),
        replay_manifest,
        &format!("{scope_label}-concurrent-replay"),
    );
    let first = first.expect("first exact concurrent publication");
    let second = second.expect("second exact concurrent replay");
    assert_eq!(first.verified_head(), second.verified_head());

    let conflict_projection = projection(&format!("{scope_label}-concurrent-conflict"));
    let left_manifest = manifest(&conflict_projection, "concurrent-conflict", "left");
    let right_manifest = manifest(&conflict_projection, "concurrent-conflict", "right");
    let results = reconcile_pair(
        Arc::clone(&database),
        left_manifest,
        right_manifest,
        &format!("{scope_label}-concurrent-conflict"),
    );
    let winners = results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .collect::<Vec<_>>();
    let conflicts = results
        .iter()
        .filter(|result| matches!(result, Err(GraphDbError::Conflict)))
        .count();
    assert_eq!(winners.len(), 1, "exactly one changed input must win");
    assert_eq!(conflicts, 1, "the losing changed input must conflict");
    let operation = database
        .issue_memory_graph_runtime_operation()
        .expect("mounted graph snapshot operation");
    let retained = snapshot_through_trait(operation.runtime(), &conflict_projection)
        .expect("snapshot after concurrent changed-input conflict")
        .expect("winning concurrent verified head");
    assert_eq!(retained.verified_head(), winners[0].verified_head());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_and_profile_ports_serialize_exact_replay_and_changed_input_conflicts() {
    let fixture = ContractFixture::new("concurrent-publication").await;
    let project_id = project_id("concurrent-publication");
    let (project_database, _sessions) = fixture.mount_unbound(&project_id).await;
    let profile_database = fixture
        .registry
        .profile_memory()
        .await
        .expect("profile memory database");
    assert_concurrent_replay_and_conflict(project_database, "project");
    assert_concurrent_replay_and_conflict(profile_database, "profile");
}
