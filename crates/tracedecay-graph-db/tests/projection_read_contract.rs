use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphDb, GraphDbLocation, GraphDbOpenOptions, GraphDurability, GraphEntity, GraphEntityId,
    GraphFormatVersion, GraphMutation, GraphNamespace, GraphProjectionId,
    GraphProjectionReadRequest, GraphProjectionTelemetryRequest, GraphProperty, GraphPropertyName,
    GraphRelation, GraphRelationId, GraphRelationKind, GraphTraversalDirection, GraphVector,
    GraphVectorIndexRequest, GraphVectorIndexStatus, GraphWatermark, GraphWriteBatch,
    NeverCancelled, SourceGeneration, TraversalRequest, VectorMetric, VectorSearchRequest,
};

fn cancellation() -> Arc<dyn tracedecay_graph_db::GraphCancellation> {
    Arc::new(NeverCancelled)
}

fn vector_entity(identity: &str, values: Vec<f32>) -> GraphEntity {
    GraphEntity::new(
        GraphEntityId::new(identity).unwrap(),
        BTreeSet::new(),
        BTreeMap::from([(
            GraphPropertyName::new("embedding").unwrap(),
            GraphProperty::Vector(GraphVector::new(values, 2, VectorMetric::Euclidean).unwrap()),
        )]),
    )
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

#[test]
fn projection_reads_are_filtered_bounded_deterministic_and_snapshot_isolated() {
    let db = GraphDb::open(GraphDbOpenOptions {
        location: GraphDbLocation::Memory,
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Memory,
        cancellation: cancellation(),
    })
    .unwrap();
    publish(&db, "code-one", &["code-a", "code-b", "code-c"]);
    publish(&db, "code-two", &["other-a", "other-b"]);
    let snapshot = db.snapshot().unwrap();
    let telemetry = snapshot
        .projection_telemetry(GraphProjectionTelemetryRequest {
            namespace: GraphNamespace::new("project").unwrap(),
            projection: GraphProjectionId::new("code-one").unwrap(),
            cancellation: cancellation(),
        })
        .unwrap()
        .expect("published projection telemetry");
    assert_eq!(telemetry.source_generation.as_str(), "code-one-generation");
    assert_eq!(telemetry.entity_count, 3);
    assert_eq!(telemetry.relation_count, 1);
    assert!(
        snapshot
            .projection_telemetry(GraphProjectionTelemetryRequest {
                namespace: GraphNamespace::new("project").unwrap(),
                projection: GraphProjectionId::new("missing").unwrap(),
                cancellation: cancellation(),
            })
            .unwrap()
            .is_none()
    );

    let first = snapshot
        .read_projection(GraphProjectionReadRequest {
            namespace: GraphNamespace::new("project").unwrap(),
            projection: GraphProjectionId::new("code-one").unwrap(),
            after_entity: None,
            after_relation: None,
            max_entities: 2,
            max_relations: 1,
            cancellation: cancellation(),
        })
        .unwrap();
    assert_eq!(
        first
            .entities
            .iter()
            .map(|entity| entity.identity.as_str())
            .collect::<Vec<_>>(),
        vec!["code-a", "code-b"]
    );
    assert_eq!(first.relations.len(), 1);
    assert_eq!(
        first.next_entity.as_ref().map(GraphEntityId::as_str),
        Some("code-b")
    );
    assert_eq!(first.next_relation, None);

    db.apply(
        GraphWriteBatch::new(
            GraphNamespace::new("project").unwrap(),
            GraphProjectionId::new("code-one").unwrap(),
            SourceGeneration::new("code-one-generation-2").unwrap(),
            GraphWatermark::new("code-one-watermark-2").unwrap(),
            vec![GraphMutation::UpsertEntity(entity("code-d"))],
            cancellation(),
        )
        .unwrap(),
    )
    .unwrap();
    let second = snapshot
        .read_projection(GraphProjectionReadRequest {
            namespace: GraphNamespace::new("project").unwrap(),
            projection: GraphProjectionId::new("code-one").unwrap(),
            after_entity: first.next_entity,
            after_relation: None,
            max_entities: 2,
            max_relations: 1,
            cancellation: cancellation(),
        })
        .unwrap();
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
fn traversal_direction_supports_incoming_and_both_without_duplicate_visits() {
    let db = GraphDb::open(GraphDbOpenOptions {
        location: GraphDbLocation::Memory,
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Memory,
        cancellation: cancellation(),
    })
    .unwrap();
    publish(&db, "code", &["source", "target"]);

    let traverse = |direction| {
        db.traverse(TraversalRequest {
            namespace: GraphNamespace::new("project").unwrap(),
            start: GraphEntityId::new("target").unwrap(),
            relation_kinds: BTreeSet::new(),
            direction,
            max_depth: 1,
            max_visits: 2,
            max_results: 2,
            cancellation: cancellation(),
        })
        .unwrap()
    };
    for direction in [
        GraphTraversalDirection::Incoming,
        GraphTraversalDirection::Both,
    ] {
        assert_eq!(
            traverse(direction)
                .visits
                .into_iter()
                .map(|visit| visit.entity)
                .collect::<Vec<_>>(),
            vec![
                GraphEntityId::new("target").unwrap(),
                GraphEntityId::new("source").unwrap(),
            ]
        );
    }
}

#[test]
fn vector_search_filters_exact_projection_before_applying_the_limit() {
    let db = GraphDb::open(GraphDbOpenOptions {
        location: GraphDbLocation::Memory,
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Memory,
        cancellation: cancellation(),
    })
    .unwrap();
    for (projection, identity, values) in [
        ("active", "active-best", vec![0.0, 0.0]),
        ("active", "active-second", vec![1.0, 1.0]),
        ("staging", "staging-best", vec![0.0, 0.0]),
    ] {
        db.apply(
            GraphWriteBatch::new(
                GraphNamespace::new("project").unwrap(),
                GraphProjectionId::new(projection).unwrap(),
                SourceGeneration::new(format!("{projection}-{identity}")).unwrap(),
                GraphWatermark::new(format!("{projection}-{identity}")).unwrap(),
                vec![GraphMutation::UpsertEntity(vector_entity(identity, values))],
                cancellation(),
            )
            .unwrap(),
        )
        .unwrap();
    }

    let result = db
        .vector_search(VectorSearchRequest {
            namespace: GraphNamespace::new("project").unwrap(),
            projection: GraphProjectionId::new("active").unwrap(),
            property: GraphPropertyName::new("embedding").unwrap(),
            query: vec![0.0, 0.0],
            dimension: 2,
            metric: VectorMetric::Euclidean,
            limit: 2,
            cancellation: cancellation(),
        })
        .unwrap();
    assert_eq!(
        result
            .matches
            .into_iter()
            .map(|candidate| candidate.entity.as_str().to_owned())
            .collect::<Vec<_>>(),
        vec!["active-best", "active-second"]
    );
}

#[test]
fn vector_index_tracks_incremental_insert_update_delete_and_restart() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("vector-index.grafeo");
    let open = || {
        GraphDb::open(GraphDbOpenOptions {
            location: GraphDbLocation::Persistent(path.clone()),
            expected_format: GraphFormatVersion::new(2).unwrap(),
            durability: GraphDurability::Sync,
            cancellation: cancellation(),
        })
        .unwrap()
    };
    let search = |db: &GraphDb| {
        db.vector_search(VectorSearchRequest {
            namespace: GraphNamespace::new("project").unwrap(),
            projection: GraphProjectionId::new("active").unwrap(),
            property: GraphPropertyName::new("embedding").unwrap(),
            query: vec![0.0, 0.0],
            dimension: 2,
            metric: VectorMetric::Euclidean,
            limit: 1,
            cancellation: cancellation(),
        })
        .unwrap()
        .matches
        .into_iter()
        .map(|candidate| candidate.entity.as_str().to_owned())
        .collect::<Vec<_>>()
    };
    let apply = |db: &GraphDb, watermark: &str, mutations| {
        db.apply(
            GraphWriteBatch::new(
                GraphNamespace::new("project").unwrap(),
                GraphProjectionId::new("active").unwrap(),
                SourceGeneration::new(watermark).unwrap(),
                GraphWatermark::new(watermark).unwrap(),
                mutations,
                cancellation(),
            )
            .unwrap(),
        )
        .unwrap();
    };

    let db = open();
    apply(
        &db,
        "insert-far",
        vec![GraphMutation::UpsertEntity(vector_entity(
            "far",
            vec![10.0, 10.0],
        ))],
    );
    assert_eq!(search(&db), vec!["far"]);
    apply(
        &db,
        "insert-near",
        vec![GraphMutation::UpsertEntity(vector_entity(
            "changing",
            vec![0.0, 0.0],
        ))],
    );
    assert_eq!(search(&db), vec!["changing"]);
    apply(
        &db,
        "update-near",
        vec![GraphMutation::UpsertEntity(vector_entity(
            "changing",
            vec![20.0, 20.0],
        ))],
    );
    assert_eq!(search(&db), vec!["far"]);
    apply(
        &db,
        "restore-near",
        vec![GraphMutation::UpsertEntity(vector_entity(
            "changing",
            vec![0.0, 0.0],
        ))],
    );
    apply(
        &db,
        "delete-near",
        vec![GraphMutation::DeleteEntity(
            GraphEntityId::new("changing").unwrap(),
        )],
    );
    assert_eq!(search(&db), vec!["far"]);
    let large_corpus = (0..2_048)
        .map(|ordinal| {
            GraphMutation::UpsertEntity(vector_entity(
                &format!("bulk-{ordinal:04}"),
                vec![100.0 + ordinal as f32, 100.0 + ordinal as f32],
            ))
        })
        .collect();
    apply(&db, "large-corpus", large_corpus);
    db.close().unwrap();

    let admission_started = Instant::now();
    let reopened = open();
    let admission_elapsed = admission_started.elapsed();
    assert!(
        admission_elapsed < Duration::from_secs(5),
        "opening a 2,049-vector graph took {admission_elapsed:?}"
    );
    let index_request = GraphVectorIndexRequest {
        namespace: GraphNamespace::new("project").unwrap(),
        projection: GraphProjectionId::new("active").unwrap(),
        property: GraphPropertyName::new("embedding").unwrap(),
        dimension: 2,
        metric: VectorMetric::Euclidean,
        cancellation: cancellation(),
    };
    assert_eq!(
        reopened.vector_index_status(index_request.clone()).unwrap(),
        GraphVectorIndexStatus::Missing,
        "GraphDb admission must not synchronously rebuild a corpus index"
    );
    assert_eq!(
        reopened.ensure_vector_index(index_request).unwrap(),
        GraphVectorIndexStatus::Available
    );
    assert_eq!(search(&reopened), vec!["far"]);
}
