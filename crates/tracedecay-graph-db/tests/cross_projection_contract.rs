use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphEntity, GraphEntityId, GraphIdempotencyKey, GraphMutation, GraphNamespace,
    GraphProjectionId, GraphPublication, GraphPublicationInputDigest, GraphRelation,
    GraphRelationId, GraphRelationKind, GraphTraversalDirection, GraphWatermark, GraphWriteBatch,
    NeverCancelled, ProjectionReplacement, SourceGeneration, TraversalRequest,
};

mod support;

use support::RegisteredGraph;

fn identity(value: &str) -> GraphEntityId {
    GraphEntityId::new(value).unwrap()
}

fn entity(value: &str) -> GraphEntity {
    GraphEntity {
        identity: identity(value),
        labels: BTreeSet::new(),
        properties: BTreeMap::new(),
    }
}

fn relation(identity: &str, from: &str, to: &str) -> GraphRelation {
    GraphRelation {
        identity: GraphRelationId::new(identity).unwrap(),
        from: self::identity(from),
        to: self::identity(to),
        kind: GraphRelationKind::new("refers").unwrap(),
        properties: BTreeMap::new(),
    }
}

fn batch(
    projection: &str,
    generation: &str,
    watermark: &str,
    mutations: Vec<GraphMutation>,
) -> GraphWriteBatch {
    GraphWriteBatch::new(
        GraphNamespace::new("workspace").unwrap(),
        GraphProjectionId::new(projection).unwrap(),
        SourceGeneration::new(generation).unwrap(),
        GraphWatermark::new(watermark).unwrap(),
        mutations,
        Arc::new(NeverCancelled),
    )
    .unwrap()
}

fn traversal(start: &str) -> TraversalRequest {
    TraversalRequest {
        namespace: GraphNamespace::new("workspace").unwrap(),
        start: identity(start),
        relation_kinds: BTreeSet::new(),
        direction: GraphTraversalDirection::Outgoing,
        max_depth: 1,
        max_visits: 2,
        max_results: 2,
        cancellation: Arc::new(NeverCancelled),
    }
}

fn visit_identities(
    result: tracedecay_graph_db::TraversalResult,
) -> Vec<tracedecay_graph_db::GraphEntityId> {
    result
        .visits
        .into_iter()
        .map(|visit| visit.entity)
        .collect()
}

#[test]
fn replacing_entity_owner_preserves_foreign_edge_in_live_snapshot_and_reopen() {
    let temp = TempDir::new().unwrap();
    let (registered, db) = RegisteredGraph::open(temp.path()).unwrap();
    db.apply(batch(
        "facts",
        "facts-g1",
        "facts-w1",
        vec![GraphMutation::UpsertEntity(entity("shared"))],
    ))
    .unwrap();
    db.apply(batch(
        "code",
        "code-g1",
        "code-w1",
        vec![
            GraphMutation::UpsertEntity(entity("source")),
            GraphMutation::UpsertRelation(relation("source-shared", "source", "shared")),
        ],
    ))
    .unwrap();

    db.replace_projection(ProjectionReplacement {
        namespace: GraphNamespace::new("workspace").unwrap(),
        projection: GraphProjectionId::new("facts").unwrap(),
        source_generation: SourceGeneration::new("facts-g2").unwrap(),
        next_watermark: GraphWatermark::new("facts-w2").unwrap(),
        entities: vec![entity("shared")],
        relations: Vec::new(),
        cancellation: Arc::new(NeverCancelled),
    })
    .unwrap();

    let expected = vec![identity("source"), identity("shared")];
    assert_eq!(
        visit_identities(db.traverse(traversal("source")).unwrap()),
        expected
    );
    let snapshot = db.snapshot().unwrap();
    assert_eq!(
        visit_identities(snapshot.traverse(traversal("source")).unwrap()),
        expected
    );
    drop(snapshot);
    drop(db);
    registered.close().unwrap();
    let reopened = registered.reopen().unwrap();
    assert_eq!(
        visit_identities(reopened.traverse(traversal("source")).unwrap()),
        expected
    );
}

