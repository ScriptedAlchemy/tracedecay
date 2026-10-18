use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tracedecay_domain::{CopyProofV1, ObservationId, UserProfileId};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

use super::*;
use crate::relations::test_support::occurrence_id_for_test;
use crate::relations::{
    LogicalCopyRelation, SessionRelationGraphStore, SessionRelationProjection,
    SessionRelationScope, SummaryRelationNode, SummarySourceRef,
};

const PROJECTION_CHECK_SPAN: &str = "graph_db.projection.telemetry";
const OUTGOING_SPAN: &str = "graph_db.traversal.outgoing";

#[derive(Default)]
struct RelationCallCensus {
    projection_checks: AtomicU64,
    outgoing_reads: AtomicU64,
}

struct RelationCallSubscriber(Arc<RelationCallCensus>);

impl Subscriber for RelationCallSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, span: &Attributes<'_>) -> Id {
        match span.metadata().name() {
            PROJECTION_CHECK_SPAN => self.0.projection_checks.fetch_add(1, Ordering::Relaxed),
            OUTGOING_SPAN => self.0.outgoing_reads.fetch_add(1, Ordering::Relaxed),
            _ => 0,
        };
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, _event: &Event<'_>) {}

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

/// Relation hydration result plus the `(projection checks, outgoing reads)`
/// it issued against the relation graph.
type CountedRelations = (Result<RecordRelationBatch, TemporalPortError>, (u64, u64));

fn hydrate_counted(
    store: &SessionRelationGraphStore,
    snapshot: &TemporalExecutionSnapshot,
    candidates: &[RankingCandidate],
    candidate_offset: usize,
    request: &PageRequest,
) -> CountedRelations {
    let census = Arc::new(RelationCallCensus::default());
    let _guard = tracedecay_runtime_core::logging::set_tracing_capture(RelationCallSubscriber(
        Arc::clone(&census),
    ));
    let relations = load_record_relations(
        store,
        &scope(),
        snapshot.retrieval_scope(),
        snapshot,
        candidates,
        candidate_offset,
        request,
    );
    (
        relations,
        (
            census.projection_checks.load(Ordering::Relaxed),
            census.outgoing_reads.load(Ordering::Relaxed),
        ),
    )
}

fn scope() -> SessionRelationScope {
    SessionRelationScope::profile_sessions(UserProfileId::new("profile.fixture").expect("profile"))
}

fn copy(occurrence: &str, source: &str) -> LogicalCopyRelation {
    let source = occurrence_id_for_test(source);
    LogicalCopyRelation {
        occurrence_id: occurrence_id_for_test(occurrence),
        copied_from_occurrence_id: source.clone(),
        proof: CopyProofV1::ProviderLinkage {
            source_occurrence_id: source,
            provider_record_id: ObservationId::new("batch-proof").expect("proof"),
        },
        knowledge_at: UtcMicros(5),
        valid_time: TemporalValidityV1::Unknown,
    }
}

fn store_with(
    projections: Vec<(
        &str,
        u64,
        Vec<SummaryRelationNode>,
        Vec<LogicalCopyRelation>,
    )>,
) -> SessionRelationGraphStore {
    let store = crate::relations::memory_relation_store();
    for (session_id, generation, summaries, logical_copies) in projections {
        store
            .replace(&SessionRelationProjection {
                scope: scope(),
                session_id: SessionId::new(session_id).expect("session"),
                generation,
                summaries,
                logical_copies,
                thread_hierarchy: Vec::new(),
                agent_hierarchy: Vec::new(),
                parent_session_id: None,
                workflow_agents: Vec::new(),
            })
            .expect("relation projection");
    }
    store
}

fn occurrence(session_id: &str, source: &str, generation: u64, seed: &str) -> RankingCandidate {
    let mut candidate = candidate_for_anchor(&format!("anchor-{seed}"));
    candidate.retriever_record_id = occurrence_id_for_test(seed).to_string();
    candidate.session = Some(session_id.to_string());
    candidate.source = Some(source.to_string());
    candidate.participant_generation = generation;
    candidate
}

/// Two sessions frozen at different generations under one authorized root.
fn two_session_snapshot() -> TemporalExecutionSnapshot {
    root_snapshot_with_mode(99, None, TemporalModeV1::Forensic)
        .with_participant_manifest(
            TemporalParticipantManifest::new(vec![
                participant("session-a", "claude", 1),
                participant("session-b", "codex", 2),
            ])
            .expect("participant manifest"),
        )
        .expect("root snapshot")
}

/// A production-sized record page: 64 items plus the lookahead candidate.
fn full_page_request() -> PageRequest {
    PageRequest::for_test(64, 64 * 1024, 8 * 1024, 64, 512)
}

