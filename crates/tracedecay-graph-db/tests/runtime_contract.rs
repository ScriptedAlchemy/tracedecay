use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphBudgetKind, GraphCancellation, GraphDbError, GraphDbLeaseV1, GraphDbOwner, GraphEntity,
    GraphEntityId, GraphIdempotencyKey, GraphLabel, GraphMutation, GraphNamespace,
    GraphProjectionId, GraphProperty, GraphPropertyName, GraphPublication,
    GraphPublicationInputDigest, GraphRelation, GraphRelationId, GraphRelationKind,
    GraphTraversalDirection, GraphVector, GraphVectorIndexRequest, GraphVectorIndexStatus,
    GraphWatermark, GraphWriteBatch, NeverCancelled, ProjectionReplacement, SourceGeneration,
    TraversalRequest, VectorMetric, VectorSearchRequest,
};

mod support;

use support::{RegisteredGraph, graph_path};

#[derive(Debug)]
struct Cancelled;

impl GraphCancellation for Cancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

#[derive(Debug)]
struct CancelOnPoll {
    polls: AtomicUsize,
    cancel_on: usize,
}

impl CancelOnPoll {
    fn new(cancel_on: usize) -> Self {
        Self {
            polls: AtomicUsize::new(0),
            cancel_on,
        }
    }
}

