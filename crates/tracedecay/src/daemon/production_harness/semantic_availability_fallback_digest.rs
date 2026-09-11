//! Activation assertions for `query_fallback_digest` (#1109).
//!
//! The digest stays provenance-sensitive: it names ranking evidence, not
//! ranking bytes. Activation therefore moves the digest when
//! `evaluation_result_anchor` flips from the checked-in core policy to the
//! evaluated search-eval authority. These helpers assert the ranking that
//! must stay identical (exact/lexical candidates, order, and lane
//! contributions) and the authority transition that is allowed to move the
//! digest. `QueryFallbackSubpayload` does not expose its digest inputs, so
//! isolation is proven by comparing two captured search responses that
//! differ only in that anchor.
//!
//! `origin/verify/native-acceptance-707` does not carry this change: its
//! seven commits ahead of the redesign tip touch search-quality scoring,
//! fusion tie order, artifact import checkpoints, and retention timings.

use std::collections::BTreeSet;

use serde_json::{Value, json};
use tracedecay_domain::{
    CandidateContribution, EvidenceRole, ExactClass, FixedPointScore, FreshnessCompatibilityV1,
    FusedCandidate, FusionProfileId, OccurrenceProvenance, PublicRetrieverStatus,
    QueryFallbackSubpayload, RankedCandidate, RankingDecision, RankingDecisionKind,
    RetrievalAnchorId, RetrieverKind, SourceFreshness, SourceOccurrenceId, UtcMicros,
};

/// Checked-in core policy mounted at project open (`46f3ddca4`).
///
/// Issue #1109 recorded the serving exact-profile authority as
/// `policy.query-fallback.workload.v1.sha256:a4c7…` before activation.
pub(super) const CORE_POLICY_EVALUATION_RESULT_ANCHOR_PREFIX: &str =
    "policy.query-fallback.workload.v1.sha256:";

/// Evaluated initial fallback promoted by `prepare_after_successful_activation`.
///
/// Issue #1109 recorded the post-activation authority as
/// `search-eval:sha256:c23e…`.
pub(super) const EVALUATED_EVALUATION_RESULT_ANCHOR_PREFIX: &str = "search-eval:sha256:";

const EXACT_LEXICAL_LANES: &[&str] = &["exact_literal", "lexical"];

/// Exact-tier `policy_anchor` is the served `evaluation_result_anchor`.
pub(super) fn evaluation_result_anchors(payload: &Value) -> BTreeSet<String> {
    payload["results"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|result| {
            result["candidate"]["decisions"]
                .as_array()
                .into_iter()
                .flatten()
        })
        .filter(|decision| decision["kind"] == json!("exact_tier_admission"))
        .filter_map(|decision| decision["policy_anchor"].as_str().map(ToOwned::to_owned))
        .collect()
}

fn exact_lexical_contributions(result: &Value) -> Vec<Value> {
    result["candidate"]["contributions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|contribution| {
            EXACT_LEXICAL_LANES.contains(&contribution["retriever"].as_str().unwrap_or(""))
        })
        .cloned()
        .collect()
}

/// Ranked exact/lexical view: identity, relative order, and those lanes'
/// contributions. Semantic-only hits and `final_ordinal` shifts are excluded
/// so activation can join the semantic lane without looking like a ranking
/// change.
pub(super) fn exact_lexical_ranking(payload: &Value) -> Vec<Value> {
    payload["results"]
        .as_array()
        .unwrap_or_else(|| panic!("search must publish ranked results: {payload}"))
        .iter()
        .filter_map(|result| {
            let contributions = exact_lexical_contributions(result);
            if contributions.is_empty() {
                return None;
            }
            Some(json!({
                "anchor_id": result["candidate"]["anchor_id"],
                "logical_evidence_id": result["candidate"]["logical_evidence_id"],
                "contributions": contributions,
            }))
        })
        .collect()
}

