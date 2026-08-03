use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphDb, GraphDbLocation, GraphDbOpenOptions, GraphDurability, GraphEntity, GraphEntityId,
    GraphFormatVersion, GraphMutation, GraphNamespace, GraphProjectionId, GraphRelation,
    GraphRelationId, GraphRelationKind, GraphWatermark, GraphWriteBatch, NeverCancelled,
    ProjectionReplacement, SourceGeneration, TraversalRequest,
};

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
    let path = temp.path().join("cross-projection.grafeo");
    let options = || GraphDbOpenOptions {
        location: GraphDbLocation::Persistent(path.clone()),
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Sync,
        cancellation: Arc::new(NeverCancelled),
    };
    let db = GraphDb::open(options()).unwrap();
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
    db.close().unwrap();
    let reopened = GraphDb::open(options()).unwrap();
    assert_eq!(
        visit_identities(reopened.traverse(traversal("source")).unwrap()),
        expected
    );
}
