use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphBudgetKind, GraphCancellation, GraphDbError, GraphDbLeaseV1, GraphDbOwner, GraphEntity,
    GraphEntityId, GraphIdempotencyKey, GraphLabel, GraphMutation, GraphNamespace,
    GraphProjectionId, GraphPublication, GraphPublicationInputDigest, GraphRelation,
    GraphRelationId, GraphRelationKind, GraphTraversalDirection, GraphWatermark, GraphWriteBatch,
    NeverCancelled, ProjectionReplacement, SourceGeneration, TraversalRequest,
};

use crate::support;

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
    let targets = db
        .outgoing_relation_targets(&namespace(), &starts, &kinds, 1, live())
        .unwrap();
    assert_eq!(targets[0][0].relation.identity.as_str(), "ab");
    assert_eq!(targets[0][0].target.identity.as_str(), "b");
    assert!(targets[1].is_empty());
    assert!(targets[2].is_empty());
    assert_eq!(
        db.outgoing_relation_ids(&namespace(), &starts, &kinds, 0, live())
            .unwrap_err(),
        GraphDbError::budget_exhausted(GraphBudgetKind::Read, 0)
    );
    assert_eq!(
        db.outgoing_relations(&namespace(), &starts, &kinds, 1, Arc::new(Cancelled))
            .unwrap_err(),
        GraphDbError::Cancelled
    );
}

#[test]
fn wide_fanout_refuse_errors_and_truncate_returns_the_prefix() {
    let db = memory_db();
    let mut mutations = vec![GraphMutation::UpsertEntity(entity("hub"))];
    for index in 0..32 {
        let spoke = format!("spoke-{index:02}");
        mutations.push(GraphMutation::UpsertEntity(entity(&spoke)));
        mutations.push(GraphMutation::UpsertRelation(relation(
            &format!("edge-{index:02}"),
            "hub",
            &spoke,
            "calls",
        )));
    }
    db.apply_unverified(batch("code", "g1", "w1", mutations))
        .unwrap();
    let starts = [entity_id("hub")];
    let kinds = BTreeSet::from([GraphRelationKind::new("calls").unwrap()]);
    assert_eq!(
        db.outgoing_relations(&namespace(), &starts, &kinds, 8, live())
            .unwrap_err(),
        GraphDbError::budget_exhausted(GraphBudgetKind::Read, 8)
    );
    let truncated = db
        .outgoing_relations_truncated(&namespace(), &starts, &kinds, 8, live())
        .unwrap();
    assert_eq!(truncated.len(), 1);
    assert_eq!(truncated[0].len(), 8);
}

#[test]
fn outgoing_target_visitor_streams_rows_and_observes_cancellation() {
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
    let kinds = BTreeSet::from([GraphRelationKind::new("calls").unwrap()]);
    let mut visited = Vec::new();
    let count = db
        .visit_outgoing_relation_targets(
            &namespace(),
            &entity_id("a"),
            &kinds,
            live(),
            &mut |target| {
                visited.push((
                    target.relation.identity.as_str().to_owned(),
                    target.target.identity.as_str().to_owned(),
                ));
            },
        )
        .unwrap();
    visited.sort();
    assert_eq!(count, 2);
    assert_eq!(
        visited,
        vec![
            ("ab".to_owned(), "b".to_owned()),
            ("ac".to_owned(), "c".to_owned()),
        ]
    );

    let mut visited_before_cancel = 0_usize;
    let error = db
        .visit_outgoing_relation_targets(
            &namespace(),
            &entity_id("a"),
            &kinds,
            Arc::new(CancelOnPoll::new(3)),
            &mut |_| visited_before_cancel += 1,
        )
        .unwrap_err();
    assert_eq!(error, GraphDbError::Cancelled);
    assert_eq!(visited_before_cancel, 1);
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
    assert!(matches!(stale, GraphDbError::Conflict { .. }));
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
    assert!(matches!(
        db.publish_unverified(changed).unwrap_err(),
        GraphDbError::Conflict { .. }
    ));
    assert!(matches!(
        db.publish_unverified(publication("event-2", Some("stale")))
            .unwrap_err(),
        GraphDbError::Conflict { .. }
    ));
}

#[test]
fn persistent_close_and_reopen_preserves_graph() {
    let temp = TempDir::new().unwrap();
    let (registered, db) = RegisteredGraph::open_lease(temp.path()).unwrap();
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
    drop(db);
    registered.close().unwrap();

    let reopened = registered.reopen_lease().unwrap();
    assert_eq!(reopened.traverse(traversal("a")).unwrap().visits.len(), 2);
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