/// Relation budget of three: `page_item_limit` two plus the lookahead.
fn three_relation_request() -> PageRequest {
    PageRequest::for_test(2, 64 * 1024, 8 * 1024, 2, 512)
}

fn exhausted(limit: u64) -> TemporalPortError {
    TemporalPortError::BudgetExceeded {
        resource: "record relations",
        accounting: Some(ReadBudgetAccounting::consumed_with_more(limit, limit)),
    }
}

const REFUSED: TemporalPortError = TemporalPortError::BudgetExceeded {
    resource: "record relations",
    accounting: None,
};

#[test]
fn a_same_session_window_reads_relations_once_for_its_group() {
    let store = store_with(vec![("session-snapshot", 1, Vec::new(), Vec::new())]);
    let snapshot = scoped_snapshot(1, None);
    let candidates = (0..65)
        .map(|index| occurrence("session-snapshot", "claude", 1, &format!("window-{index}")))
        .collect::<Vec<_>>();

    let (relations, calls) =
        hydrate_counted(&store, &snapshot, &candidates, 0, &full_page_request());

    let relations = relations.expect("applied-empty relations");
    assert!(relations.copy_candidates().is_empty());
    assert_eq!(
        calls,
        (1, 1),
        "65 same-session candidates must share one projection check and one outgoing read"
    );
}

#[test]
fn a_mixed_window_matches_per_candidate_hydration_with_one_read_per_group() {
    let summary = SummaryRelationNode {
        summary_id: "summary-a".to_string(),
        sources: vec![SummarySourceRef::Anchor {
            anchor_id: RetrievalAnchorId::new("anchor-summarized").expect("anchor"),
        }],
        predecessor_summary_id: None,
    };
    let store = store_with(vec![
        (
            "session-a",
            1,
            vec![summary],
            vec![copy("a-copied", "a-source"), copy("a-twice", "a-source")],
        ),
        (
            "session-b",
            2,
            Vec::new(),
            vec![copy("b-copied", "b-one"), copy("b-copied", "b-two")],
        ),
    ]);
    let snapshot = two_session_snapshot();
    let mut summary_candidate = occurrence("session-a", "claude", 1, "summary");
    summary_candidate.channel = CandidateChannel::Summary;
    summary_candidate.retriever_record_id = "summary-a".to_string();
    let mut span_candidate = occurrence("session-b", "codex", 2, "span");
    span_candidate.channel = CandidateChannel::Span;
    let candidates = vec![
        occurrence("session-a", "claude", 1, "a-copied"),
        occurrence("session-b", "codex", 2, "b-plain"),
        span_candidate,
        summary_candidate.clone(),
        occurrence("session-b", "codex", 2, "b-copied"),
        occurrence("session-a", "claude", 1, "a-plain"),
        occurrence("session-a", "claude", 1, "a-twice"),
        occurrence("session-a", "claude", 1, "a-copied"),
    ];
    let request = full_page_request();
    let offset = 7;

    let (window, calls) = hydrate_counted(&store, &snapshot, &candidates, offset, &request);
    let window = window.expect("window relations");

    let mut per_candidate = RecordRelationBatch::empty();
    for (local, candidate) in candidates.iter().enumerate() {
        let (single, _) = hydrate_counted(
            &store,
            &snapshot,
            std::slice::from_ref(candidate),
            offset + local,
            &request,
        );
        per_candidate.append(single.expect("single candidate relations"));
    }
    assert_eq!(format!("{window:?}"), format!("{per_candidate:?}"));
    assert_eq!(window.copy_candidates(), vec![7, 11, 11, 13, 14]);

    let (_, summary_calls) = hydrate_counted(&store, &snapshot, &[summary_candidate], 0, &request);
    assert_eq!(
        calls,
        (2 + summary_calls.0, 2 + summary_calls.1),
        "occurrences read once per (session, generation) group, summaries keep their own reads"
    );
}

#[test]
fn copies_filling_the_budget_refuse_a_later_copy_free_occurrence() {
    let store = store_with(vec![(
        "session-snapshot",
        1,
        Vec::new(),
        vec![
            copy("full", "one"),
            copy("full", "two"),
            copy("full", "three"),
        ],
    )]);
    let candidates = [
        occurrence("session-snapshot", "claude", 1, "full"),
        occurrence("session-snapshot", "claude", 1, "empty"),
    ];

    let (relations, _) = hydrate_counted(
        &store,
        &scoped_snapshot(1, None),
        &candidates,
        0,
        &three_relation_request(),
    );

    assert_eq!(relations.expect_err("budget boundary"), exhausted(3));
}