#[test]
fn direct_apply_delete_then_upsert_preserves_foreign_edge_through_reopen() {
    let temp = TempDir::new().unwrap();
    let (registered, db) = RegisteredGraph::open(temp.path()).unwrap();
    db.apply(batch(
        "facts",
        "facts-g1",
        "facts-w1",
        vec![GraphMutation::UpsertEntity(entity("shared"))],
    ))
    .unwrap();
    db.apply(batch(
        "code",
        "code-g1",
        "code-w1",
        vec![
            GraphMutation::UpsertEntity(entity("source")),
            GraphMutation::UpsertRelation(relation("source-shared", "source", "shared")),
        ],
    ))
    .unwrap();

    let mut direct_update = batch(
        "facts",
        "facts-g2",
        "facts-w2",
        vec![GraphMutation::UpsertEntity(entity("shared"))],
    );
    direct_update
        .mutations
        .push(GraphMutation::DeleteEntity(identity("shared")));
    db.apply(direct_update).unwrap();

    let expected = vec![identity("source"), identity("shared")];
    assert_eq!(
        visit_identities(db.traverse(traversal("source")).unwrap()),
        expected
    );
    let snapshot = db.snapshot().unwrap();
    assert_eq!(
        visit_identities(snapshot.traverse(traversal("source")).unwrap()),
        expected
    );
    drop(snapshot);
    drop(db);
    registered.close().unwrap();
    let reopened = registered.reopen().unwrap();
    assert_eq!(
        visit_identities(reopened.traverse(traversal("source")).unwrap()),
        expected
    );
}

#[test]
fn publish_delete_then_upsert_preserves_foreign_edge_through_reopen() {
    let temp = TempDir::new().unwrap();
    let (registered, db) = RegisteredGraph::open(temp.path()).unwrap();
    db.apply(batch(
        "facts",
        "facts-g1",
        "facts-w1",
        vec![GraphMutation::UpsertEntity(entity("shared"))],
    ))
    .unwrap();
    db.apply(batch(
        "code",
        "code-g1",
        "code-w1",
        vec![
            GraphMutation::UpsertEntity(entity("source")),
            GraphMutation::UpsertRelation(relation("source-shared", "source", "shared")),
        ],
    ))
    .unwrap();
    let replacement = batch(
        "facts",
        "facts-g2",
        "facts-w2",
        vec![
            GraphMutation::DeleteEntity(identity("shared")),
            GraphMutation::UpsertEntity(entity("shared")),
        ],
    );

    db.publish(GraphPublication {
        namespace: GraphNamespace::new("workspace").unwrap(),
        idempotency_key: GraphIdempotencyKey::new("facts-event-g2").unwrap(),
        input_digest: GraphPublicationInputDigest::new(format!("sha256:{}", "a".repeat(64)))
            .unwrap(),
        source_generation: SourceGeneration::new("facts-g2").unwrap(),
        expected_watermark: Some(GraphWatermark::new("facts-w1").unwrap()),
        next_watermark: GraphWatermark::new("facts-w2").unwrap(),
        batch: replacement,
        cancellation: Arc::new(NeverCancelled),
    })
    .unwrap();

    let expected = vec![identity("source"), identity("shared")];
    assert_eq!(
        visit_identities(db.traverse(traversal("source")).unwrap()),
        expected
    );
    let snapshot = db.snapshot().unwrap();
    assert_eq!(
        visit_identities(snapshot.traverse(traversal("source")).unwrap()),
        expected
    );
    drop(snapshot);
    drop(db);
    registered.close().unwrap();
    let reopened = registered.reopen().unwrap();
    assert_eq!(
        visit_identities(reopened.traverse(traversal("source")).unwrap()),
        expected
    );
}
