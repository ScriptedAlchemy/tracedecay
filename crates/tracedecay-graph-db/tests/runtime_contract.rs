use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphCancellation, GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability,
    GraphEntity, GraphEntityId, GraphFormatVersion, GraphIdempotencyKey, GraphLabel, GraphMutation,
    GraphNamespace, GraphProjectionId, GraphProperty, GraphPropertyName, GraphPublication,
    GraphRelation, GraphRelationId, GraphRelationKind, GraphVector, GraphWatermark,
    GraphWriteBatch, NeverCancelled, ProjectionReplacement, SourceGeneration, TraversalRequest,
    VectorMetric, VectorSearchRequest,
};

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
    GraphNamespace::new("project").unwrap()
}

fn projection(value: &str) -> GraphProjectionId {
    GraphProjectionId::new(value).unwrap()
}

fn entity_id(value: &str) -> GraphEntityId {
    GraphEntityId::new(value).unwrap()
}

fn relation_id(value: &str) -> GraphRelationId {
    GraphRelationId::new(value).unwrap()
}

fn generation(value: &str) -> SourceGeneration {
    SourceGeneration::new(value).unwrap()
}

fn watermark(value: &str) -> GraphWatermark {
    GraphWatermark::new(value).unwrap()
}

fn memory_db() -> GraphDb {
    GraphDb::open(GraphDbOpenOptions {
        location: GraphDbLocation::Memory,
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Memory,
        cancellation: live(),
    })
    .unwrap()
}

fn entity(value: &str) -> GraphEntity {
    GraphEntity::new(entity_id(value), BTreeSet::new(), BTreeMap::new()).unwrap()
}

fn relation(value: &str, from: &str, to: &str, kind: &str) -> GraphRelation {
    GraphRelation::new(
        relation_id(value),
        entity_id(from),
        entity_id(to),
        GraphRelationKind::new(kind).unwrap(),
        BTreeMap::new(),
    )
    .unwrap()
}

fn batch(
    owner: &str,
    generation_value: &str,
    watermark_value: &str,
    mutations: Vec<GraphMutation>,
) -> GraphWriteBatch {
    GraphWriteBatch::new(
        namespace(),
        projection(owner),
        generation(generation_value),
        watermark(watermark_value),
        mutations,
        live(),
    )
    .unwrap()
}

fn traversal(start: &str) -> TraversalRequest {
    TraversalRequest {
        namespace: namespace(),
        start: entity_id(start),
        relation_kinds: BTreeSet::new(),
        max_depth: 8,
        max_visits: 100,
        max_results: 100,
        cancellation: live(),
    }
}

#[test]
fn rejects_invalid_opaque_identities() {
    assert!(matches!(
        GraphNamespace::new(""),
        Err(GraphDbError::InvalidRequest { .. })
    ));
    assert!(matches!(
        GraphEntityId::new("__tracedecay_graph_db_forbidden"),
        Err(GraphDbError::InvalidRequest { .. })
    ));
    assert!(matches!(
        GraphLabel::new("x".repeat(1025)),
        Err(GraphDbError::InvalidRequest { .. })
    ));
}

#[test]
fn opens_memory_and_accepts_exact_format() {
    let db = memory_db();
    let commit = db
        .apply(batch(
            "code",
            "g1",
            "w1",
            vec![GraphMutation::UpsertEntity(entity("a"))],
        ))
        .unwrap();
    assert_eq!(commit.sequence, 1);
}

#[test]
fn open_honors_cancellation() {
    let error = GraphDb::open(GraphDbOpenOptions {
        location: GraphDbLocation::Memory,
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Memory,
        cancellation: Arc::new(Cancelled),
    })
    .unwrap_err();
    assert_eq!(error, GraphDbError::Cancelled);
}

