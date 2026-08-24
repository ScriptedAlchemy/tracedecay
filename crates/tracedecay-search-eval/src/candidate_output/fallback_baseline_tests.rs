//! Native semantic outage regressions for the canonical query fallback.

use std::path::Path;

use super::*;
use crate::semantic_native::SemanticNativeQueryInputV1;

fn admitted_scope(_repo_root: &Path) -> Option<ResolvedScope> {
    ResolvedScope::new(
        ProjectId::new("project.search-eval-fallback").ok()?,
        RepositoryId::new("repository.search-eval-fallback").ok()?,
        tracedecay_domain::WorktreeId::new("worktree.search-eval-fallback").ok()?,
        None,
    )
    .ok()
}

fn checked_in_workload() -> CandidateWorkloadV1 {
    load_candidate_workload(&crate::checked_in_fixture_root().join(WORKLOAD_RELATIVE))
        .expect("checked-in search-quality workload")
}

#[test]
fn native_evaluation_rejects_a_digest_valid_fallback_with_the_wrong_baseline_order() {
    let repo_root = crate::checked_in_fixture_root();
    let workload = checked_in_workload();
    let published = publish_corpus(&repo_root, &workload, admitted_scope).expect("publish corpus");
    let profile = workload
        .profile_matrix
        .iter()
        .find(|profile| profile.profile_id == "hybrid-conservative")
        .expect("semantic profile");
    let query = workload
        .queries
        .iter()
        .find(|query| query.strata.iter().any(|stratum| stratum == "exact_symbol"))
        .expect("protected exact query");
    let prepared = prepare_production_query(&published, profile, query).expect("prepared query");
    assert!(
        prepared.fallback.ordered_candidates.len() > 1,
        "the protected fixture must have a multi-candidate exact/lexical/graph baseline"
    );
    let mut unrelated_candidates = prepared.fallback.ordered_candidates.clone();
    unrelated_candidates.rotate_left(1);
    for (ordinal, candidate) in unrelated_candidates.iter_mut().enumerate() {
        candidate.final_ordinal = ordinal as u32;
    }
    let unrelated_fallback = QueryFallbackSubpayload::new(
        prepared.fallback.profile_id.clone(),
        unrelated_candidates,
        prepared.fallback.public_fallback_lane_coverage.clone(),
        prepared.fallback.freshness.clone(),
        None,
    )
    .expect("digest-valid unrelated fallback");
    let fusion = fusion_profile(profile, true).expect("semantic fusion");

    let error = evaluate_native_query(SemanticNativeQueryInputV1 {
        profile_spec: profile,
        fusion_profile: &fusion,
        diversity_policy: &prepared.diversity,
        kernel: &prepared.kernel,
        fallback_lanes: &prepared.fallback_lanes,
        query_measurements: prepared.query_measurements,
        semantic: None,
        fallback: &unrelated_fallback,
        rerank: None,
    })
    .expect_err("semantic outage must not accept an unrelated fallback");

    assert!(error.to_string().contains("baseline"));
}
