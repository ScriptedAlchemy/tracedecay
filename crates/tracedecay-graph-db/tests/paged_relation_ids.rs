//! Operation-count contract for ID-only, cursor-aware relation fan-out (#801).
//!
//! These tests favor decode/lock/scan counts over wall clock. A star whose
//! fan-out dwarfs the page size makes an O(fanout) hydrate or per-edge
//! quarantine lock fail loudly.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_graph_db::{
    GraphBudgetKind, GraphCancellation, GraphDbError, GraphDbOwner, GraphEntity, GraphEntityId,
    GraphMutation, GraphNamespace, GraphProjectionId, GraphProperty, GraphPropertyName,
    GraphRelation, GraphRelationId, GraphRelationKind, GraphWatermark, GraphWriteBatch,
    NeverCancelled, SourceGeneration, take_graph_db_traversal_counters,
};

const PAGE: usize = 10;
const STAR: usize = 2_048;
const STAR_100K: usize = 100_000;

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

fn kinds() -> BTreeSet<GraphRelationKind> {
    BTreeSet::from([GraphRelationKind::new("calls").unwrap()])
}

fn memory_db() -> tracedecay_graph_db::GraphDbLeaseV1 {
    GraphDbOwner::memory(live()).unwrap().issue_lease().unwrap()
}

fn entity(value: &str) -> GraphEntity {
    GraphEntity::new(entity_id(value), BTreeSet::new(), BTreeMap::new()).unwrap()
}

fn fat_relation(value: &str, from: &str, to: &str) -> GraphRelation {
    let mut properties = BTreeMap::new();
    properties.insert(
        GraphPropertyName::new("payload").unwrap(),
        GraphProperty::String("x".repeat(64)),
    );
    GraphRelation::new(
        relation_id(value),
        entity_id(from),
        entity_id(to),
        GraphRelationKind::new("calls").unwrap(),
        properties,
    )
    .unwrap()
}

fn apply_star(
    db: &tracedecay_graph_db::GraphDbLeaseV1,
    spokes: usize,
    incoming: bool,
) -> GraphEntityId {
    let hub = "hub";
    let mut mutations = vec![GraphMutation::UpsertEntity(entity(hub))];
    let mut batch_no = 0_usize;
    let flush = |batch_no: &mut usize, mutations: &mut Vec<GraphMutation>| {
        if mutations.is_empty() {
            return;
        }
        *batch_no += 1;
        db.apply_unverified(
            GraphWriteBatch::new(
                namespace(),
                projection("code"),
                SourceGeneration::new(format!("g{batch_no}")).unwrap(),
                GraphWatermark::new(format!("w{batch_no}")).unwrap(),
                std::mem::take(mutations),
                live(),
            )
            .unwrap(),
        )
        .unwrap();
    };
    for index in 0..spokes {
        let spoke = format!("spoke-{index:06}");
        let edge = format!("edge-{index:06}");
        mutations.push(GraphMutation::UpsertEntity(entity(&spoke)));
        mutations.push(GraphMutation::UpsertRelation(if incoming {
            fat_relation(&edge, &spoke, hub)
        } else {
            fat_relation(&edge, hub, &spoke)
        }));
        if mutations.len() >= 3_000 {
            flush(&mut batch_no, &mut mutations);
        }
    }
    flush(&mut batch_no, &mut mutations);
    entity_id(hub)
}

fn expected_edge_ids(spokes: usize) -> Vec<GraphRelationId> {
    (0..spokes)
        .map(|index| relation_id(&format!("edge-{index:06}")))
        .collect()
}

#[test]
fn id_only_fanout_skips_property_decode_and_snapshots_quarantine() {
    let db = memory_db();
    let hub = apply_star(&db, STAR, false);
    let starts = [hub];
    let _ = take_graph_db_traversal_counters();

    let ids = db
        .outgoing_relation_ids(&namespace(), &starts, &kinds(), STAR, live())
        .unwrap();
    assert_eq!(ids[0].len(), STAR);
    assert_eq!(ids[0], expected_edge_ids(STAR));

    let counts = take_graph_db_traversal_counters();
    assert_eq!(
        counts.property_decodes, 0,
        "ID-only fan-out must not decode relation properties; observed {}",
        counts.property_decodes
    );
    assert!(
        counts.quarantine_lock_acquisitions <= 1,
        "quarantine must be snapshotted once per distinct projection; observed {}",
        counts.quarantine_lock_acquisitions
    );
    assert!(
        counts.relation_identity_decodes > 0,
        "ID-only decode must run for traversed edges"
    );
}

#[test]
fn incoming_id_only_fanout_matches_outgoing_counts() {
    let db = memory_db();
    let hub = apply_star(&db, STAR, true);
    let starts = [hub];
    let _ = take_graph_db_traversal_counters();

    let ids = db
        .incoming_relation_ids(&namespace(), &starts, &kinds(), STAR, live())
        .unwrap();
    assert_eq!(ids[0], expected_edge_ids(STAR));

    let counts = take_graph_db_traversal_counters();
    assert_eq!(counts.property_decodes, 0);
    assert!(counts.quarantine_lock_acquisitions <= 1);
}