#[test]
fn apply_honors_cancellation_without_advancing_sequence() {
    let db = memory_db();
    let cancelled = GraphWriteBatch::new(
        namespace(),
        projection("code"),
        generation("g1"),
        watermark("w1"),
        vec![GraphMutation::UpsertEntity(entity("a"))],
        Arc::new(Cancelled),
    )
    .unwrap();
    assert_eq!(db.apply(cancelled).unwrap_err(), GraphDbError::Cancelled);
    let commit = db
        .apply(batch(
            "code",
            "g2",
            "w2",
            vec![GraphMutation::UpsertEntity(entity("b"))],
        ))
        .unwrap();
    assert_eq!(commit.sequence, 1);
}

#[test]
fn rejects_persistent_path_without_grafeo_extension() {
    let temp = TempDir::new().unwrap();
    let error = GraphDb::open(GraphDbOpenOptions {
        location: GraphDbLocation::Persistent(temp.path().join("graph.db")),
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Sync,
        cancellation: live(),
    })
    .unwrap_err();
    assert!(matches!(error, GraphDbError::InvalidRequest { .. }));
}

#[test]
fn invalid_late_mutation_rolls_back_whole_batch_and_sequence() {
    let db = memory_db();
    let error = db
        .apply(batch(
            "code",
            "g1",
            "w1",
            vec![
                GraphMutation::UpsertEntity(entity("a")),
                GraphMutation::UpsertRelation(relation("r", "a", "missing", "calls")),
            ],
        ))
        .unwrap_err();
    assert!(matches!(error, GraphDbError::InvalidRequest { .. }));

    let result = db.traverse(traversal("a")).unwrap_err();
    assert!(matches!(result, GraphDbError::InvalidRequest { .. }));
    let commit = db
        .apply(batch(
            "code",
            "g2",
            "w2",
            vec![GraphMutation::UpsertEntity(entity("b"))],
        ))
        .unwrap();
    assert_eq!(commit.sequence, 1);
}

#[test]
fn snapshot_is_immutable_after_live_write() {
    let db = memory_db();
    db.apply(batch(
        "code",
        "g1",
        "w1",
        vec![
            GraphMutation::UpsertEntity(entity("a")),
            GraphMutation::UpsertEntity(entity("b")),
            GraphMutation::UpsertRelation(relation("ab", "a", "b", "calls")),
        ],
    ))
    .unwrap();
    let snapshot = db.snapshot().unwrap();
    db.apply(batch(
        "code",
        "g2",
        "w2",
        vec![
            GraphMutation::UpsertEntity(entity("c")),
            GraphMutation::UpsertRelation(relation("bc", "b", "c", "calls")),
        ],
    ))
    .unwrap();

    assert_eq!(snapshot.traverse(traversal("a")).unwrap().visits.len(), 2);
    assert_eq!(db.traverse(traversal("a")).unwrap().visits.len(), 3);
}