pub(super) fn assert_core_policy_evaluation_result_anchor(payload: &Value) {
    let anchors = evaluation_result_anchors(payload);
    assert!(
        !anchors.is_empty(),
        "pre-activation exact-tier decisions must publish evaluation_result_anchor: {payload}"
    );
    for anchor in &anchors {
        assert!(
            anchor.starts_with(CORE_POLICY_EVALUATION_RESULT_ANCHOR_PREFIX),
            "pre-activation evaluation_result_anchor must be the checked-in core policy \
             ({CORE_POLICY_EVALUATION_RESULT_ANCHOR_PREFIX}…): {anchor}"
        );
    }
}

/// Activation preserves the served exact/lexical ranking and flips the
/// evaluation authority. The digest must move with that flip, and only
/// because of it: the ranking view is identical, the anchors match the
/// #1109 transition, and the public digest is therefore unequal.
pub(super) fn assert_activation_preserves_ranking_and_transitions_anchor(
    before: &Value,
    after: &Value,
) {
    assert_eq!(
        exact_lexical_ranking(before),
        exact_lexical_ranking(after),
        "activation must preserve exact/lexical candidates, order, and lane contributions"
    );

    let before_anchors = evaluation_result_anchors(before);
    let after_anchors = evaluation_result_anchors(after);
    assert_eq!(
        before_anchors.len(),
        1,
        "pre-activation must serve one evaluation_result_anchor: {before_anchors:?}"
    );
    assert_eq!(
        after_anchors.len(),
        1,
        "post-activation must serve one evaluation_result_anchor: {after_anchors:?}"
    );
    let before_anchor = before_anchors.iter().next().expect("checked length");
    let after_anchor = after_anchors.iter().next().expect("checked length");
    assert!(
        before_anchor.starts_with(CORE_POLICY_EVALUATION_RESULT_ANCHOR_PREFIX),
        "pre-activation evaluation_result_anchor must be the checked-in core policy \
         ({CORE_POLICY_EVALUATION_RESULT_ANCHOR_PREFIX}…): {before_anchor}"
    );
    assert!(
        after_anchor.starts_with(EVALUATED_EVALUATION_RESULT_ANCHOR_PREFIX),
        "post-activation evaluation_result_anchor must be the evaluated search-eval authority \
         ({EVALUATED_EVALUATION_RESULT_ANCHOR_PREFIX}…): {after_anchor}"
    );
    assert_ne!(
        before_anchor, after_anchor,
        "activation must change evaluation_result_anchor"
    );

    let before_digest = before["query_fallback_digest"]
        .as_str()
        .unwrap_or_else(|| panic!("pre-activation must publish query_fallback_digest: {before}"));
    let after_digest = after["query_fallback_digest"]
        .as_str()
        .unwrap_or_else(|| panic!("post-activation must publish query_fallback_digest: {after}"));
    assert_ne!(
        before_digest, after_digest,
        "query_fallback_digest must move with the evaluation_result_anchor transition \
         ({before_anchor} → {after_anchor})"
    );
}

fn typed<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("captured search fixture identity")
}

fn freshness() -> SourceFreshness {
    SourceFreshness {
        source_namespace: typed("ns.semantic-availability"),
        source_instance: typed("instance.semantic-availability"),
        source_watermark: Some(1),
        projection_watermark: Some(1),
        observed_at: UtcMicros(1),
        source_generation: Some(1),
        generation_lag: Some(0),
        compatibility: FreshnessCompatibilityV1::Current,
        policy_revision: typed("policy.semantic-availability.v1"),
    }
}

fn contribution(
    retriever: RetrieverKind,
    occurrence: &SourceOccurrenceId,
    ordinal_rank: u32,
) -> CandidateContribution {
    CandidateContribution {
        retriever,
        retriever_revision: typed(&format!("retriever.{}.v1", retriever.as_str())),
        source_occurrence_id: occurrence.clone(),
        ordinal_rank,
        raw_score: FixedPointScore(1),
        score_domain: typed(&format!("score.{}.v1", retriever.as_str())),
        calibration_profile_id: typed(&format!("calibration.{}.v1", retriever.as_str())),
        calibrated_feature_micros: 1,
        weight_micros: 1,
        weighted_contribution_micros: 1,
    }
}