#[test]
fn an_interleaved_group_overflow_refuses_at_the_overflowing_candidate() {
    let store = store_with(vec![
        (
            "session-a",
            1,
            Vec::new(),
            vec![
                copy("a-first", "one"),
                copy("a-second", "two"),
                copy("a-second", "three"),
            ],
        ),
        ("session-b", 2, Vec::new(), vec![copy("b-first", "one")]),
    ]);
    let candidates = [
        occurrence("session-a", "claude", 1, "a-first"),
        occurrence("session-b", "codex", 2, "b-first"),
        occurrence("session-a", "claude", 1, "a-second"),
    ];

    let (relations, _) = hydrate_counted(
        &store,
        &two_session_snapshot(),
        &candidates,
        0,
        &three_relation_request(),
    );

    assert_eq!(relations.expect_err("interleaved overflow"), REFUSED);
}

#[test]
fn a_group_past_the_budget_refuses_like_its_overflowing_candidate() {
    let store = store_with(vec![(
        "session-snapshot",
        1,
        Vec::new(),
        vec![
            copy("first", "one"),
            copy("first", "two"),
            copy("second", "one"),
            copy("second", "two"),
        ],
    )]);
    let candidates = [
        occurrence("session-snapshot", "claude", 1, "first"),
        occurrence("session-snapshot", "claude", 1, "second"),
    ];

    let (relations, _) = hydrate_counted(
        &store,
        &scoped_snapshot(1, None),
        &candidates,
        0,
        &three_relation_request(),
    );

    assert_eq!(relations.expect_err("group overflow"), REFUSED);
}

#[test]
fn an_absent_group_receipt_fails_while_an_applied_empty_group_reads() {
    let store = store_with(vec![("session-a", 1, Vec::new(), Vec::new())]);
    let snapshot = two_session_snapshot();
    let applied = [
        occurrence("session-a", "claude", 1, "a-one"),
        occurrence("session-a", "claude", 1, "a-two"),
    ];
    let (relations, _) = hydrate_counted(&store, &snapshot, &applied, 0, &full_page_request());
    assert!(
        relations
            .expect("applied-empty group")
            .copy_candidates()
            .is_empty()
    );

    let absent = [
        occurrence("session-a", "claude", 1, "a-one"),
        occurrence("session-b", "codex", 2, "b-one"),
        occurrence("session-b", "codex", 2, "b-two"),
    ];
    let (relations, _) = hydrate_counted(&store, &snapshot, &absent, 0, &full_page_request());
    assert!(matches!(
        relations.expect_err("absent receipt"),
        TemporalPortError::Read { .. }
    ));
}

#[test]
fn a_planning_failure_surfaces_after_earlier_candidates_read() {
    let store = store_with(vec![(
        "session-snapshot",
        1,
        Vec::new(),
        vec![
            copy("full", "one"),
            copy("full", "two"),
            copy("full", "three"),
            copy("full", "four"),
        ],
    )]);
    let mut malformed = occurrence("session-snapshot", "claude", 1, "malformed");
    malformed.retriever_record_id = "not-an-occurrence".to_string();
    let candidates = [
        occurrence("session-snapshot", "claude", 1, "full"),
        malformed,
    ];

    let (relations, _) = hydrate_counted(
        &store,
        &scoped_snapshot(1, None),
        &candidates,
        0,
        &three_relation_request(),
    );

    assert_eq!(
        relations.expect_err("earlier overflow wins"),
        REFUSED,
        "the earlier candidate's refusal precedes the later malformed identity"
    );
}

#[test]
#[ignore = "record-relation hydration latency probe; run with --ignored --nocapture"]
fn record_relation_hydration_latency_probe() {
    let copies = (0..16)
        .map(|index| copy(&format!("probe-{}", index * 4), &format!("source-{index}")))
        .collect();
    let store = store_with(vec![("session-snapshot", 1, Vec::new(), copies)]);
    let snapshot = scoped_snapshot(1, None);
    let candidates = (0..65)
        .map(|index| occurrence("session-snapshot", "claude", 1, &format!("probe-{index}")))
        .collect::<Vec<_>>();
    let request = full_page_request();
    let rounds = 500_u32;
    let started = Instant::now();
    for _ in 0..rounds {
        load_record_relations(
            &store,
            &scope(),
            snapshot.retrieval_scope(),
            &snapshot,
            &candidates,
            0,
            &request,
        )
        .expect("probe relations");
    }
    let elapsed = started.elapsed();
    eprintln!(
        "record relation hydration: 65 candidates, 16 copies, {rounds} rounds, {:?} total, {:?} per page",
        elapsed,
        elapsed / rounds
    );
}