impl GraphCancellation for CancelOnPoll {
    fn is_cancelled(&self) -> bool {
        self.polls.fetch_add(1, Ordering::SeqCst) + 1 >= self.cancel_on
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

fn memory_db() -> GraphDbLeaseV1 {
    GraphDbOwner::memory(live()).unwrap().issue_lease().unwrap()
}

#[test]
fn only_the_owner_can_close_shared_operation_handles() {
    let owner = GraphDbOwner::memory(live()).unwrap();
    let handle = owner.issue_lease().unwrap();
    let peer = handle.clone();

    assert!(handle.snapshot().is_ok());
    owner.close().unwrap();
    assert_eq!(handle.snapshot().unwrap_err(), GraphDbError::Closed);
    assert_eq!(peer.snapshot().unwrap_err(), GraphDbError::Closed);
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

fn batch_in(
    namespace_value: &str,
    owner: &str,
    generation_value: &str,
    watermark_value: &str,
    mutations: Vec<GraphMutation>,
) -> GraphWriteBatch {
    GraphWriteBatch::new(
        GraphNamespace::new(namespace_value).unwrap(),
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
        direction: GraphTraversalDirection::Outgoing,
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
fn opaque_identity_deserialization_reuses_constructor_validation() {
    for invalid in [
        "\"\"".to_owned(),
        "\"__tracedecay_graph_db_forbidden\"".to_owned(),
        format!("\"{}\"", "x".repeat(1025)),
    ] {
        assert!(serde_json::from_str::<GraphNamespace>(&invalid).is_err());
    }
}

#[test]
fn opens_memory_and_accepts_exact_format() {
    let db = memory_db();
    let commit = db
        .apply_unverified(batch(
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
    let error = GraphDbOwner::memory(Arc::new(Cancelled)).unwrap_err();
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
    assert_eq!(
        db.apply_unverified(cancelled).unwrap_err(),
        GraphDbError::Cancelled
    );
    let commit = db
        .apply_unverified(batch(
            "code",
            "g2",
            "w2",
            vec![GraphMutation::UpsertEntity(entity("b"))],
        ))
        .unwrap();
    assert_eq!(commit.sequence, 1);
}

#[test]
fn apply_rechecks_cancellation_after_lock_for_empty_batch() {
    let db = memory_db();
    let cancelled = GraphWriteBatch::new(
        namespace(),
        projection("code"),
        generation("g1"),
        watermark("w1"),
        Vec::new(),
        Arc::new(CancelOnPoll::new(2)),
    )
    .unwrap();
    assert_eq!(
        db.apply_unverified(cancelled).unwrap_err(),
        GraphDbError::Cancelled
    );
    assert_eq!(
        db.apply_unverified(batch(
            "code",
            "g2",
            "w2",
            vec![GraphMutation::UpsertEntity(entity("kept"))],
        ))
        .unwrap()
        .sequence,
        1
    );
}

#[test]
fn apply_rechecks_cancellation_immediately_before_commit() {
    let db = memory_db();
    let cancelled = GraphWriteBatch::new(
        namespace(),
        projection("code"),
        generation("g1"),
        watermark("w1"),
        vec![GraphMutation::UpsertEntity(entity("cancelled"))],
        Arc::new(CancelOnPoll::new(4)),
    )
    .unwrap();
    assert_eq!(
        db.apply_unverified(cancelled).unwrap_err(),
        GraphDbError::Cancelled
    );
    assert!(matches!(
        db.traverse(traversal("cancelled")),
        Err(GraphDbError::InvalidRequest { .. })
    ));
    assert_eq!(
        db.apply_unverified(batch(
            "code",
            "g2",
            "w2",
            vec![GraphMutation::UpsertEntity(entity("kept"))],
        ))
        .unwrap()
        .sequence,
        1
    );
}

#[test]
fn derived_identity_apply_rolls_back_and_survives_reopen() {
    let temp = TempDir::new().unwrap();
    let (registered, db) = RegisteredGraph::open_lease(temp.path()).unwrap();
    assert_eq!(
        db.apply_unverified(batch(
            "code",
            "g1",
            "w1",
            vec![GraphMutation::UpsertEntity(entity("committed"))],
        ))
        .unwrap()
        .sequence,
        1
    );
    let cancelled = GraphWriteBatch::new(
        namespace(),
        projection("code"),
        generation("g2"),
        watermark("w2"),
        vec![GraphMutation::UpsertEntity(entity("rolled-back"))],
        Arc::new(CancelOnPoll::new(4)),
    )
    .unwrap();
    assert_eq!(db.apply_unverified(cancelled), Err(GraphDbError::Cancelled));
    assert_eq!(
        db.entity(&namespace(), &entity_id("rolled-back"), live())
            .unwrap(),
        None
    );
    drop(db);
    assert!(registered.close().unwrap());

    let reopened = registered.reopen_lease().unwrap();
    assert_eq!(
        reopened
            .entity(&namespace(), &entity_id("committed"), live())
            .unwrap(),
        Some(entity("committed"))
    );
    assert_eq!(
        reopened
            .apply_unverified(batch(
                "code",
                "g3",
                "w3",
                vec![GraphMutation::UpsertEntity(entity("after-reopen"))],
            ))
            .unwrap()
            .sequence,
        2
    );
}

#[test]
fn invalid_late_mutation_rolls_back_whole_batch_and_sequence() {
    let db = memory_db();
    let error = db
        .apply_unverified(batch(
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
        .apply_unverified(batch(
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
    db.apply_unverified(batch(
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
    let writer_db = db.clone();
    let (sent, received) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let result = writer_db.apply_unverified(batch(
            "code",
            "g2",
            "w2",
            vec![
                GraphMutation::UpsertEntity(entity("c")),
                GraphMutation::UpsertRelation(relation("bc", "b", "c", "calls")),
            ],
        ));
        sent.send(result).unwrap();
    });
    assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
    assert_eq!(snapshot.traverse(traversal("a")).unwrap().visits.len(), 2);
    let outgoing = snapshot
        .outgoing_relations(
            &namespace(),
            &[entity_id("a")],
            &BTreeSet::from([GraphRelationKind::new("calls").unwrap()]),
            1,
            live(),
        )
        .unwrap();
    assert_eq!(outgoing[0][0].identity.as_str(), "ab");
    drop(snapshot);
    received
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();
    writer.join().unwrap();
    assert_eq!(db.traverse(traversal("a")).unwrap().visits.len(), 3);
}

#[test]
fn traversal_is_deterministic_across_mutation_order() {
    fn populated(order: [&str; 2]) -> GraphDbLeaseV1 {
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
        db.apply_unverified(batch("code", "g1", "w1", mutations))
            .unwrap();
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
    db.apply_unverified(batch(
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
fn traversal_supports_incoming_and_bidirectional_neighbors() {
    let db = memory_db();
    db.apply_unverified(batch(
        "code",
        "g1",
        "w1",
        vec![
            GraphMutation::UpsertEntity(entity("a")),
            GraphMutation::UpsertEntity(entity("b")),
            GraphMutation::UpsertEntity(entity("c")),
            GraphMutation::UpsertRelation(relation("ab", "a", "b", "calls")),
            GraphMutation::UpsertRelation(relation("bc", "b", "c", "calls")),
        ],
    ))
    .unwrap();

    let mut incoming = traversal("c");
    incoming.direction = GraphTraversalDirection::Incoming;
    incoming.max_depth = 2;
    assert_eq!(
        db.traverse(incoming)
            .unwrap()
            .visits
            .into_iter()
            .map(|visit| visit.entity.as_str().to_owned())
            .collect::<Vec<_>>(),
        ["c", "b", "a"]
    );

    let mut both = traversal("b");
    both.direction = GraphTraversalDirection::Both;
    both.max_depth = 1;
    assert_eq!(
        db.traverse(both)
            .unwrap()
            .visits
            .into_iter()
            .map(|visit| visit.entity.as_str().to_owned())
            .collect::<Vec<_>>(),
        ["b", "a", "c"]
    );
}

#[test]
fn traversal_budget_exhaustion_is_typed() {
    let db = memory_db();
    db.apply_unverified(batch(
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
        GraphDbError::budget_exhausted(GraphBudgetKind::Read, 0)
    );
}

#[test]
fn traversal_visit_budget_stops_before_scanning_a_wide_frontier() {
    let db = memory_db();
    let mut mutations = vec![GraphMutation::UpsertEntity(entity("root"))];
    for index in 0..32 {
        let target = format!("target-{index:02}");
        mutations.push(GraphMutation::UpsertEntity(entity(&target)));
        mutations.push(GraphMutation::UpsertRelation(relation(
            &format!("edge-{index:02}"),
            "root",
            &target,
            "calls",
        )));
    }
    db.apply_unverified(batch("code", "g1", "w1", mutations))
        .unwrap();

    let mut request = traversal("root");
    request.max_visits = 1;
    request.cancellation = Arc::new(CancelOnPoll::new(6));
    assert_eq!(
        db.traverse(request).unwrap_err(),
        GraphDbError::budget_exhausted(GraphBudgetKind::Read, 1)
    );
}

#[test]
fn traversal_result_budget_truncates_deterministically() {
    let db = memory_db();
    db.apply_unverified(batch(
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

#[test]
fn batch_outgoing_reads_are_filtered_ordered_and_budgeted() {
    let db = memory_db();
    db.apply_unverified(batch(
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
    let starts = ["a", "missing", "b"].map(entity_id);
    let kinds = BTreeSet::from([GraphRelationKind::new("calls").unwrap()]);
    let relations = db
        .outgoing_relations(&namespace(), &starts, &kinds, 1, live())
        .unwrap();
    assert_eq!(
        relations
            .iter()
            .map(|relations| {
                relations
                    .iter()
                    .map(|relation| relation.identity.as_str())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>(),
        vec![vec!["ab"], Vec::<&str>::new(), Vec::<&str>::new()]
    );
    assert_eq!(
        db.outgoing_relation_ids(&namespace(), &starts, &kinds, 0, live())
            .unwrap_err(),
        GraphDbError::budget_exhausted(GraphBudgetKind::Read, 0)
    );
    assert_eq!(
        db.outgoing_relations(&namespace(), &starts, &kinds, 1, Arc::new(Cancelled),)
            .unwrap_err(),
        GraphDbError::Cancelled
    );
}

/// Plan 39 G7b: reverse adjacency must be readable in bulk through the graph
/// store, with the same kind filter, batch shape, budget, and cancellation
/// contract as the outgoing form — and it must actually read the opposite
/// direction, not silently mirror the outgoing result.
#[test]
fn batch_incoming_reads_are_filtered_ordered_and_budgeted() {
    let db = memory_db();
    db.apply_unverified(batch(
        "code",
        "g1",
        "w1",
        vec![
            GraphMutation::UpsertEntity(entity("a")),
            GraphMutation::UpsertEntity(entity("b")),
            GraphMutation::UpsertEntity(entity("c")),
            GraphMutation::UpsertRelation(relation("ab", "a", "b", "calls")),
            GraphMutation::UpsertRelation(relation("cb", "c", "b", "owns")),
        ],
    ))
    .unwrap();
    let starts = ["b", "missing", "a"].map(entity_id);
    let kinds = BTreeSet::from([GraphRelationKind::new("calls").unwrap()]);

    // `b` is the *target* of `ab`, so only the incoming read reaches it. `a`
    // has no inbound `calls` edge, which is what distinguishes this from the
    // outgoing result over the same fixture.
    assert_eq!(
        db.incoming_relation_ids(&namespace(), &starts, &kinds, 1, live())
            .unwrap()
            .iter()
            .map(|relations| relations
                .iter()
                .map(|relation| relation.as_str())
                .collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        vec![vec!["ab"], Vec::<&str>::new(), Vec::<&str>::new()]
    );
    assert_eq!(
        db.outgoing_relation_ids(&namespace(), &starts, &kinds, 1, live())
            .unwrap()
            .iter()
            .map(|relations| relations
                .iter()
                .map(|relation| relation.as_str())
                .collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        vec![Vec::<&str>::new(), Vec::<&str>::new(), vec!["ab"]],
        "outgoing must stay the mirror image of incoming over the same fixture"
    );

    // An over-budget read fails rather than returning a truncated fan-out.
    assert_eq!(
        db.incoming_relation_ids(&namespace(), &starts, &kinds, 0, live())
            .unwrap_err(),
        GraphDbError::budget_exhausted(GraphBudgetKind::Read, 0)
    );
    assert_eq!(
        db.incoming_relation_ids(&namespace(), &starts, &kinds, 1, Arc::new(Cancelled))
            .unwrap_err(),
        GraphDbError::Cancelled
    );
}

#[test]
fn multi_source_reachability_uses_overlay_and_global_budget() {
    let db = memory_db();
    db.apply_unverified(batch(
        "code",
        "g1",
        "w1",
        vec![
            GraphMutation::UpsertEntity(entity("a")),
            GraphMutation::UpsertEntity(entity("b")),
            GraphMutation::UpsertEntity(entity("c")),
            GraphMutation::UpsertEntity(entity("d")),
            GraphMutation::UpsertRelation(relation("ab", "a", "b", "depends")),
            GraphMutation::UpsertRelation(relation("bc", "b", "c", "depends")),
        ],
    ))
    .unwrap();
    let starts = [entity_id("a"), entity_id("d")];
    let kinds = BTreeSet::from([GraphRelationKind::new("depends").unwrap()]);
    let overrides = BTreeMap::from([(entity_id("b"), BTreeSet::from([entity_id("d")]))]);
    let reachable = db
        .reachable_entities(
            &namespace(),
            &projection("code"),
            &starts,
            &kinds,
            &overrides,
            4,
            live(),
        )
        .unwrap();
    assert_eq!(
        reachable[0]
            .iter()
            .map(GraphEntityId::as_str)
            .collect::<Vec<_>>(),
        vec!["a", "b", "d"]
    );
    assert_eq!(
        reachable[1]
            .iter()
            .map(GraphEntityId::as_str)
            .collect::<Vec<_>>(),
        vec!["d"]
    );
    assert_eq!(
        db.reachable_entities(
            &namespace(),
            &projection("code"),
            &starts,
            &kinds,
            &overrides,
            3,
            live(),
        )
        .unwrap_err(),
        GraphDbError::budget_exhausted(GraphBudgetKind::Read, 3)
    );
}

#[test]
fn reachability_excludes_relations_owned_by_another_projection() {
    let db = memory_db();
    db.apply_unverified(batch(
        "code",
        "code-g1",
        "code-w1",
        vec![
            GraphMutation::UpsertEntity(entity("a")),
            GraphMutation::UpsertEntity(entity("b")),
            GraphMutation::UpsertEntity(entity("c")),
            GraphMutation::UpsertRelation(relation("ac", "a", "c", "depends")),
        ],
    ))
    .unwrap();
    db.apply_unverified(batch(
        "facts",
        "facts-g1",
        "facts-w1",
        vec![GraphMutation::UpsertRelation(relation(
            "ab", "a", "b", "depends",
        ))],
    ))
    .unwrap();

    let reachable = db
        .reachable_entities(
            &namespace(),
            &projection("code"),
            &[entity_id("a")],
            &BTreeSet::from([GraphRelationKind::new("depends").unwrap()]),
            &BTreeMap::new(),
            3,
            live(),
        )
        .unwrap();
    assert_eq!(
        reachable[0]
            .iter()
            .map(GraphEntityId::as_str)
            .collect::<Vec<_>>(),
        vec!["a", "c"]
    );
}

fn vector_entity(value: &str, vector: Vec<f32>, metric: VectorMetric) -> GraphEntity {
    vector_entity_with_dimension(value, vector.clone(), vector.len(), metric)
}

fn vector_entity_with_dimension(
    value: &str,
    vector: Vec<f32>,
    dimension: usize,
    metric: VectorMetric,
) -> GraphEntity {
    let mut properties = BTreeMap::new();
    properties.insert(
        GraphPropertyName::new("embedding").unwrap(),
        GraphProperty::Vector(GraphVector::new(vector, dimension, metric).unwrap()),
    );
    GraphEntity::new(entity_id(value), BTreeSet::new(), properties).unwrap()
}

fn vector_request(metric: VectorMetric, query: Vec<f32>) -> VectorSearchRequest {
    VectorSearchRequest {
        namespace: namespace(),
        projection: projection("vectors"),
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
    db.apply_unverified(batch(
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
        db.apply_unverified(batch(
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
fn vector_search_isolates_dimension_metric_and_namespace_before_distance() {
    let db = memory_db();
    db.apply_unverified(batch_in(
        "project",
        "vectors",
        "g1",
        "w1",
        vec![
            GraphMutation::UpsertEntity(vector_entity_with_dimension(
                "wanted",
                vec![1.0, 0.0],
                2,
                VectorMetric::Cosine,
            )),
            GraphMutation::UpsertEntity(vector_entity_with_dimension(
                "wrong-dimension",
                vec![1.0, 0.0, 0.0],
                3,
                VectorMetric::Cosine,
            )),
            GraphMutation::UpsertEntity(vector_entity_with_dimension(
                "wrong-metric",
                vec![1.0, 0.0],
                2,
                VectorMetric::Euclidean,
            )),
        ],
    ))
    .unwrap();
    db.apply_unverified(batch_in(
        "another-project",
        "vectors",
        "g2",
        "w2",
        vec![GraphMutation::UpsertEntity(vector_entity_with_dimension(
            "foreign-dimension",
            vec![1.0, 0.0, 0.0, 0.0],
            4,
            VectorMetric::Cosine,
        ))],
    ))
    .unwrap();

    let result = db
        .vector_search(vector_request(VectorMetric::Cosine, vec![1.0, 0.0]))
        .unwrap();
    assert_eq!(result.matches.len(), 1);
    assert_eq!(result.matches[0].entity.as_str(), "wanted");
}

#[test]
fn vector_upsert_clears_prior_dimension_and_metric_keys() {
    let db = memory_db();
    db.apply_unverified(batch(
        "vectors",
        "g1",
        "w1",
        vec![GraphMutation::UpsertEntity(vector_entity_with_dimension(
            "changing",
            vec![1.0, 0.0, 0.0],
            3,
            VectorMetric::Cosine,
        ))],
    ))
    .unwrap();
    db.apply_unverified(batch(
        "vectors",
        "g2",
        "w2",
        vec![GraphMutation::UpsertEntity(vector_entity_with_dimension(
            "changing",
            vec![1.0, 0.0],
            2,
            VectorMetric::Euclidean,
        ))],
    ))
    .unwrap();

    let stale = db
        .vector_search(VectorSearchRequest {
            namespace: namespace(),
            projection: projection("vectors"),
            property: GraphPropertyName::new("embedding").unwrap(),
            query: vec![1.0, 0.0, 0.0],
            dimension: 3,
            metric: VectorMetric::Cosine,
            limit: 10,
            cancellation: live(),
        })
        .unwrap();
    assert!(stale.matches.is_empty());
    let current_index = GraphVectorIndexRequest {
        namespace: namespace(),
        projection: projection("vectors"),
        property: GraphPropertyName::new("embedding").unwrap(),
        dimension: 2,
        metric: VectorMetric::Euclidean,
        cancellation: live(),
    };
    assert_eq!(
        db.vector_index_status(current_index.clone()).unwrap(),
        GraphVectorIndexStatus::Missing,
        "a new vector shape must wait for the retained index owner"
    );
    assert_eq!(
        db.ensure_vector_index(current_index).unwrap(),
        GraphVectorIndexStatus::Available
    );
    let current = db
        .vector_search(vector_request(VectorMetric::Euclidean, vec![1.0, 0.0]))
        .unwrap();
    assert_eq!(current.matches.len(), 1);
    assert_eq!(current.matches[0].entity.as_str(), "changing");
}

#[test]
fn repeated_scale_traversal_and_followup_write_remain_exact() {
    let db = memory_db();
    let mut mutations = Vec::new();
    for index in 0..256 {
        mutations.push(GraphMutation::UpsertEntity(entity(&format!("n{index:03}"))));
        if index > 0 {
            mutations.push(GraphMutation::UpsertRelation(relation(
                &format!("r{index:03}"),
                &format!("n{:03}", index - 1),
                &format!("n{index:03}"),
                "next",
            )));
        }
    }
    db.apply_unverified(batch("scale", "g1", "w1", mutations))
        .unwrap();
    let started = std::time::Instant::now();
    for _ in 0..32 {
        let mut request = traversal("n000");
        request.max_depth = 255;
        request.max_visits = 256;
        request.max_results = 256;
        assert_eq!(db.traverse(request).unwrap().visits.len(), 256);
    }
    let elapsed = started.elapsed();
    db.apply_unverified(batch(
        "scale",
        "g2",
        "w2",
        vec![
            GraphMutation::UpsertEntity(entity("n256")),
            GraphMutation::UpsertRelation(relation("r256", "n255", "n256", "next")),
        ],
    ))
    .unwrap();
    let mut request = traversal("n000");
    request.max_depth = 256;
    request.max_visits = 257;
    request.max_results = 257;
    assert_eq!(db.traverse(request).unwrap().visits.len(), 257);
    eprintln!("32 traversals across 256 nodes completed in {elapsed:?}");
}

#[test]
fn projection_replacement_preserves_cross_projection_target() {
    let db = memory_db();
    db.apply_unverified(batch(
        "facts",
        "g1",
        "w1",
        vec![GraphMutation::UpsertEntity(entity("shared"))],
    ))
    .unwrap();
    db.apply_unverified(batch(
        "code",
        "g2",
        "w2",
        vec![
            GraphMutation::UpsertEntity(entity("source")),
            GraphMutation::UpsertRelation(relation("link", "source", "shared", "refers")),
        ],
    ))
    .unwrap();
    db.replace_projection_unverified(ProjectionReplacement {
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

#[test]
fn conditional_projection_replacement_rejects_a_stale_source_snapshot() {
    let db = memory_db();
    db.replace_projection_unverified(ProjectionReplacement {
        namespace: namespace(),
        projection: projection("memory"),
        source_generation: generation("g1"),
        next_watermark: watermark("w1"),
        entities: vec![entity("current")],
        relations: Vec::new(),
        cancellation: live(),
    })
    .unwrap();

    let stale = db
        .replace_projection_unverified_if_current(
            ProjectionReplacement {
                namespace: namespace(),
                projection: projection("memory"),
                source_generation: generation("g2"),
                next_watermark: watermark("w2"),
                entities: vec![entity("stale")],
                relations: Vec::new(),
                cancellation: live(),
            },
            None,
        )
        .unwrap_err();
    assert_eq!(stale, GraphDbError::Conflict);
    assert!(db.traverse(traversal("current")).is_ok());
    assert!(matches!(
        db.traverse(traversal("stale")),
        Err(GraphDbError::InvalidRequest { .. })
    ));

    db.replace_projection_unverified_if_current(
        ProjectionReplacement {
            namespace: namespace(),
            projection: projection("memory"),
            source_generation: generation("g2"),
            next_watermark: watermark("w2"),
            entities: vec![entity("next")],
            relations: Vec::new(),
            cancellation: live(),
        },
        Some(&watermark("w1")),
    )
    .unwrap();
    assert!(db.traverse(traversal("next")).is_ok());

    db.replace_projection_unverified_if_current(
        ProjectionReplacement {
            namespace: namespace(),
            projection: projection("memory"),
            source_generation: generation("g3"),
            next_watermark: watermark("w3"),
            entities: Vec::new(),
            relations: Vec::new(),
            cancellation: live(),
        },
        Some(&watermark("w2")),
    )
    .unwrap();
    assert!(matches!(
        db.traverse(traversal("next")),
        Err(GraphDbError::InvalidRequest { .. })
    ));
}

fn publication(key: &str, expected: Option<&str>) -> GraphPublication {
    GraphPublication {
        namespace: namespace(),
        idempotency_key: GraphIdempotencyKey::new(key).unwrap(),
        input_digest: GraphPublicationInputDigest::new(format!("sha256:{}", "a".repeat(64)))
            .unwrap(),
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
    db.apply_unverified(batch(
        "code",
        "g1",
        "w1",
        vec![GraphMutation::UpsertEntity(entity("old"))],
    ))
    .unwrap();
    let error = db
        .replace_projection_unverified(ProjectionReplacement {
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
        .apply_unverified(batch(
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
    let first = db.publish_unverified(publication("event-1", None)).unwrap();
    let receipt = db
        .publication_receipt(
            &namespace(),
            &GraphIdempotencyKey::new("event-1").unwrap(),
            live(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(receipt.commit, first);
    assert_eq!(receipt.digest.as_str().len(), 64);
    assert_eq!(
        receipt.input_digest.as_str(),
        format!("sha256:{}", "a".repeat(64))
    );
    let second = db.publish_unverified(publication("event-1", None)).unwrap();
    assert_eq!(first, second);
    assert!(
        db.publication_receipt(
            &namespace(),
            &GraphIdempotencyKey::new("missing-event").unwrap(),
            live(),
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(
        db.publication_receipt(
            &namespace(),
            &GraphIdempotencyKey::new("event-1").unwrap(),
            Arc::new(Cancelled),
        )
        .unwrap_err(),
        GraphDbError::Cancelled
    );
}

#[test]
fn publication_changed_input_and_stale_watermark_conflict() {
    let db = memory_db();
    db.publish_unverified(publication("event-1", None)).unwrap();
    let mut changed = publication("event-1", None);
    changed.next_watermark = watermark("w2");
    changed.batch.next_watermark = watermark("w2");
    assert_eq!(
        db.publish_unverified(changed).unwrap_err(),
        GraphDbError::Conflict
    );
    assert_eq!(
        db.publish_unverified(publication("event-2", Some("stale")))
            .unwrap_err(),
        GraphDbError::Conflict
    );
}

#[test]
fn persistent_close_and_reopen_preserves_graph_and_vector() {
    let temp = TempDir::new().unwrap();
    let (registered, db) = RegisteredGraph::open_lease(temp.path()).unwrap();
    db.apply_unverified(batch(
        "vectors",
        "g1",
        "w1",
        vec![
            GraphMutation::UpsertEntity(vector_entity("a", vec![1.0, 0.0], VectorMetric::Cosine)),
            GraphMutation::UpsertEntity(entity("b")),
            GraphMutation::UpsertRelation(relation("ab", "a", "b", "calls")),
        ],
    ))
    .unwrap();
    drop(db);
    registered.close().unwrap();

    let reopened = registered.reopen_lease().unwrap();
    assert_eq!(reopened.traverse(traversal("a")).unwrap().visits.len(), 2);
    let index = GraphVectorIndexRequest {
        namespace: namespace(),
        projection: projection("vectors"),
        property: GraphPropertyName::new("embedding").unwrap(),
        dimension: 2,
        metric: VectorMetric::Cosine,
        cancellation: live(),
    };
    assert_eq!(
        reopened.vector_index_status(index.clone()).unwrap(),
        GraphVectorIndexStatus::Missing
    );
    assert_eq!(
        reopened.ensure_vector_index(index).unwrap(),
        GraphVectorIndexStatus::Available
    );
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
fn large_vector_corpus_reopens_without_synchronous_index_rebuild() {
    let temp = TempDir::new().unwrap();
    let (registered, db) = RegisteredGraph::open_lease(temp.path()).unwrap();
    let vectors = (0..2_049)
        .map(|ordinal| {
            GraphMutation::UpsertEntity(vector_entity(
                &format!("vector-{ordinal:04}"),
                vec![ordinal as f32, ordinal as f32],
                VectorMetric::Euclidean,
            ))
        })
        .collect();
    db.apply_unverified(batch("vectors", "g1", "w1", vectors))
        .unwrap();
    drop(db);
    registered.close().unwrap();

    let admission_started = Instant::now();
    let reopened = registered.reopen_lease().unwrap();
    let admission_elapsed = admission_started.elapsed();
    assert!(
        admission_elapsed < Duration::from_secs(5),
        "opening a 2,049-vector graph took {admission_elapsed:?}"
    );
    assert_eq!(
        reopened
            .vector_index_status(GraphVectorIndexRequest {
                namespace: namespace(),
                projection: projection("vectors"),
                property: GraphPropertyName::new("embedding").unwrap(),
                dimension: 2,
                metric: VectorMetric::Euclidean,
                cancellation: live(),
            })
            .unwrap(),
        GraphVectorIndexStatus::Missing,
        "GraphDb admission must not synchronously rebuild a corpus index"
    );
}

#[test]
fn vector_write_after_reopen_leaves_index_activation_to_background_owner() {
    let temp = TempDir::new().unwrap();
    let (registered, db) = RegisteredGraph::open_lease(temp.path()).unwrap();
    db.apply_unverified(batch(
        "vectors",
        "g1",
        "w1",
        vec![GraphMutation::UpsertEntity(vector_entity(
            "before",
            vec![1.0, 0.0],
            VectorMetric::Cosine,
        ))],
    ))
    .unwrap();
    drop(db);
    registered.close().unwrap();

    let reopened = registered.reopen_lease().unwrap();
    let index = GraphVectorIndexRequest {
        namespace: namespace(),
        projection: projection("vectors"),
        property: GraphPropertyName::new("embedding").unwrap(),
        dimension: 2,
        metric: VectorMetric::Cosine,
        cancellation: live(),
    };
    assert_eq!(
        reopened.vector_index_status(index.clone()).unwrap(),
        GraphVectorIndexStatus::Missing
    );
    reopened
        .apply_unverified(batch(
            "vectors",
            "g2",
            "w2",
            vec![GraphMutation::UpsertEntity(vector_entity(
                "after",
                vec![0.0, 1.0],
                VectorMetric::Cosine,
            ))],
        ))
        .unwrap();
    assert_eq!(
        reopened.vector_index_status(index.clone()).unwrap(),
        GraphVectorIndexStatus::Missing,
        "ordinary writes must not synchronously rebuild a missing corpus index"
    );
    assert_eq!(
        reopened.ensure_vector_index(index).unwrap(),
        GraphVectorIndexStatus::Available
    );
    assert_eq!(
        reopened
            .vector_search(vector_request(VectorMetric::Cosine, vec![0.0, 1.0]))
            .unwrap()
            .matches[0]
            .entity
            .as_str(),
        "after"
    );
}

#[test]
fn publication_state_survives_reopen() {
    let temp = TempDir::new().unwrap();
    let (registered, db) = RegisteredGraph::open_lease(temp.path()).unwrap();
    let first = db.publish_unverified(publication("event-1", None)).unwrap();
    drop(db);
    registered.close().unwrap();
    let reopened = registered.reopen_lease().unwrap();
    assert_eq!(
        reopened
            .publish_unverified(publication("event-1", None))
            .unwrap(),
        first
    );
}

#[test]
fn valid_foreign_grafeo_store_requires_reset() {
    let temp = TempDir::new().unwrap();
    let path = graph_path(temp.path());
    let raw = grafeo_engine::GrafeoDB::with_config(
        grafeo_engine::Config::persistent(&path)
            .with_storage_format(grafeo_engine::config::StorageFormat::SingleFile),
    )
    .unwrap();
    raw.session().create_node(&["foreign"]);
    raw.close().unwrap();
    let error = RegisteredGraph::open_lease(temp.path()).err().unwrap();
    assert!(matches!(error, GraphDbError::ResetRequired { .. }));
}

#[test]
fn wrong_tracedecay_format_requires_reset() {
    let temp = TempDir::new().unwrap();
    let path = graph_path(temp.path());
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
    let error = RegisteredGraph::open_lease(temp.path()).err().unwrap();
    assert!(matches!(error, GraphDbError::ResetRequired { .. }));
}

#[test]
fn closed_handle_fails_typed() {
    let owner = GraphDbOwner::memory(live()).unwrap();
    let db = owner.issue_lease().unwrap();
    owner.close().unwrap();
    assert_eq!(
        db.apply_unverified(batch("code", "g1", "w1", Vec::new()))
            .unwrap_err(),
        GraphDbError::Closed
    );
}
