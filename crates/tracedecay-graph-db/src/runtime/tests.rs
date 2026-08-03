use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Barrier};
use std::time::Duration;

use super::GraphDb;
use crate::{
    GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability, GraphEntityId,
    GraphFormatVersion, GraphNamespace, NeverCancelled, TraversalRequest,
};

#[test]
fn queued_reader_rechecks_postcommit_poison_after_database_lock() {
    let db = GraphDb::open(GraphDbOpenOptions {
        location: GraphDbLocation::Memory,
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Memory,
        cancellation: Arc::new(NeverCancelled),
    })
    .unwrap();
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
