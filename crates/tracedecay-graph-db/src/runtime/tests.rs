use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Barrier, mpsc};
use std::time::Duration;

use grafeo_common::types::Value;

use super::{GraphDb, require_committed_vector_scalar};
use crate::{
    GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability, GraphEntity, GraphEntityId,
    GraphFormatVersion, GraphMutation, GraphNamespace, GraphProjectionId, GraphProperty,
    GraphPropertyName, GraphTraversalDirection, GraphWatermark, GraphWriteBatch, NeverCancelled,
    SourceGeneration, TraversalRequest,
};

fn memory_db() -> GraphDb {
    GraphDb::open(GraphDbOpenOptions {
        location: GraphDbLocation::Memory,
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Memory,
        cancellation: Arc::new(NeverCancelled),
    })
    .unwrap()
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
    db.apply(scalar_batch("before")).unwrap();
    let snapshot = db.snapshot().unwrap();
    let writer_db = db.clone();
    let (sent, received) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        let result = writer_db.apply(scalar_batch("after"));
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
    db.apply(scalar_batch("native")).unwrap();
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