/// One captured `tracedecay_search` result: `json!(RankedCandidate)` plus the
/// display object MCP attaches. Not a hand-built field set.
fn captured_ranked_result(policy_anchor: &str) -> (RankedCandidate, Value) {
    let evidence = RetrievalAnchorId::new("evidence-anchor.semantic_availability_probe")
        .expect("captured evidence anchor");
    let occurrence: SourceOccurrenceId = typed("occurrence.semantic-availability-probe");
    let ranked = RankedCandidate {
        candidate: FusedCandidate {
            anchor_id: RetrievalAnchorId::new("code-symbol:semantic_availability_probe")
                .expect("captured candidate anchor"),
            logical_evidence_id: typed("logical.semantic-availability-probe"),
            occurrences: vec![OccurrenceProvenance {
                source_occurrence_id: occurrence.clone(),
                file_occurrence_id: None,
                retriever_evidence_anchor: evidence.clone(),
                source_namespace: typed("ns.semantic-availability"),
                repository_id: None,
                session_or_thread_id: None,
                logical_copy_cluster_id: None,
                logical_copy_evidence_anchor: None,
                evidence_role: EvidenceRole::Primary,
                freshness: freshness(),
            }],
            exact_class: ExactClass::ExactMessage,
            utility_micros: 2,
            contributions: vec![
                contribution(RetrieverKind::ExactLiteral, &occurrence, 0),
                contribution(RetrieverKind::Lexical, &occurrence, 0),
            ],
            freshness: vec![freshness()],
            decisions: vec![RankingDecision {
                kind: RankingDecisionKind::ExactTierAdmission,
                retriever: Some(RetrieverKind::ExactLiteral),
                policy_anchor: Some(
                    RetrievalAnchorId::new(policy_anchor).expect("captured anchor"),
                ),
                evidence_anchor: Some(evidence),
                detail: "validated exact admission proof".to_owned(),
            }],
        },
        final_ordinal: 0,
    };
    let mut result = json!(ranked);
    result["display"] = json!({
        "name": "semantic_availability_probe",
        "qualified_name": "crate::semantic_availability_probe",
        "kind": "function",
        "path": "src/lib.rs",
    });
    (ranked, result)
}

fn captured_search_response(policy_anchor: &str) -> Value {
    let (ranked, result) = captured_ranked_result(policy_anchor);
    let coverage = RetrieverKind::QUERY_FALLBACK_LANES
        .into_iter()
        .map(|lane| (lane, PublicRetrieverStatus::Complete))
        .collect();
    let fallback = QueryFallbackSubpayload::new(
        FusionProfileId::new("profile.query-fallback").expect("captured fallback profile"),
        vec![ranked],
        coverage,
        Vec::new(),
        None,
    )
    .expect("captured query fallback subpayload");
    json!({
        "code_generation": "generation.semantic-availability.1",
        "query_fallback_digest": fallback.digest,
        "semantic": {
            "status": "unavailable",
            "reason": "semantic_profile_pending",
        },
        "coverage": {
            "recall": "partial",
            "exact": "complete",
            "lexical": "complete",
            "graph": "complete",
            "semantic": { "status": "unavailable" },
        },
        "results": [result],
    })
}

#[test]
fn fallback_digest_moves_only_with_evaluation_result_anchor_transition() {
    // Issue #1109 measured these two authorities on a live FastEmbed
    // activation; the hex prefixes are the recorded values, completed so
    // the captured RankedCandidate can carry a real RetrievalAnchorId.
    let before = captured_search_response(
        "policy.query-fallback.workload.v1.sha256:a4c7aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    let after = captured_search_response(
        "search-eval:sha256:c23ebbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    );

    assert_eq!(
        exact_lexical_ranking(&before),
        exact_lexical_ranking(&after),
        "captured pair must differ only in the evaluation_result_anchor"
    );
    assert_ne!(
        before["query_fallback_digest"], after["query_fallback_digest"],
        "the captured pair must recompute query_fallback_digest from the anchor"
    );
    assert_activation_preserves_ranking_and_transitions_anchor(&before, &after);
}
