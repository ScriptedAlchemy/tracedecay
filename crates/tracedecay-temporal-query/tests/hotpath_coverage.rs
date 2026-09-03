//! Runtime coverage for temporal candidate planning and ranking labels.

use tracedecay_domain::RetrievalAnchorId;
use tracedecay_temporal_query::candidates::CandidateChannel;
use tracedecay_temporal_query::plan_temporal_candidates;
use tracedecay_temporal_query::ranking::{DiversityLimits, RankingCandidate, rank_candidates};

#[cfg(feature = "hotpath")]
#[path = "../../../tests/hotpath_report_support.rs"]
mod hotpath_report_support;

#[cfg(feature = "hotpath")]
const EXPECTED_LABELS: &[&str] = &[
    "temporal.candidates.plan_scope",
    "temporal.candidates.plan_text",
    "temporal.rank",
];

fn exercise_temporal_query() {
    let scope_plan = plan_temporal_candidates("", None, false);
    assert!(scope_plan.contains(CandidateChannel::Scope, ""));

    let text_plan = plan_temporal_candidates("cargo test 2026-07-18", None, false);
    assert!(!text_plan.clauses().is_empty());

    let anchor = RetrievalAnchorId::new("anchor.hotpath-coverage").expect("valid anchor id");
    let ranked = rank_candidates(
        &[RankingCandidate {
            stable_id: "candidate.hotpath-coverage".into(),
            anchor_id: anchor,
            retriever_record_id: "record.hotpath-coverage".into(),
            channel: CandidateChannel::Lexical,
            raw_score: 10,
            knowledge_at_micros: 1,
            logical_message: None,
            turn: None,
            session: Some("session.hotpath-coverage".into()),
            source: Some("store".into()),
            evidence_role: Some("message".into()),
            exact_ranges: Vec::new(),
            participant_generation: 1,
        }],
        DiversityLimits::unbounded(),
    )
    .expect("rank deterministic candidate");
    assert_eq!(ranked.len(), 1);
}

#[cfg(not(feature = "hotpath"))]
#[test]
fn measured_temporal_query_runs_with_hotpath_off() {
    exercise_temporal_query();
}

#[cfg(feature = "hotpath")]
#[test]
fn measured_temporal_query_emits_exact_labels() {
    hotpath_report_support::assert_hotpath_report(
        "temporal-query-hotpath-coverage",
        "functions-timing",
        EXPECTED_LABELS,
        exercise_temporal_query,
    );
}