#[test]
fn label_keys_scan_once_per_store_epoch() {
    let db = memory_db();
    let hub = apply_star(&db, 32, false);
    let starts = [hub];
    let _ = take_graph_db_traversal_counters();

    db.outgoing_relation_ids(&namespace(), &starts, &kinds(), 32, live())
        .unwrap();
    let first = take_graph_db_traversal_counters();
    assert!(
        first.label_universe_scans >= 1,
        "first fan-out must expand labels"
    );

    db.outgoing_relation_ids(&namespace(), &starts, &kinds(), 32, live())
        .unwrap();
    let second = take_graph_db_traversal_counters();
    assert_eq!(
        second.label_universe_scans, 0,
        "same epoch must reuse the label expansion; observed {}",
        second.label_universe_scans
    );

    db.apply_unverified(
        GraphWriteBatch::new(
            namespace(),
            projection("code"),
            SourceGeneration::new("g-invalidate").unwrap(),
            GraphWatermark::new("w-invalidate").unwrap(),
            vec![GraphMutation::UpsertEntity(entity("extra"))],
            live(),
        )
        .unwrap(),
    )
    .unwrap();
    let _ = take_graph_db_traversal_counters();
    db.outgoing_relation_ids(&namespace(), &starts, &kinds(), 32, live())
        .unwrap();
    let after_write = take_graph_db_traversal_counters();
    assert!(
        after_write.label_universe_scans >= 1,
        "a write must invalidate the epoch-scoped label cache"
    );
}

#[test]
fn paged_star_is_stable_cursor_ordered_and_page_bounded() {
    let db = memory_db();
    let hub = apply_star(&db, STAR, false);
    let starts = [hub];
    let expected = expected_edge_ids(STAR);
    let _ = take_graph_db_traversal_counters();

    let page1 = db
        .outgoing_relation_ids_page(&namespace(), &starts, &kinds(), None, PAGE, live())
        .unwrap();
    let page1_counts = take_graph_db_traversal_counters();
    assert_eq!(page1[0], expected[..PAGE]);
    assert_eq!(page1_counts.property_decodes, 0);
    assert!(page1_counts.quarantine_lock_acquisitions <= 1);
    assert!(
        page1_counts.relation_identity_decodes <= STAR as u64,
        "page 1 may walk the frontier once to order identities"
    );

    let page2 = db
        .outgoing_relation_ids_page(
            &namespace(),
            &starts,
            &kinds(),
            page1[0].last(),
            PAGE,
            live(),
        )
        .unwrap();
    let page2_counts = take_graph_db_traversal_counters();
    assert_eq!(page2[0], expected[PAGE..PAGE * 2]);
    assert_eq!(page2_counts.property_decodes, 0);
    assert_eq!(
        page2_counts.quarantine_lock_acquisitions, 0,
        "page 2 must reuse the request/epoch approval, not re-lock per edge"
    );
    assert_eq!(
        page2_counts.relation_identity_decodes, 0,
        "page 2 must answer from the epoch adjacency index, not re-decode the star"
    );
    assert!(
        page2_counts.adjacency_index_hits >= 1,
        "page 2 must hit the epoch-scoped adjacency index"
    );
    assert_eq!(page2_counts.label_universe_scans, 0);
}

#[test]
fn star_100k_page_work_stays_page_plus_frontier() {
    let db = memory_db();
    let hub = apply_star(&db, STAR_100K, true);
    let starts = [hub];
    let expected = expected_edge_ids(STAR_100K);
    let _ = take_graph_db_traversal_counters();

    let page1 = db
        .incoming_relation_ids_page(&namespace(), &starts, &kinds(), None, PAGE, live())
        .unwrap();
    let page1_counts = take_graph_db_traversal_counters();
    assert_eq!(page1[0], expected[..PAGE]);
    assert_eq!(page1_counts.property_decodes, 0);
    assert!(page1_counts.quarantine_lock_acquisitions <= 1);
    assert!(page1_counts.relation_identity_decodes <= STAR_100K as u64);

    let page2 = db
        .incoming_relation_ids_page(
            &namespace(),
            &starts,
            &kinds(),
            page1[0].last(),
            PAGE,
            live(),
        )
        .unwrap();
    let page2_counts = take_graph_db_traversal_counters();
    assert_eq!(page2[0], expected[PAGE..PAGE * 2]);
    assert_eq!(page2_counts.property_decodes, 0);
    assert_eq!(page2_counts.relation_identity_decodes, 0);
    assert_eq!(page2_counts.quarantine_lock_acquisitions, 0);
    assert!(page2_counts.adjacency_index_hits >= 1);
}

#[test]
fn paged_ids_honor_cancellation_and_refuse_over_budget_complete_reads() {
    let db = memory_db();
    let hub = apply_star(&db, 32, false);
    let starts = [hub];

    assert_eq!(
        db.outgoing_relation_ids(&namespace(), &starts, &kinds(), 8, live())
            .unwrap_err(),
        GraphDbError::budget_exhausted(GraphBudgetKind::Read, 8)
    );
    assert_eq!(
        db.outgoing_relation_ids_page(
            &namespace(),
            &starts,
            &kinds(),
            None,
            PAGE,
            Arc::new(Cancelled)
        )
        .unwrap_err(),
        GraphDbError::Cancelled
    );
    assert_eq!(
        db.outgoing_relation_ids_page(
            &namespace(),
            &starts,
            &kinds(),
            None,
            PAGE,
            Arc::new(CancelOnPoll::new(1))
        )
        .unwrap_err(),
        GraphDbError::Cancelled
    );
}

#[test]
fn paged_ids_refuse_foreign_namespace_and_absent_start() {
    let db = memory_db();
    let hub = apply_star(&db, 8, false);
    let missing = db
        .outgoing_relation_ids_page(
            &namespace(),
            &[entity_id("missing")],
            &kinds(),
            None,
            PAGE,
            live(),
        )
        .unwrap();
    assert_eq!(missing, vec![Vec::<GraphRelationId>::new()]);

    let owns = BTreeSet::from([GraphRelationKind::new("owns").unwrap()]);
    let filtered = db
        .outgoing_relation_ids_page(&namespace(), &[hub], &owns, None, PAGE, live())
        .unwrap();
    assert_eq!(
        filtered,
        vec![Vec::<GraphRelationId>::new()],
        "a kind filter must not leak another kind's identities"
    );
}