#[test]
fn traversal_is_deterministic_across_mutation_order() {
    fn populated(order: [&str; 2]) -> GraphDb {
        let db = memory_db();
        let mut mutations = vec![
            GraphMutation::UpsertEntity(entity("a")),
            GraphMutation::UpsertEntity(entity("b")),
            GraphMutation::UpsertEntity(entity("c")),
        ];
        for target in order {
            mutations.push(GraphMutation::UpsertRelation(relation(
                &format!("a{target}"),
                "a",
                target,
                "calls",
            )));
        }
        db.apply(batch("code", "g1", "w1", mutations)).unwrap();
        db
    }
    let first = populated(["b", "c"]).traverse(traversal("a")).unwrap();
    let second = populated(["c", "b"]).traverse(traversal("a")).unwrap();
    assert_eq!(first, second);
    assert_eq!(
        first
            .visits
            .iter()
            .map(|visit| visit.entity.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b", "c"]
    );
}

#[test]
fn traversal_filters_before_discovery_and_honors_depth() {
    let db = memory_db();
    db.apply(batch(
        "code",
        "g1",
        "w1",
        vec![
            GraphMutation::UpsertEntity(entity("a")),
            GraphMutation::UpsertEntity(entity("b")),
            GraphMutation::UpsertEntity(entity("c")),
            GraphMutation::UpsertRelation(relation("ab", "a", "b", "calls")),
            GraphMutation::UpsertRelation(relation("ac", "a", "c", "owns")),
        ],
    ))
    .unwrap();
    let mut request = traversal("a");
    request
        .relation_kinds
        .insert(GraphRelationKind::new("calls").unwrap());
    request.max_depth = 1;
    let result = db.traverse(request).unwrap();
    assert_eq!(result.visits.len(), 2);
    assert_eq!(result.visits[1].entity.as_str(), "b");
}

#[test]
fn traversal_budget_exhaustion_is_typed() {
    let db = memory_db();
    db.apply(batch(
        "code",
        "g1",
        "w1",
        vec![GraphMutation::UpsertEntity(entity("a"))],
    ))
    .unwrap();
    let mut request = traversal("a");
    request.max_visits = 0;
    assert_eq!(
        db.traverse(request).unwrap_err(),
        GraphDbError::BudgetExhausted
    );
}

#[test]
fn traversal_result_budget_truncates_deterministically() {
    let db = memory_db();
    db.apply(batch(
        "code",
        "g1",
        "w1",
        vec![
            GraphMutation::UpsertEntity(entity("a")),
            GraphMutation::UpsertEntity(entity("b")),
            GraphMutation::UpsertEntity(entity("c")),
            GraphMutation::UpsertRelation(relation("ab", "a", "b", "calls")),
            GraphMutation::UpsertRelation(relation("ac", "a", "c", "calls")),
        ],
    ))
    .unwrap();
    let mut request = traversal("a");
    request.max_results = 2;
    let result = db.traverse(request).unwrap();
    assert_eq!(
        result
            .visits
            .iter()
            .map(|visit| visit.entity.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
}

#[test]
fn traversal_honors_cancellation() {
    let db = memory_db();
    let mut request = traversal("a");
    request.cancellation = Arc::new(Cancelled);
    assert_eq!(db.traverse(request).unwrap_err(), GraphDbError::Cancelled);
}

fn vector_entity(value: &str, vector: Vec<f32>, metric: VectorMetric) -> GraphEntity {
    let mut properties = BTreeMap::new();
    properties.insert(
        GraphPropertyName::new("embedding").unwrap(),
        GraphProperty::Vector(GraphVector::new(vector, 2, metric).unwrap()),
    );
    GraphEntity::new(entity_id(value), BTreeSet::new(), properties).unwrap()
}

fn vector_request(metric: VectorMetric, query: Vec<f32>) -> VectorSearchRequest {
    VectorSearchRequest {
        namespace: namespace(),
        property: GraphPropertyName::new("embedding").unwrap(),
        query,
        dimension: 2,
        metric,
        limit: 10,
        cancellation: live(),
    }
}

#[test]
fn vector_admission_rejects_dimension_and_non_finite_values() {
    assert!(matches!(
        GraphVector::new(Vec::new(), 0, VectorMetric::Cosine),
        Err(GraphDbError::InvalidRequest { .. })
    ));
    assert!(matches!(
        GraphVector::new(vec![1.0], 2, VectorMetric::Cosine),
        Err(GraphDbError::InvalidRequest { .. })
    ));
    assert!(matches!(
        GraphVector::new(vec![f32::NAN, 0.0], 2, VectorMetric::Cosine),
        Err(GraphDbError::InvalidRequest { .. })
    ));
    let db = memory_db();
    assert!(matches!(
        db.vector_search(vector_request(VectorMetric::Cosine, vec![1.0])),
        Err(GraphDbError::InvalidRequest { .. })
    ));
}

#[test]
fn vector_metric_rejects_unsupported_values() {
    assert_eq!(
        VectorMetric::parse("manhattan").unwrap_err(),
        GraphDbError::InvalidRequest {
            message: "unsupported vector metric `manhattan`".to_owned()
        }
    );
}

#[test]
fn vector_search_uses_metric_and_stable_identity_ties() {
    let db = memory_db();
    db.apply(batch(
        "vectors",
        "g1",
        "w1",
        vec![
            GraphMutation::UpsertEntity(vector_entity("b", vec![1.0, 0.0], VectorMetric::Cosine)),
            GraphMutation::UpsertEntity(vector_entity("a", vec![1.0, 0.0], VectorMetric::Cosine)),
            GraphMutation::UpsertEntity(vector_entity(
                "ignored",
                vec![1.0, 0.0],
                VectorMetric::Euclidean,
            )),
        ],
    ))
    .unwrap();
    let result = db
        .vector_search(vector_request(VectorMetric::Cosine, vec![1.0, 0.0]))
        .unwrap();
    assert_eq!(
        result
            .matches
            .iter()
            .map(|item| item.entity.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    assert_eq!(result.matches[0].distance, 0.0);
}

#[test]
fn vector_search_supports_dot_product_and_euclidean() {
    for metric in [VectorMetric::DotProduct, VectorMetric::Euclidean] {
        let db = memory_db();
        db.apply(batch(
            "vectors",
            "g1",
            "w1",
            vec![
                GraphMutation::UpsertEntity(vector_entity("near", vec![1.0, 0.0], metric)),
                GraphMutation::UpsertEntity(vector_entity("far", vec![0.0, 1.0], metric)),
            ],
        ))
        .unwrap();
        let result = db
            .vector_search(vector_request(metric, vec![1.0, 0.0]))
            .unwrap();
        assert_eq!(result.matches[0].entity.as_str(), "near");
    }
}

#[test]
fn projection_replacement_preserves_cross_projection_target() {
    let db = memory_db();
    db.apply(batch(
        "facts",
        "g1",
        "w1",
        vec![GraphMutation::UpsertEntity(entity("shared"))],
    ))
    .unwrap();
    db.apply(batch(
        "code",
        "g2",
        "w2",
        vec![
            GraphMutation::UpsertEntity(entity("source")),
            GraphMutation::UpsertRelation(relation("link", "source", "shared", "refers")),
        ],
    ))
    .unwrap();
    db.replace_projection(ProjectionReplacement {
        namespace: namespace(),
        projection: projection("code"),
        source_generation: generation("g3"),
        next_watermark: watermark("w3"),
        entities: vec![entity("new-source")],
        relations: Vec::new(),
        cancellation: live(),
    })
    .unwrap();

    assert_eq!(
        db.traverse(traversal("shared")).unwrap().visits[0]
            .entity
            .as_str(),
        "shared"
    );
    assert!(matches!(
        db.traverse(traversal("source")),
        Err(GraphDbError::InvalidRequest { .. })
    ));
}

fn publication(key: &str, expected: Option<&str>) -> GraphPublication {
    GraphPublication {
        namespace: namespace(),
        idempotency_key: GraphIdempotencyKey::new(key).unwrap(),
        source_generation: generation("g1"),
        expected_watermark: expected.map(watermark),
        next_watermark: watermark("w1"),
        batch: batch(
            "code",
            "g1",
            "w1",
            vec![GraphMutation::UpsertEntity(entity("a"))],
        ),
        cancellation: live(),
    }
}

#[test]
fn invalid_projection_replacement_preserves_prior_graph() {
    let db = memory_db();
    db.apply(batch(
        "code",
        "g1",
        "w1",
        vec![GraphMutation::UpsertEntity(entity("old"))],
    ))
    .unwrap();
    let error = db
        .replace_projection(ProjectionReplacement {
            namespace: namespace(),
            projection: projection("code"),
            source_generation: generation("g2"),
            next_watermark: watermark("w2"),
            entities: vec![entity("new")],
            relations: vec![relation("bad", "new", "missing", "calls")],
            cancellation: live(),
        })
        .unwrap_err();
    assert!(matches!(error, GraphDbError::InvalidRequest { .. }));
    assert_eq!(
        db.traverse(traversal("old")).unwrap().visits[0]
            .entity
            .as_str(),
        "old"
    );
    let commit = db
        .apply(batch(
            "code",
            "g3",
            "w3",
            vec![GraphMutation::UpsertEntity(entity("kept"))],
        ))
        .unwrap();
    assert_eq!(commit.sequence, 2);
}

#[test]
fn publication_replay_returns_original_commit() {
    let db = memory_db();
    let first = db.publish(publication("event-1", None)).unwrap();
    let second = db.publish(publication("event-1", None)).unwrap();
    assert_eq!(first, second);
}

#[test]
fn publication_changed_input_and_stale_watermark_conflict() {
    let db = memory_db();
    db.publish(publication("event-1", None)).unwrap();
    let mut changed = publication("event-1", None);
    changed.next_watermark = watermark("w2");
    changed.batch.next_watermark = watermark("w2");
    assert_eq!(db.publish(changed).unwrap_err(), GraphDbError::Conflict);
    assert_eq!(
        db.publish(publication("event-2", Some("stale")))
            .unwrap_err(),
        GraphDbError::Conflict
    );
}

fn persistent_options(path: std::path::PathBuf) -> GraphDbOpenOptions {
    GraphDbOpenOptions {
        location: GraphDbLocation::Persistent(path),
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Sync,
        cancellation: live(),
    }
}

#[test]
fn persistent_close_and_reopen_preserves_graph_and_vector() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("graph.grafeo");
    let db = GraphDb::open(persistent_options(path.clone())).unwrap();
    db.apply(batch(
        "code",
        "g1",
        "w1",
        vec![
            GraphMutation::UpsertEntity(vector_entity("a", vec![1.0, 0.0], VectorMetric::Cosine)),
            GraphMutation::UpsertEntity(entity("b")),
            GraphMutation::UpsertRelation(relation("ab", "a", "b", "calls")),
        ],
    ))
    .unwrap();
    db.close().unwrap();

    let reopened = GraphDb::open(persistent_options(path)).unwrap();
    assert_eq!(reopened.traverse(traversal("a")).unwrap().visits.len(), 2);
    assert_eq!(
        reopened
            .vector_search(vector_request(VectorMetric::Cosine, vec![1.0, 0.0]))
            .unwrap()
            .matches[0]
            .entity
            .as_str(),
        "a"
    );
}

#[test]
fn publication_state_survives_reopen() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("graph.grafeo");
    let db = GraphDb::open(persistent_options(path.clone())).unwrap();
    let first = db.publish(publication("event-1", None)).unwrap();
    db.close().unwrap();
    let reopened = GraphDb::open(persistent_options(path)).unwrap();
    assert_eq!(
        reopened.publish(publication("event-1", None)).unwrap(),
        first
    );
}

#[test]
fn malformed_persistent_file_is_corrupt() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("graph.grafeo");
    std::fs::write(&path, b"not a grafeo database").unwrap();
    let error = GraphDb::open(persistent_options(path)).unwrap_err();
    assert!(matches!(error, GraphDbError::Corrupt { .. }));
}

#[test]
fn valid_foreign_grafeo_store_requires_reset() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("graph.grafeo");
    let raw = grafeo_engine::GrafeoDB::with_config(
        grafeo_engine::Config::persistent(&path)
            .with_storage_format(grafeo_engine::config::StorageFormat::SingleFile),
    )
    .unwrap();
    raw.session().create_node(&["foreign"]);
    raw.close().unwrap();
    let error = GraphDb::open(persistent_options(path)).unwrap_err();
    assert!(matches!(error, GraphDbError::ResetRequired { .. }));
}

#[test]
fn wrong_tracedecay_format_requires_reset() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("graph.grafeo");
    let raw = grafeo_engine::GrafeoDB::with_config(
        grafeo_engine::Config::persistent(&path)
            .with_storage_format(grafeo_engine::config::StorageFormat::SingleFile),
    )
    .unwrap();
    raw.session()
        .create_node_with_props(
            &["__tracedecay_graph_db_format"],
            [("__tracedecay_graph_db_version", 1_i64.into())],
        )
        .unwrap();
    raw.close().unwrap();
    let error = GraphDb::open(persistent_options(path)).unwrap_err();
    assert!(matches!(error, GraphDbError::ResetRequired { .. }));
}

#[test]
fn closed_handle_fails_typed() {
    let db = memory_db();
    db.close().unwrap();
    assert_eq!(
        db.apply(batch("code", "g1", "w1", Vec::new())).unwrap_err(),
        GraphDbError::Closed
    );
}
