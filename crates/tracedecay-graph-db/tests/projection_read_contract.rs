use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tracedecay_graph_db::{
    GraphCancellation, GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability,
    GraphEntity, GraphEntityId, GraphFormatVersion, GraphMutation, GraphNamespace,
    GraphProjectionId, GraphProjectionReadRequest, GraphProjectionTelemetryRequest, GraphRelation,
    GraphRelationId, GraphRelationKind, GraphWatermark, GraphWriteBatch, NeverCancelled,
    SourceGeneration,
};

fn cancellation() -> Arc<dyn GraphCancellation> {
    Arc::new(NeverCancelled)
}

#[derive(Debug)]
struct Cancelled;

impl GraphCancellation for Cancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

fn memory_db() -> GraphDb {
    GraphDb::open(GraphDbOpenOptions {
        location: GraphDbLocation::Memory,
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Memory,
        cancellation: cancellation(),
    })
    .unwrap()
}

fn entity(identity: &str) -> GraphEntity {
    GraphEntity::new(
        GraphEntityId::new(identity).unwrap(),
        BTreeSet::new(),
        BTreeMap::new(),
    )
    .unwrap()
}

fn relation(identity: &str, from: &str, to: &str) -> GraphRelation {
    GraphRelation::new(
        GraphRelationId::new(identity).unwrap(),
        GraphEntityId::new(from).unwrap(),
        GraphEntityId::new(to).unwrap(),
        GraphRelationKind::new("depends-on").unwrap(),
        BTreeMap::new(),
    )
    .unwrap()
}

fn publish(db: &GraphDb, projection: &str, identities: &[&str]) {
    let mut mutations = identities
        .iter()
        .map(|identity| GraphMutation::UpsertEntity(entity(identity)))
        .collect::<Vec<_>>();
    if identities.len() >= 2 {
        mutations.push(GraphMutation::UpsertRelation(relation(
            &format!("{projection}-edge"),
            identities[0],
            identities[1],
        )));
    }
    db.apply(
        GraphWriteBatch::new(
            GraphNamespace::new("project").unwrap(),
            GraphProjectionId::new(projection).unwrap(),
            SourceGeneration::new(format!("{projection}-generation")).unwrap(),
            GraphWatermark::new(format!("{projection}-watermark")).unwrap(),
            mutations,
            cancellation(),
        )
        .unwrap(),
    )
    .unwrap();
}

fn request(projection: &str) -> GraphProjectionReadRequest {
    GraphProjectionReadRequest {
        namespace: GraphNamespace::new("project").unwrap(),
        projection: GraphProjectionId::new(projection).unwrap(),
        after_entity: None,
        after_relation: None,
        max_entities: 2,
        max_relations: 1,
        cancellation: cancellation(),
    }
}

#[test]
fn projection_reads_are_filtered_bounded_deterministic_and_counted() {
    let db = memory_db();
    publish(&db, "code-one", &["code-c", "code-a", "code-b"]);
    publish(&db, "code-two", &["other-a", "other-b"]);

    let telemetry = db
        .projection_telemetry(GraphProjectionTelemetryRequest {
            namespace: GraphNamespace::new("project").unwrap(),
            projection: GraphProjectionId::new("code-one").unwrap(),
            cancellation: cancellation(),
        })
        .unwrap()
        .expect("published projection telemetry");
    assert_eq!(telemetry.source_generation.as_str(), "code-one-generation");
    assert_eq!(telemetry.watermark.as_str(), "code-one-watermark");
    assert_eq!(telemetry.entity_count, 3);
    assert_eq!(telemetry.relation_count, 1);
    assert!(
        db.projection_telemetry(GraphProjectionTelemetryRequest {
            namespace: GraphNamespace::new("project").unwrap(),
            projection: GraphProjectionId::new("missing").unwrap(),
            cancellation: cancellation(),
        })
        .unwrap()
        .is_none()
    );

    let first = db.read_projection(request("code-one")).unwrap();
    assert_eq!(
        first
            .entities
            .iter()
            .map(|entity| entity.identity.as_str())
            .collect::<Vec<_>>(),
        vec!["code-a", "code-b"]
    );
    assert_eq!(
        first
            .relations
            .iter()
            .map(|relation| relation.identity.as_str())
            .collect::<Vec<_>>(),
        vec!["code-one-edge"]
    );
    assert_eq!(
        first.next_entity.as_ref().map(GraphEntityId::as_str),
        Some("code-b")
    );
    assert_eq!(first.next_relation, None);

    let mut second_request = request("code-one");
    second_request.after_entity = first.next_entity;
    let second = db.read_projection(second_request).unwrap();
    assert_eq!(
        second
            .entities
            .iter()
            .map(|entity| entity.identity.as_str())
            .collect::<Vec<_>>(),
        vec!["code-c"]
    );
    assert_eq!(second.next_entity, None);
}

#[test]
fn projection_read_authenticates_cursors_and_enforces_budgets() {
    let db = memory_db();
    publish(&db, "code-one", &["code-a", "code-b"]);
    publish(&db, "code-two", &["other-a", "other-b"]);

    let mut foreign_entity = request("code-one");
    foreign_entity.after_entity = Some(GraphEntityId::new("other-a").unwrap());
    assert!(matches!(
        db.read_projection(foreign_entity),
        Err(GraphDbError::InvalidRequest { .. })
    ));

    let mut foreign_relation = request("code-one");
    foreign_relation.after_relation = Some(GraphRelationId::new("code-two-edge").unwrap());
    assert!(matches!(
        db.read_projection(foreign_relation),
        Err(GraphDbError::InvalidRequest { .. })
    ));

    let mut empty = request("code-one");
    empty.max_entities = 0;
    empty.max_relations = 0;
    assert_eq!(
        db.read_projection(empty).unwrap_err(),
        GraphDbError::BudgetExhausted
    );

    let mut oversized = request("code-one");
    oversized.max_entities = 100_001;
    assert_eq!(
        db.read_projection(oversized).unwrap_err(),
        GraphDbError::BudgetExhausted
    );

    let mut cancelled = request("code-one");
    cancelled.cancellation = Arc::new(Cancelled);
    assert_eq!(
        db.read_projection(cancelled).unwrap_err(),
        GraphDbError::Cancelled
    );
}

#[test]
fn snapshot_projection_reads_share_the_native_read_lease() {
    let db = memory_db();
    publish(&db, "code", &["code-a", "code-b"]);
    let snapshot = db.snapshot().unwrap();
    let page = snapshot.read_projection(request("code")).unwrap();
    assert_eq!(page.entities.len(), 2);
    let telemetry = snapshot
        .projection_telemetry(GraphProjectionTelemetryRequest {
            namespace: GraphNamespace::new("project").unwrap(),
            projection: GraphProjectionId::new("code").unwrap(),
            cancellation: cancellation(),
        })
        .unwrap()
        .expect("published projection telemetry");
    assert_eq!(telemetry.entity_count, 2);
}
