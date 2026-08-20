use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, mpsc};
use std::time::Duration;

use grafeo_common::types::Value;

use super::require_committed_vector_scalar;
use crate::{
    GraphCommit, GraphDbError, GraphDbLeaseV1, GraphDbLocation, GraphDbOpenOptions, GraphDbOwner,
    GraphDbRuntimeState, GraphDurability, GraphEntity, GraphEntityId, GraphFormatVersion,
    GraphMutation, GraphNamespace, GraphProjectionId, GraphProperty, GraphPropertyName,
    GraphTraversalDirection, GraphVector, GraphWatermark, GraphWriteBatch, NeverCancelled,
    SourceGeneration, TraversalRequest, VectorMetric, mutation,
};

fn memory_db() -> GraphDbLeaseV1 {
    GraphDbOwner::open(GraphDbOpenOptions {
        location: GraphDbLocation::Memory,
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Memory,
        cancellation: Arc::new(NeverCancelled),
    })
    .unwrap()
    .issue_lease()
    .unwrap()
}

#[test]
fn runtime_state_distinguishes_ready_closed_and_durability_uncertain() {
    let ready = memory_db();
    assert_eq!(ready.runtime_state(), GraphDbRuntimeState::Ready);
    ready.close().unwrap();
    assert_eq!(ready.runtime_state(), GraphDbRuntimeState::Closed);

    let uncertain = memory_db();
    uncertain.inner.poisoned.store(true, Ordering::Release);
    assert_eq!(
        uncertain.runtime_state(),
        GraphDbRuntimeState::DurabilityUncertain
    );
}

#[test]
fn owner_close_releases_the_physical_database_after_durability_uncertainty() {
    let owner = GraphDbOwner::open(GraphDbOpenOptions {
        location: GraphDbLocation::Memory,
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Memory,
        cancellation: Arc::new(NeverCancelled),
    })
    .unwrap();
    let handle = owner.issue_lease().unwrap();
    handle.inner.poisoned.store(true, Ordering::Release);

    assert!(matches!(
        owner.close(),
        Err(GraphDbError::DurabilityUncertain { .. })
    ));
    assert!(
        handle
            .inner
            .database
            .read()
            .expect("database lock remains readable")
            .is_none(),
        "owner close must release the physical Grafeo database even after poisoning"
    );
}

#[test]
fn poisoned_database_lock_makes_close_terminally_uncertain() {
    let owner = GraphDbOwner::open(GraphDbOpenOptions {
        location: GraphDbLocation::Memory,
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Memory,
        cancellation: Arc::new(NeverCancelled),
    })
    .unwrap();
    let lease = owner.issue_lease().unwrap();
    let poison = lease.clone();

    assert!(
        std::thread::spawn(move || {
            let _guard = poison.inner.database.write().unwrap();
            panic!("poison the graph database lock");
        })
        .join()
        .is_err()
    );

    assert!(matches!(
        owner.close(),
        Err(GraphDbError::DurabilityUncertain { .. })
    ));
    assert_eq!(
        owner.runtime_state(),
        GraphDbRuntimeState::DurabilityUncertain
    );
    assert!(matches!(
        owner.close(),
        Err(GraphDbError::DurabilityUncertain { .. })
    ));
}

fn scalar_batch(value: &str) -> GraphWriteBatch {
    GraphWriteBatch::new(
        GraphNamespace::new("project").unwrap(),
        GraphProjectionId::new("code").unwrap(),
        SourceGeneration::new(value).unwrap(),
        GraphWatermark::new(value).unwrap(),
        vec![GraphMutation::UpsertEntity(
            GraphEntity::new(
                GraphEntityId::new("a").unwrap(),
                BTreeSet::new(),
                BTreeMap::from([(
                    GraphPropertyName::new("name").unwrap(),
                    GraphProperty::String(value.to_owned()),
                )]),
            )
            .unwrap(),
        )],
        Arc::new(NeverCancelled),
    )
    .unwrap()
}

#[test]
fn queued_reader_rechecks_postcommit_poison_after_database_lock() {
    let db = memory_db();
    let database_guard = db.inner.database.write().unwrap();
    let started = Arc::new(Barrier::new(2));
    let reader_db = db.clone();
    let reader_started = Arc::clone(&started);
    let reader = std::thread::spawn(move || {
        reader_started.wait();
        reader_db.traverse(TraversalRequest {
            namespace: GraphNamespace::new("workspace").unwrap(),
            start: GraphEntityId::new("missing").unwrap(),
            relation_kinds: BTreeSet::new(),
            direction: GraphTraversalDirection::Outgoing,
            max_depth: 1,
            max_visits: 1,
            max_results: 1,
            cancellation: Arc::new(NeverCancelled),
        })
    });
    started.wait();
    std::thread::sleep(Duration::from_millis(25));
    assert!(
        !reader.is_finished(),
        "reader must be queued behind the writer"
    );

    db.inner.poisoned.store(true, Ordering::Release);
    drop(database_guard);

    assert!(matches!(
        reader.join().unwrap(),
        Err(GraphDbError::DurabilityUncertain { .. })
    ));
}

