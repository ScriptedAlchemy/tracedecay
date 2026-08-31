//! The staging database's apply-state release: the pruning a completed
//! publication performs so a quarantine-recovery replay converges without
//! being OOM-killed into it.
//!
//! Bulk staging accumulates MVCC version chains and transaction metadata
//! beside the live rows, and nothing pruned them within an incarnation: the
//! engine's version GC runs only when called, freed nodes then sit in
//! retained glibc arenas, and the kernel keeps counting them as anon RSS.
//! Before the release existed, only a process restart reclaimed that
//! overhead. These tests pin the properties that make the in-place release
//! sound:
//!
//! 1. the release preserves serving: a verified generation published before
//!    the release still recovers and answers reads afterwards;
//! 2. below the applied-mutation budget a publication never pays the GC
//!    walk, and the counter is untouched;
//! 3. the production trigger is the budget: a publication crossing it
//!    releases inside the ordinary `publish_verified` path and resets the
//!    counter.

use super::*;

/// The trigger constant mirrored from
/// `limits::GRAPH_APPLY_STATE_RELEASE_MUTATIONS` (crate-private on purpose;
/// drift fails the threshold assertions below).
const RELEASE_MUTATION_BUDGET: u64 = 1_000_000;

struct PublishedOpen {
    temp: TempDir,
    registered: RegisteredGraph,
    authority: RelationalAuthority,
    key: GraphPublicationKeyV1,
}

/// Publishes one generation into a fresh persistent store and keeps it
/// mounted, so a test can operate on the live handle.
fn publish_one_open(namespace: &str) -> PublishedOpen {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection(namespace, "work");
    let generation = manifest(identity, "g1", "g1", vec![], vec![]);
    let record = stage_manifest(
        &mut authority,
        &registered.binding,
        &generation,
        "publish:g1",
        None,
        'd',
    );
    registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key,
            None,
        )
        .unwrap();
    let key = record.publication.key.clone();
    PublishedOpen {
        temp,
        registered,
        authority,
        key,
    }
}

fn resolve(published: &PublishedOpen) -> tracedecay_graph_db::GraphDbLeaseV1 {
    published
        .registered
        .registry
        .resolve(registration(
            published.registered.binding.clone(),
            published.temp.path(),
        ))
        .unwrap()
}

fn recover(published: &mut PublishedOpen) -> tracedecay_graph_db::VerifiedGraphSnapshot {
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    published
        .registered
        .registry
        .recover_verified_snapshot(
            registration(published.registered.binding.clone(), published.temp.path()),
            &mut published.authority,
            &context,
            &published.key.projection,
        )
        .unwrap()
}

fn shared_entity_marker(
    snapshot: &tracedecay_graph_db::VerifiedGraphSnapshot,
    namespace: &str,
) -> String {
    let entity = snapshot
        .entity(
            &GraphEntityRef {
                projection: projection(namespace, "work"),
                identity: GraphEntityId::new("entity:shared").unwrap(),
            },
            std::sync::Arc::new(TestCancellation),
        )
        .unwrap()
        .expect("the published entity must answer after a release");
    match entity
        .properties
        .get(&GraphPropertyName::new("marker").unwrap())
        .expect("the published entity keeps its marker property")
    {
        GraphProperty::String(marker) => marker.clone(),
        other => panic!("marker must remain a string property: {other:?}"),
    }
}

/// The release is transparent to a live handle: the publication's rows, its
/// verified recovery, and its reads all survive the in-place GC, and the
/// applied-mutation counter returns to zero.
#[test]
fn a_release_preserves_serving_on_the_live_handle() {
    let namespace = "release:serving";
    let mut published = publish_one_open(namespace);
    let graph = resolve(&published);
    assert!(
        graph.applied_mutations_since_release() > 0,
        "publication staging must be counted toward the release budget"
    );

    // Below the budget nothing releases and the counter is untouched.
    let before = graph.applied_mutations_since_release();
    assert!(!graph.maybe_release_apply_state(&|| Ok(())).unwrap());
    assert_eq!(graph.applied_mutations_since_release(), before);

    assert!(
        graph.release_apply_state(&|| Ok(())).unwrap(),
        "a live staging store must release on demand"
    );
    assert_eq!(
        graph.applied_mutations_since_release(),
        0,
        "a release resets the applied-mutation budget"
    );

    let snapshot = recover(&mut published);
    assert_eq!(shared_entity_marker(&snapshot, namespace), "g1");
    drop(snapshot);
    drop(graph);
    assert!(published.registered.close().unwrap());
}

/// A released store still closes durably and remounts: nothing about the GC
/// or the arena trim invalidates the container, the markers, or recovery.
#[test]
fn a_remount_after_a_release_recovers_and_serves() {
    let namespace = "release:remount";
    let mut published = publish_one_open(namespace);
    let graph = resolve(&published);
    assert!(graph.release_apply_state(&|| Ok(())).unwrap());
    drop(graph);
    assert!(published.registered.close().unwrap());

    published.registered.mount().unwrap();
    let snapshot = recover(&mut published);
    assert_eq!(shared_entity_marker(&snapshot, namespace), "g1");
}

/// The production trigger: a publication that crosses the applied-mutation
/// budget releases inside `publish_verified` itself and resets the counter;
/// serving continues on the released store.
#[test]
fn a_publication_at_the_mutation_budget_releases_and_resets() {
    let namespace = "release:budget";
    let mut published = publish_one_open(namespace);
    let graph = resolve(&published);

    // Pretend the incarnation already staged a corpus's worth of records,
    // the state a quarantine-recovery replay reaches after one scope.
    graph.force_applied_mutations_for_test(RELEASE_MUTATION_BUDGET);

    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let identity = projection(namespace, "work");
    let generation = manifest(identity, "g2", "g2", vec![], vec![]);
    let head = published
        .authority
        .heads
        .get(&published.key.projection)
        .cloned();
    let record = stage_manifest(
        &mut published.authority,
        &published.registered.binding,
        &generation,
        "publish:g2",
        head,
        'e',
    );
    published
        .registered
        .registry
        .publish_verified(
            registration(published.registered.binding.clone(), published.temp.path()),
            &mut published.authority,
            &context,
            &record.publication.key,
            None,
        )
        .unwrap();
    assert_eq!(
        graph.applied_mutations_since_release(),
        0,
        "crossing the budget must release inside the publication path"
    );

    published.key = record.publication.key.clone();
    let snapshot = recover(&mut published);
    assert_eq!(shared_entity_marker(&snapshot, namespace), "g2");
}
