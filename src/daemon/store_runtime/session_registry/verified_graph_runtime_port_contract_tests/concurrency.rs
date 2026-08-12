use std::sync::{Arc, Barrier};

use tracedecay_graph_db::{GraphDbError, GraphGenerationManifest, VerifiedGraphSnapshot};

use super::{
    ContractFixture, key, manifest, project_id, projection, reconcile_through_trait,
    snapshot_through_trait,
};
use crate::global_db::VerifiedGraphRuntimePortV1;

fn reconcile_pair(
    port: Arc<dyn VerifiedGraphRuntimePortV1>,
    left: GraphGenerationManifest,
    right: GraphGenerationManifest,
    publication_key: &str,
) -> [Result<VerifiedGraphSnapshot, GraphDbError>; 2] {
    std::thread::scope(|scope| {
        let barrier = Arc::new(Barrier::new(3));
        let left_port = Arc::clone(&port);
        let left_barrier = Arc::clone(&barrier);
        let left_key = publication_key.to_owned();
        let left = scope.spawn(move || {
            left_barrier.wait();
            reconcile_through_trait(left_port.as_ref(), &left, key(&left_key))
        });
        let right_port = Arc::clone(&port);
        let right_barrier = Arc::clone(&barrier);
        let right_key = publication_key.to_owned();
        let right = scope.spawn(move || {
            right_barrier.wait();
            reconcile_through_trait(right_port.as_ref(), &right, key(&right_key))
        });
        barrier.wait();
        [
            left.join().expect("left graph publication thread"),
            right.join().expect("right graph publication thread"),
        ]
    })
}

fn assert_concurrent_replay_and_conflict(
    port: Arc<dyn VerifiedGraphRuntimePortV1>,
    scope_label: &str,
) {
    let replay_projection = projection(&format!("{scope_label}-concurrent-replay"));
    let replay_manifest = manifest(&replay_projection, "concurrent-replay", "1");
    let [first, second] = reconcile_pair(
        Arc::clone(&port),
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
        Arc::clone(&port),
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
    let retained = snapshot_through_trait(port.as_ref(), &conflict_projection)
        .expect("snapshot after concurrent changed-input conflict")
        .expect("winning concurrent verified head");
    assert_eq!(retained.verified_head(), winners[0].verified_head());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_and_profile_ports_serialize_exact_replay_and_changed_input_conflicts() {
    let fixture = ContractFixture::new("concurrent-publication").await;
    let project_id = project_id("concurrent-publication");
    let (_, _, project_port) = fixture.bind(&project_id).await;
    let profile_database = fixture
        .registry
        .profile_memory()
        .await
        .expect("profile memory database");
    let profile_port = profile_database
        .memory_graph_runtime()
        .expect("profile memory graph runtime");

    assert_concurrent_replay_and_conflict(project_port, "project");
    assert_concurrent_replay_and_conflict(profile_port, "profile");
}