#[test]
fn snapshot_is_a_zero_copy_read_lease_that_blocks_writes_until_drop() {
    let db = memory_db();
    db.apply_unverified(scalar_batch("before")).unwrap();
    let snapshot = db.snapshot().unwrap();
    let writer_db = db.clone();
    let (sent, received) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        let result = writer_db.apply_unverified(scalar_batch("after"));
        sent.send(result).unwrap();
    });

    assert!(
        received.recv_timeout(Duration::from_millis(50)).is_err(),
        "a live snapshot must retain its read lease instead of copying the whole store"
    );
    assert_eq!(
        snapshot
            .entity(
                &GraphNamespace::new("project").unwrap(),
                &GraphEntityId::new("a").unwrap(),
                Arc::new(NeverCancelled),
            )
            .unwrap()
            .unwrap()
            .properties
            .get(&GraphPropertyName::new("name").unwrap()),
        Some(&GraphProperty::String("before".to_owned()))
    );

    drop(snapshot);
    received
        .recv_timeout(Duration::from_secs(1))
        .expect("writer must resume when the snapshot lease is released")
        .unwrap();
    writer.join().unwrap();
}

#[test]
fn open_installs_native_locator_indexes_and_entity_scalars() {
    let db = memory_db();
    db.apply_unverified(scalar_batch("native")).unwrap();
    let database_guard = db.inner.database.read().unwrap();
    let database = database_guard.as_ref().unwrap();
    for property in [
        "__tracedecay_graph_db_entity_key",
        "__tracedecay_graph_db_relation_key",
        "__tracedecay_graph_db_projection_key",
        "__tracedecay_graph_db_publication_key",
    ] {
        assert!(
            database.has_property_index(property),
            "missing native property index {property}"
        );
    }

    let nodes = database.find_nodes_by_property(
        "__tracedecay_graph_db_entity_key",
        &Value::from("70726f6a656374:61"),
    );
    assert_eq!(nodes.len(), 1);
    let node = database.get_node(nodes[0]).unwrap();
    assert_eq!(
        node.get_property("__tracedecay_graph_db_namespace"),
        Some(&Value::from("project"))
    );
    assert_eq!(
        node.get_property("__tracedecay_graph_db_projection"),
        Some(&Value::from("code"))
    );
    assert_eq!(
        node.get_property("__tracedecay_graph_db_entity_id"),
        Some(&Value::from("a"))
    );
    assert!(
        node.get_property("__tracedecay_graph_db_payload").is_none(),
        "typed graph state must not be hidden in a JSON payload"
    );
}

#[test]
fn grafeo_query_mutations_track_conflicting_marker_writes() {
    let database = grafeo_engine::GrafeoDB::new_in_memory();
    database.create_property_index("marker_key");
    database
        .session()
        .create_node_with_props(
            &["Marker"],
            [
                ("marker_key", Value::from("one")),
                ("value", Value::from(0_i64)),
            ],
        )
        .unwrap();
    let mut first = database.session();
    let mut second = database.session();
    first.begin_transaction().unwrap();
    second.begin_transaction().unwrap();
    first
        .execute("MATCH (n:Marker {marker_key: 'one'}) SET n.value = 1")
        .unwrap();
    assert!(
        second
            .execute("MATCH (n:Marker {marker_key: 'one'}) SET n.value = 2")
            .is_err(),
        "the second tracked writer must conflict before changing the marker"
    );
    first.rollback().unwrap();
    second.rollback().unwrap();
}

#[test]
fn vector_index_refresh_requires_the_identical_committed_scalar() {
    let database = grafeo_engine::GrafeoDB::new_in_memory();
    let expected = Value::Vector(vec![1.0_f32, 2.0].into());
    let node = database
        .session()
        .create_node_with_props(&["Vector"], [("embedding", expected.clone())])
        .unwrap();

    assert_eq!(
        require_committed_vector_scalar(&database, node, "embedding", &expected),
        Ok(())
    );
    assert!(matches!(
        require_committed_vector_scalar(
            &database,
            node,
            "embedding",
            &Value::Vector(vec![2.0_f32, 1.0].into()),
        ),
        Err(GraphDbError::DurabilityUncertain { .. })
    ));
}

#[test]
fn snapshots_can_cross_daemon_worker_boundaries() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<super::GraphSnapshot>();
}

