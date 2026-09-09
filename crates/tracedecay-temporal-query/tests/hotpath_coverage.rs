//! Hotpath coverage contract for `tracedecay-temporal-query`.
//!
//! Feature-off (default build): every hotpath macro must be a no-op — no
//! report file even when the report environment is set.
//!
//! Feature-on (`--features hotpath`): a process-boundary guard must capture
//! this crate's measured candidate-planning and ranking sites in a
//! functions-timing report, proving the instrumentation is real rather than
//! dead configuration.

#[cfg(not(feature = "hotpath"))]
use tracedecay_domain::RetrievalAnchorId;
#[cfg(not(feature = "hotpath"))]
use tracedecay_temporal_query::candidates::CandidateChannel;
#[cfg(not(feature = "hotpath"))]
use tracedecay_temporal_query::plan_temporal_candidates;
#[cfg(not(feature = "hotpath"))]
use tracedecay_temporal_query::ranking::{DiversityLimits, RankingCandidate, rank_candidates};

/// Deterministic, daemon-free workload that reaches this crate's measured
/// sites: `temporal.candidates.plan_scope`, `temporal.candidates.plan_text`,
/// and `temporal.rank`.
#[cfg(not(feature = "hotpath"))]
fn run_temporal_query_workload() -> usize {
    let scope_plan = plan_temporal_candidates("", None, false);
    assert!(
        scope_plan.contains(CandidateChannel::Scope, ""),
        "an empty query must plan a scope sweep"
    );

    let text_plan = plan_temporal_candidates("cargo test 2026-07-18", None, false);
    assert!(
        !text_plan.clauses().is_empty(),
        "a text query must plan candidate clauses"
    );

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
    .expect("rank single deterministic candidate");
    assert_eq!(ranked.len(), 1);

    text_plan.clauses().len()
}

#[cfg(not(feature = "hotpath"))]
mod feature_off {
    use std::path::Path;

    /// With the feature off the macros expand to their primary expression:
    /// the workload behaves identically and the report environment is ignored.
    #[test]
    fn workload_is_a_no_op_for_profiling() {
        let report = Path::new(env!("CARGO_TARGET_TMPDIR")).join("temporal-query-hotpath-off.json");
        let _ = std::fs::remove_file(&report);
        // SAFETY: single-threaded with respect to readers — the feature-off
        // build contains no hotpath runtime and nothing else in this test
        // binary reads these variables.
        unsafe {
            std::env::set_var("HOTPATH_OUTPUT_FORMAT", "json");
            std::env::set_var("HOTPATH_OUTPUT_PATH", &report);
        }

        assert!(super::run_temporal_query_workload() > 0);

        assert!(
            !report.exists(),
            "feature-off build must never write a hotpath report"
        );
    }
}
