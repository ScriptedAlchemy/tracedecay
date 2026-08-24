use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphCancellation, GraphDb, GraphDbError, GraphDbLeaseV1, GraphDbOwner, GraphEntity,
    GraphEntityId, GraphMutation, GraphNamespace, GraphProjectionId, GraphProperty,
    GraphPropertyName, GraphRelation, GraphRelationId, GraphRelationKind, GraphWatermark,
    GraphWriteBatch, NeverCancelled, SourceGeneration,
};

mod support;

use support::RegisteredGraph;

#[derive(Debug)]
struct Cancelled;

impl GraphCancellation for Cancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

fn live() -> Arc<dyn GraphCancellation> {
    Arc::new(NeverCancelled)
}

fn namespace() -> GraphNamespace {
    GraphNamespace::new("point-read").unwrap()
}

fn entity_id(value: &str) -> GraphEntityId {
    GraphEntityId::new(value).unwrap()
}

fn relation_id(value: &str) -> GraphRelationId {
    GraphRelationId::new(value).unwrap()
}

fn entity(value: &str, generation: &str) -> GraphEntity {
    GraphEntity::new(
        entity_id(value),
        BTreeSet::new(),
        BTreeMap::from([(
            GraphPropertyName::new("generation").unwrap(),
            GraphProperty::String(generation.to_owned()),
        )]),
    )
    .unwrap()
}

fn relation(value: &str, from: &str, to: &str) -> GraphRelation {
    GraphRelation::new(
        relation_id(value),
        entity_id(from),
        entity_id(to),
        GraphRelationKind::new("points-to").unwrap(),
        BTreeMap::new(),
    )
    .unwrap()
}

fn open_memory() -> GraphDbLeaseV1 {
    GraphDbOwner::memory(live()).unwrap().issue_lease().unwrap()
}

fn publish(db: &GraphDb, generation: &str) {
    db.apply_unverified(
        GraphWriteBatch::new(
            namespace(),
            GraphProjectionId::new("code").unwrap(),
            SourceGeneration::new(generation).unwrap(),
            GraphWatermark::new(generation).unwrap(),
            vec![
                GraphMutation::UpsertEntity(entity("root", generation)),
                GraphMutation::UpsertEntity(entity("target", generation)),
                GraphMutation::UpsertRelation(relation("edge", "root", "target")),
            ],
            live(),
        )
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn typed_point_reads_preserve_snapshot_and_reopen_identity() {
    let temp = TempDir::new().unwrap();
    let (registered, db) = RegisteredGraph::open_lease(temp.path()).unwrap();
    publish(&db, "generation.one");
    let snapshot = db.snapshot().unwrap();

    let stored = db
        .entity(&namespace(), &entity_id("root"), live())
        .unwrap()
        .unwrap();
    assert_eq!(stored, entity("root", "generation.one"));
    assert_eq!(
        db.relation(&namespace(), &relation_id("edge"), live())
            .unwrap(),
        Some(relation("edge", "root", "target"))
    );
    assert_eq!(
        snapshot
            .entity(&namespace(), &entity_id("root"), live())
            .unwrap(),
        Some(entity("root", "generation.one"))
    );
    assert_eq!(
        snapshot
            .relation(&namespace(), &relation_id("edge"), live())
            .unwrap(),
        Some(relation("edge", "root", "target"))
    );

    drop(snapshot);
    db.apply_unverified(
        GraphWriteBatch::new(
            namespace(),
            GraphProjectionId::new("code").unwrap(),
            SourceGeneration::new("generation.two").unwrap(),
            GraphWatermark::new("generation.two").unwrap(),
            vec![GraphMutation::UpsertEntity(entity(
                "root",
                "generation.two",
            ))],
            live(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        db.entity(&namespace(), &entity_id("root"), live()).unwrap(),
        Some(entity("root", "generation.two"))
    );

    drop(db);
    registered.close().unwrap();
    let reopened = registered.reopen_lease().unwrap();
    assert_eq!(
        reopened
            .entity(&namespace(), &entity_id("root"), live())
            .unwrap(),
        Some(entity("root", "generation.two"))
    );
    assert_eq!(
        reopened
            .relation(&namespace(), &relation_id("edge"), live())
            .unwrap(),
        Some(relation("edge", "root", "target"))
    );
}

#[test]
fn typed_point_reads_honor_cancellation_without_serving_state() {
    let db = open_memory();
    publish(&db, "generation.one");
    let snapshot = db.snapshot().unwrap();
    let cancelled: Arc<dyn GraphCancellation> = Arc::new(Cancelled);

    assert_eq!(
        db.entity(&namespace(), &entity_id("root"), Arc::clone(&cancelled))
            .unwrap_err(),
        GraphDbError::Cancelled
    );
    assert_eq!(
        snapshot
            .relation(&namespace(), &relation_id("edge"), cancelled)
            .unwrap_err(),
        GraphDbError::Cancelled
    );
}