fn walsync_db(dir: &tempfile::TempDir) -> GraphDbLeaseV1 {
    GraphDbOwner::open(GraphDbOpenOptions {
        location: GraphDbLocation::Persistent(dir.path().join("graph.grafeo")),
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::WalSync,
        cancellation: Arc::new(NeverCancelled),
    })
    .unwrap()
    .issue_lease()
    .unwrap()
}

fn vector_batch(value: &str) -> GraphWriteBatch {
    GraphWriteBatch::new(
        GraphNamespace::new("project").unwrap(),
        GraphProjectionId::new("code").unwrap(),
        SourceGeneration::new(value).unwrap(),
        GraphWatermark::new(value).unwrap(),
        vec![GraphMutation::UpsertEntity(
            GraphEntity::new(
                GraphEntityId::new("vector-entity").unwrap(),
                BTreeSet::new(),
                BTreeMap::from([(
                    GraphPropertyName::new("embedding").unwrap(),
                    GraphProperty::Vector(
                        GraphVector::new(vec![1.0, 2.0], 2, VectorMetric::Cosine).unwrap(),
                    ),
                )]),
            )
            .unwrap(),
        )],
        Arc::new(NeverCancelled),
    )
    .unwrap()
}

fn apply_vector_batch_with_check(
    db: &GraphDbLeaseV1,
    value: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<GraphCommit, GraphDbError> {
    let mut batch = vector_batch(value);
    let digest = batch.validate_and_digest().unwrap();
    let _snapshot_gate = db.inner.snapshot_gate.write();
    let guard = db.write_guard().unwrap();
    let database = guard.as_ref().unwrap();
    let mut state = db.state_write_guard().unwrap();
    db.apply_locked(
        database,
        &mut state,
        batch,
        mutation::CommitMetadata::for_digest(digest),
        &mutation::RelationEndpointNamespaces::new(),
        check,
    )
}

fn committed_entity_present(db: &GraphDbLeaseV1) -> bool {
    db.snapshot()
        .unwrap()
        .entity(
            &GraphNamespace::new("project").unwrap(),
            &GraphEntityId::new("vector-entity").unwrap(),
            Arc::new(NeverCancelled),
        )
        .unwrap()
        .is_some()
}

/// A threaded deadline may expire at any observation point in the write path.
/// Sweeping every expiry point proves the durability contract: cancellation
/// may only surface while the Grafeo transaction is still uncommitted (rolled
/// back, nothing durable); once the transaction commits, the apply must settle
/// HNSW refresh and WAL sync and report the commit. A committed write reported
/// as `Cancelled`/`DeadlineExceeded` on a `Ready` handle is the F-class defect:
/// replay short-circuits on the committed publication record and the skipped
/// settlement is never repaired.
#[test]
fn deadline_expiry_after_commit_settles_the_write_instead_of_cancelling() {
    // Calibration: count every cancellation observation one full apply makes.
    let total_checks = {
        let dir = tempfile::tempdir().unwrap();
        let db = walsync_db(&dir);
        let observations = AtomicUsize::new(0);
        apply_vector_batch_with_check(&db, "calibrate", &|| {
            observations.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
        .expect("unrestricted apply must commit");
        observations.into_inner()
    };
    assert!(total_checks > 0, "write path must observe cancellation");

    // Sweep: expire the deadline at the k-th observation on a fresh store.
    for expiry_point in 1..=total_checks + 1 {
        let dir = tempfile::tempdir().unwrap();
        let db = walsync_db(&dir);
        let observations = AtomicUsize::new(0);
        let result = apply_vector_batch_with_check(&db, "window", &|| {
            if observations.fetch_add(1, Ordering::Relaxed) + 1 >= expiry_point {
                Err(GraphDbError::DeadlineExceeded)
            } else {
                Ok(())
            }
        });

        let committed = committed_entity_present(&db);
        match result {
            Ok(_) => {
                assert!(
                    committed,
                    "expiry point {expiry_point}: reported commit is not readable"
                );
                assert_eq!(
                    db.runtime_state(),
                    GraphDbRuntimeState::Ready,
                    "expiry point {expiry_point}: settled commit left a non-ready handle"
                );
            }
            Err(GraphDbError::DeadlineExceeded | GraphDbError::Cancelled) => {
                assert!(
                    !committed,
                    "expiry point {expiry_point}: durably committed write was mistyped as \
                     cancelled, stranding unsynced WAL and stale HNSW state on a Ready handle"
                );
                assert_eq!(
                    db.runtime_state(),
                    GraphDbRuntimeState::Ready,
                    "expiry point {expiry_point}: pre-commit cancellation must roll back cleanly"
                );
            }
            Err(other) => {
                panic!("expiry point {expiry_point}: unexpected write outcome {other:?}")
            }
        }
    }
}
