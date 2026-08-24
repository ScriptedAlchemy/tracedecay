use std::collections::BTreeMap;
use std::{cell::Cell, rc::Rc};

use tracedecay_domain::{
    DiversityPolicy, EvidenceRole, ExactClass, HydrationReceipt, PublicRetrieverStatus,
    RankingDecisionKind, RetrievalBudgetUsage, RetrievalFailure, RetrieverKind, RetrieverOutcome,
    ScoreDomainCalibrationV1,
};

use super::{
    batch, budget, candidate, composition_lanes, exact_candidate, id, no_caps, profile, request,
};
use crate::retrieval::fusion::{CompositionKernel, FusionStageInput};
use crate::retrieval::hydrate::{
    CanonicalLateHydration, HydrationAuthorizationV1, HydrationExecutionControlV1,
    HydrationOutcomeV1, HydrationPreflightOutcomeV1, HydrationReadOutcomeV1, HydrationStageError,
    HydrationUnavailableV1, HydrationWorkPermitV1, LateHydrationSource,
};

fn receipt(
    candidate: &tracedecay_domain::RankedCandidate,
    bytes_hydrated: u64,
) -> HydrationReceipt {
    HydrationReceipt {
        anchor_id: candidate.candidate.anchor_id.clone(),
        source_occurrence_id: candidate.candidate.occurrences[0]
            .source_occurrence_id
            .clone(),
        hydration_revision: id("hydration.fixture.v1"),
        bytes_hydrated,
        authorized: true,
        freshness: candidate.candidate.freshness[0].clone(),
    }
}

#[test]
fn semantic_budget_exceeded_does_not_take_down_exact_lexical_or_graph() {
    let exact = exact_candidate("exact", 1);
    let lexical = candidate(RetrieverKind::Lexical, "lexical", 900_000, 0);
    let graph = candidate(RetrieverKind::Graph, "graph", 800_000, 0);
    let lanes = vec![
        (
            RetrieverKind::ExactLiteral,
            RetrieverOutcome::Complete(batch(vec![exact], "exact evidence")),
        ),
        (
            RetrieverKind::Lexical,
            RetrieverOutcome::Complete(batch(vec![lexical], "lexical evidence")),
        ),
        (
            RetrieverKind::Graph,
            RetrieverOutcome::Complete(batch(vec![graph], "graph evidence")),
        ),
        (
            RetrieverKind::Semantic,
            RetrieverOutcome::BudgetExceeded(RetrievalBudgetUsage::default()),
        ),
    ];
    let composition = CompositionKernel::new(id("ranking.fixture.v1"))
        .compose(
            &FusionStageInput {
                profile: profile(),
                lanes: composition_lanes(lanes),
            },
            &no_caps(),
        )
        .expect("semantic budget miss must not fail composition");

    assert_eq!(
        composition.public_lane_statuses[&RetrieverKind::Semantic],
        PublicRetrieverStatus::Partial
    );
    assert_eq!(
        composition.public_lane_statuses[&RetrieverKind::ExactLiteral],
        PublicRetrieverStatus::Complete
    );
    assert_eq!(
        composition.public_lane_statuses[&RetrieverKind::Lexical],
        PublicRetrieverStatus::Complete
    );
    assert_eq!(
        composition.public_lane_statuses[&RetrieverKind::Graph],
        PublicRetrieverStatus::Complete
    );
    assert_eq!(
        composition.ranked_candidates[0].candidate.exact_class,
        ExactClass::ExactMessage
    );
    assert!(composition.ranked_candidates.iter().all(|ranked| {
        ranked
            .candidate
            .contributions
            .iter()
            .all(|contribution| contribution.retriever != RetrieverKind::Semantic)
    }));
}

#[test]
fn composition_is_shuffle_stable_and_exact_is_non_demotable() {
    let exact = exact_candidate("exact", 1);
    let lexical = candidate(RetrieverKind::Lexical, "lexical", 900_000, 0);
    let graph = candidate(RetrieverKind::Graph, "graph", 800_000, 0);
    let lanes = vec![
        (
            RetrieverKind::ExactLiteral,
            RetrieverOutcome::Complete(batch(vec![exact], "exact evidence")),
        ),
        (
            RetrieverKind::Lexical,
            RetrieverOutcome::Complete(batch(vec![lexical], "lexical evidence")),
        ),
        (
            RetrieverKind::Graph,
            RetrieverOutcome::Complete(batch(vec![graph], "graph evidence")),
        ),
    ];
    let kernel = CompositionKernel::new(id("ranking.fixture.v1"));
    let first = kernel
        .compose(
            &FusionStageInput {
                profile: profile(),
                lanes: composition_lanes(lanes.clone()),
            },
            &no_caps(),
        )
        .expect("composition succeeds");

    for iteration in 0..100 {
        let mut shuffled = lanes.clone();
        let offset = iteration % shuffled.len();
        shuffled.rotate_left(offset);
        if iteration % 2 == 1 {
            shuffled.reverse();
        }
        let rerun = kernel
            .compose(
                &FusionStageInput {
                    profile: profile(),
                    lanes: composition_lanes(shuffled),
                },
                &no_caps(),
            )
            .expect("shuffled composition succeeds");
        assert_eq!(first, rerun, "shuffle {iteration} changed composition");
    }
    assert_eq!(
        first.ranked_candidates[0].candidate.exact_class,
        ExactClass::ExactMessage
    );
    assert_eq!(
        first
            .ranked_candidates
            .iter()
            .map(|ranked| ranked.candidate.utility_micros)
            .collect::<Vec<_>>(),
        vec![1, 450_000, 200_000]
    );
    assert_eq!(first.comparator_records.len(), 3);
    assert!(first.comparator_records.iter().all(|record| {
        !record.anchor_id.as_str().is_empty()
            && !record.logical_evidence_id.as_str().is_empty()
            && !record.source_occurrence_ids.is_empty()
    }));
    assert!(first.ranked_candidates.iter().all(|ranked| {
        ranked
            .candidate
            .decisions
            .iter()
            .any(|decision| decision.kind == RankingDecisionKind::ComparatorProvenance)
    }));
}

#[test]
fn fusion_retains_every_occurrence_evidence_pair_and_contribution() {
    let mut lexical = candidate(RetrieverKind::Lexical, "shared", 800_000, 0);
    let mut graph = candidate(RetrieverKind::Graph, "shared", 400_000, 0);
    graph.source_occurrence_id = lexical.source_occurrence_id.clone();
    graph.anchor_id = lexical.anchor_id.clone();
    graph.logical_evidence_id = lexical.logical_evidence_id.clone();
    graph.freshness = lexical.freshness.clone();
    lexical.retriever_evidence_anchor =
        tracedecay_domain::RetrievalAnchorId::new("evidence.lexical.shared").unwrap();
    graph.retriever_evidence_anchor =
        tracedecay_domain::RetrievalAnchorId::new("evidence.graph.shared").unwrap();

    let output = CompositionKernel::new(id("ranking.fixture.v1"))
        .compose(
            &FusionStageInput {
                profile: profile(),
                lanes: composition_lanes(vec![
                    (
                        RetrieverKind::ExactLiteral,
                        RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
                    ),
                    (
                        RetrieverKind::Lexical,
                        RetrieverOutcome::Complete(batch(vec![lexical], "lexical")),
                    ),
                    (
                        RetrieverKind::Graph,
                        RetrieverOutcome::Complete(batch(vec![graph], "graph")),
                    ),
                ]),
            },
            &no_caps(),
        )
        .unwrap();

    let fused = &output.ranked_candidates[0].candidate;
    assert_eq!(fused.contributions.len(), 2);
    assert_eq!(fused.occurrences.len(), 2);
    assert_ne!(
        fused.occurrences[0].retriever_evidence_anchor,
        fused.occurrences[1].retriever_evidence_anchor
    );
    assert_eq!(fused.utility_micros, 500_000);
}

#[test]
fn fusion_calibrates_raw_scores_in_their_declared_score_domain() {
    let normal = candidate(RetrieverKind::Lexical, "normal-domain", 1_000_000, 0);
    let mut shifted = candidate(RetrieverKind::Lexical, "shifted-domain", 1_000_000, 1);
    shifted.score_domain = id("score.lexical.shifted.v1");

    let mut fusion_profile = profile();
    fusion_profile.score_domain_calibrations.insert(
        id("score.lexical.shifted.v1"),
        ScoreDomainCalibrationV1 {
            calibration_profile_id: id("calibration.lexical.v1"),
            score_domain: id("score.lexical.shifted.v1"),
            raw_min_micros: 0,
            raw_max_micros: 2_000_000,
        },
    );

    let output = CompositionKernel::new(id("ranking.fixture.v1"))
        .compose(
            &FusionStageInput {
                profile: fusion_profile,
                lanes: composition_lanes(vec![
                    (
                        RetrieverKind::ExactLiteral,
                        RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
                    ),
                    (
                        RetrieverKind::Lexical,
                        RetrieverOutcome::Complete(batch(vec![normal, shifted], "lexical")),
                    ),
                    (
                        RetrieverKind::Graph,
                        RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
                    ),
                ]),
            },
            &no_caps(),
        )
        .expect("composition succeeds");

    let calibrated = output
        .ranked_candidates
        .iter()
        .map(|ranked| {
            (
                ranked.candidate.anchor_id.as_str(),
                ranked.candidate.contributions[0].calibrated_feature_micros,
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(calibrated["anchor.normal-domain"], 1_000_000);
    assert_eq!(calibrated["anchor.shifted-domain"], 500_000);
}

#[test]
fn same_source_duplicate_rows_collapse_only_for_the_same_evidence_pair() {
    let lexical = candidate(RetrieverKind::Lexical, "duplicate", 800_000, 0);
    let mut graph = candidate(RetrieverKind::Graph, "duplicate", 400_000, 0);
    graph.source_occurrence_id = lexical.source_occurrence_id.clone();
    graph.anchor_id = lexical.anchor_id.clone();
    graph.logical_evidence_id = lexical.logical_evidence_id.clone();
    graph.retriever_evidence_anchor = lexical.retriever_evidence_anchor.clone();
    graph.freshness = lexical.freshness.clone();

    let output = CompositionKernel::new(id("ranking.fixture.v1"))
        .compose(
            &FusionStageInput {
                profile: profile(),
                lanes: composition_lanes(vec![
                    (
                        RetrieverKind::ExactLiteral,
                        RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
                    ),
                    (
                        RetrieverKind::Lexical,
                        RetrieverOutcome::Complete(batch(vec![lexical], "lexical")),
                    ),
                    (
                        RetrieverKind::Graph,
                        RetrieverOutcome::Complete(batch(vec![graph], "graph")),
                    ),
                ]),
            },
            &no_caps(),
        )
        .unwrap();

    let fused = &output.ranked_candidates[0].candidate;
    assert_eq!(fused.occurrences.len(), 1);
    assert_eq!(fused.contributions.len(), 2);
    assert_eq!(fused.utility_micros, 500_000);
    assert!(
        fused
            .decisions
            .iter()
            .any(|decision| { decision.kind == RankingDecisionKind::SameSourceDuplicateCollapse })
    );
}

#[test]
fn partial_optional_batches_contribute_without_losing_the_typed_outcome() {
    let reason = RetrievalFailure::AuthorityUnavailable {
        detail: "bounded graph traversal ended at its checkpoint".to_owned(),
    };
    let output = CompositionKernel::new(id("ranking.fixture.v1"))
        .compose(
            &FusionStageInput {
                profile: profile(),
                lanes: composition_lanes(vec![
                    (
                        RetrieverKind::ExactLiteral,
                        RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
                    ),
                    (
                        RetrieverKind::Lexical,
                        RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
                    ),
                    (
                        RetrieverKind::Graph,
                        RetrieverOutcome::Partial {
                            value: batch(
                                vec![candidate(RetrieverKind::Graph, "partial", 800_000, 0)],
                                "graph",
                            ),
                            reason: reason.clone(),
                        },
                    ),
                ]),
            },
            &no_caps(),
        )
        .unwrap();

    assert_eq!(output.ranked_candidates.len(), 1);
    assert_eq!(
        output.public_lane_statuses[&RetrieverKind::Graph],
        PublicRetrieverStatus::Partial
    );
    assert_eq!(
        output.internal_lane_outcomes[&RetrieverKind::Graph],
        RetrieverOutcome::Partial { value: (), reason }
    );
}

#[test]
fn logical_copies_and_file_caps_preserve_contradictions_deterministically() {
    let mut primary = candidate(RetrieverKind::Lexical, "primary", 900_000, 0);
    primary.logical_copy_cluster_id = Some(id("copy.same"));
    primary.logical_copy_evidence_anchor =
        Some(tracedecay_domain::RetrievalAnchorId::new("copy-evidence.same").unwrap());
    primary.freshness.source_instance = id("file.same");

    let mut copy = candidate(RetrieverKind::Lexical, "copy", 800_000, 1);
    copy.logical_copy_cluster_id = Some(id("copy.same"));
    copy.logical_copy_evidence_anchor =
        Some(tracedecay_domain::RetrievalAnchorId::new("copy-evidence.same").unwrap());
    copy.freshness.source_instance = id("file.same");

    let mut contradiction = candidate(RetrieverKind::Lexical, "contradiction", 700_000, 2);
    contradiction.logical_copy_cluster_id = Some(id("copy.same"));
    contradiction.logical_copy_evidence_anchor =
        Some(tracedecay_domain::RetrievalAnchorId::new("copy-evidence.same").unwrap());
    contradiction.evidence_role = EvidenceRole::Contradiction;
    contradiction.freshness.source_instance = id("file.same");

    let mut other_file = candidate(RetrieverKind::Lexical, "other-file", 600_000, 3);
    other_file.freshness.source_instance = id("file.other");

    let policy = DiversityPolicy {
        per_source_instance: Some(1),
        ..no_caps()
    };
    let output = CompositionKernel::new(id("ranking.fixture.v1"))
        .compose(
            &FusionStageInput {
                profile: profile(),
                lanes: composition_lanes(vec![
                    (
                        RetrieverKind::ExactLiteral,
                        RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
                    ),
                    (
                        RetrieverKind::Lexical,
                        RetrieverOutcome::Complete(batch(
                            vec![primary, copy, contradiction, other_file],
                            "lexical",
                        )),
                    ),
                    (
                        RetrieverKind::Graph,
                        RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
                    ),
                ]),
            },
            &policy,
        )
        .unwrap();

    let anchors = output
        .ranked_candidates
        .iter()
        .map(|ranked| ranked.candidate.anchor_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        anchors,
        vec![
            "anchor.primary",
            "anchor.contradiction",
            "anchor.other-file"
        ]
    );
    assert_eq!(output.dedupe_decisions.len(), 1);
    assert_eq!(output.diversity_decisions.len(), 0);
}

#[test]
fn file_diversity_caps_non_exact_hits_and_refills_from_other_files() {
    let mut first = candidate(RetrieverKind::Lexical, "file-a-first", 900_000, 0);
    first.file_occurrence_id = Some(id("file.a"));
    let mut second = candidate(RetrieverKind::Lexical, "file-a-second", 800_000, 1);
    second.file_occurrence_id = Some(id("file.a"));
    let mut capped = candidate(RetrieverKind::Lexical, "file-a-capped", 700_000, 2);
    capped.file_occurrence_id = Some(id("file.a"));
    let mut refill = candidate(RetrieverKind::Lexical, "file-b-refill", 600_000, 3);
    refill.file_occurrence_id = Some(id("file.b"));

    let policy = DiversityPolicy {
        per_file: Some(2),
        ..no_caps()
    };
    let output = CompositionKernel::new(id("ranking.fixture.v1"))
        .compose(
            &FusionStageInput {
                profile: profile(),
                lanes: composition_lanes(vec![
                    (
                        RetrieverKind::ExactLiteral,
                        RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
                    ),
                    (
                        RetrieverKind::Lexical,
                        RetrieverOutcome::Complete(batch(
                            vec![first, second, capped, refill],
                            "lexical",
                        )),
                    ),
                    (
                        RetrieverKind::Graph,
                        RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
                    ),
                ]),
            },
            &policy,
        )
        .expect("file cap applies");

    assert_eq!(
        output
            .ranked_candidates
            .iter()
            .map(|ranked| ranked.candidate.anchor_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "anchor.file-a-first",
            "anchor.file-a-second",
            "anchor.file-b-refill"
        ]
    );
    assert_eq!(output.diversity_decisions.len(), 1);
    assert_eq!(
        output.diversity_decisions[0].decision.detail,
        "capped by file"
    );
}

#[test]
fn logical_copy_collapse_requires_relation_evidence_and_retains_every_copy_provenance() {
    let mut unproven_primary = candidate(RetrieverKind::Lexical, "unproven-primary", 900_000, 0);
    unproven_primary.logical_copy_cluster_id = Some(id("copy.unproven"));
    let mut unproven_copy = candidate(RetrieverKind::Lexical, "unproven-copy", 800_000, 1);
    unproven_copy.logical_copy_cluster_id = Some(id("copy.unproven"));
    let unproven = CompositionKernel::new(id("ranking.fixture.v1"))
        .compose(
            &FusionStageInput {
                profile: profile(),
                lanes: composition_lanes(vec![
                    (
                        RetrieverKind::ExactLiteral,
                        RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
                    ),
                    (
                        RetrieverKind::Lexical,
                        RetrieverOutcome::Complete(batch(
                            vec![unproven_primary, unproven_copy],
                            "lexical",
                        )),
                    ),
                    (
                        RetrieverKind::Graph,
                        RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
                    ),
                ]),
            },
            &no_caps(),
        )
        .expect_err("unproven logical copies fail closed");
    assert!(
        unproven
            .to_string()
            .contains("logical-copy relation lacks its evidence anchor")
    );

    let mut primary = candidate(RetrieverKind::Lexical, "proven-primary", 900_000, 0);
    primary.logical_copy_cluster_id = Some(id("copy.proven"));
    primary.logical_copy_evidence_anchor =
        Some(tracedecay_domain::RetrievalAnchorId::new("copy-evidence.proven").unwrap());
    let mut copy = candidate(RetrieverKind::Lexical, "proven-copy", 800_000, 1);
    copy.logical_copy_cluster_id = Some(id("copy.proven"));
    copy.logical_copy_evidence_anchor =
        Some(tracedecay_domain::RetrievalAnchorId::new("copy-evidence.proven").unwrap());
    let output = CompositionKernel::new(id("ranking.fixture.v1"))
        .compose(
            &FusionStageInput {
                profile: profile(),
                lanes: composition_lanes(vec![
                    (
                        RetrieverKind::ExactLiteral,
                        RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
                    ),
                    (
                        RetrieverKind::Lexical,
                        RetrieverOutcome::Complete(batch(vec![primary, copy], "lexical")),
                    ),
                    (
                        RetrieverKind::Graph,
                        RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
                    ),
                ]),
            },
            &no_caps(),
        )
        .expect("proven logical copies compose");

    let decision = output
        .dedupe_decisions
        .iter()
        .find(|decision| decision.copy_cluster.as_ref() == Some(&id("copy.proven")))
        .expect("copy-collapse decision");
    assert_eq!(decision.collapsed_candidates.len(), 1);
    let collapsed = &decision.collapsed_candidates[0];
    assert_eq!(collapsed.anchor_id.as_str(), "anchor.proven-copy");
    assert_eq!(
        collapsed.occurrences[0]
            .logical_copy_evidence_anchor
            .as_ref()
            .map(tracedecay_domain::RetrievalAnchorId::as_str),
        Some("copy-evidence.proven")
    );
}

#[derive(Default)]
struct FakeHydrationSource {
    authorization: BTreeMap<String, HydrationAuthorizationV1>,
    reads: Vec<String>,
}

impl LateHydrationSource<String> for FakeHydrationSource {
    fn authorize(
        &mut self,
        _request: &tracedecay_domain::RetrievalRequest,
        candidate: &tracedecay_domain::RankedCandidate,
    ) -> HydrationAuthorizationV1 {
        self.authorization
            .get(candidate.candidate.anchor_id.as_str())
            .copied()
            .unwrap_or(HydrationAuthorizationV1::Authorized)
    }

    fn preflight_authorized(
        &mut self,
        _request: &tracedecay_domain::RetrievalRequest,
        _candidate: &tracedecay_domain::RankedCandidate,
        _permit: &HydrationWorkPermitV1,
    ) -> HydrationPreflightOutcomeV1 {
        HydrationPreflightOutcomeV1::Ready { estimated_bytes: 1 }
    }

    fn hydrate_authorized(
        &mut self,
        _request: &tracedecay_domain::RetrievalRequest,
        candidate: &tracedecay_domain::RankedCandidate,
        _permit: &HydrationWorkPermitV1,
    ) -> HydrationReadOutcomeV1<String> {
        let anchor = candidate.candidate.anchor_id.as_str().to_owned();
        self.reads.push(anchor.clone());
        HydrationReadOutcomeV1::Complete {
            payload: anchor.clone(),
            receipt: receipt(candidate, 1),
        }
    }
}

#[test]
fn hydration_reauthorizes_after_ranking_and_denial_never_reads_payload() {
    let lanes = vec![
        (
            RetrieverKind::ExactLiteral,
            RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
        ),
        (
            RetrieverKind::Lexical,
            RetrieverOutcome::Complete(batch(
                vec![
                    candidate(RetrieverKind::Lexical, "first", 900_000, 0),
                    candidate(RetrieverKind::Lexical, "denied", 800_000, 1),
                    candidate(RetrieverKind::Lexical, "third", 700_000, 2),
                ],
                "lexical",
            )),
        ),
        (
            RetrieverKind::Graph,
            RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
        ),
    ];
    let ranked = CompositionKernel::new(id("ranking.fixture.v1"))
        .compose(
            &FusionStageInput {
                profile: profile(),
                lanes: composition_lanes(lanes),
            },
            &no_caps(),
        )
        .unwrap()
        .ranked_candidates;
    let mut source = FakeHydrationSource::default();
    source
        .authorization
        .insert("anchor.denied".to_owned(), HydrationAuthorizationV1::Denied);
    let page = CanonicalLateHydration::new(&mut source)
        .hydrate(&request(), &ranked, &budget())
        .unwrap();

    assert_eq!(source.reads, vec!["anchor.first", "anchor.third"]);
    assert_eq!(page.results.len(), 3);
    assert!(matches!(
        page.results[1].outcome,
        HydrationOutcomeV1::Unavailable(HydrationUnavailableV1::AuthorityUnavailable)
    ));
    assert_eq!(page.receipts.len(), 2);

    let mut unavailable_source = FakeHydrationSource::default();
    unavailable_source.authorization.insert(
        "anchor.denied".to_owned(),
        HydrationAuthorizationV1::Unavailable(HydrationUnavailableV1::AuthorityUnavailable),
    );
    let unavailable_page = CanonicalLateHydration::new(&mut unavailable_source)
        .hydrate(&request(), &ranked, &budget())
        .unwrap();
    assert_eq!(page, unavailable_page);
    assert_eq!(source.reads, unavailable_source.reads);
}

#[derive(Default)]
struct PartialHydrationSource {
    reads: Vec<String>,
}

impl LateHydrationSource<String> for PartialHydrationSource {
    fn authorize(
        &mut self,
        _request: &tracedecay_domain::RetrievalRequest,
        _candidate: &tracedecay_domain::RankedCandidate,
    ) -> HydrationAuthorizationV1 {
        HydrationAuthorizationV1::Authorized
    }

    fn preflight_authorized(
        &mut self,
        _request: &tracedecay_domain::RetrievalRequest,
        candidate: &tracedecay_domain::RankedCandidate,
        permit: &HydrationWorkPermitV1,
    ) -> HydrationPreflightOutcomeV1 {
        let bytes = if candidate.candidate.anchor_id.as_str() == "anchor.first" {
            4
        } else {
            3
        };
        assert!(bytes <= permit.remaining_bytes);
        HydrationPreflightOutcomeV1::Ready {
            estimated_bytes: bytes,
        }
    }

    fn hydrate_authorized(
        &mut self,
        _request: &tracedecay_domain::RetrievalRequest,
        candidate: &tracedecay_domain::RankedCandidate,
        permit: &HydrationWorkPermitV1,
    ) -> HydrationReadOutcomeV1<String> {
        let anchor = candidate.candidate.anchor_id.as_str().to_owned();
        self.reads.push(anchor.clone());
        let bytes = if anchor == "anchor.first" { 4 } else { 3 };
        assert!(bytes <= permit.remaining_bytes);
        if anchor == "anchor.second" {
            HydrationReadOutcomeV1::Partial {
                payload: anchor,
                receipt: receipt(candidate, bytes),
                reason: HydrationUnavailableV1::Stale,
            }
        } else {
            HydrationReadOutcomeV1::Complete {
                payload: anchor,
                receipt: receipt(candidate, bytes),
            }
        }
    }
}

#[test]
fn hydration_preserves_partial_outcomes_and_stops_at_the_ranked_prefix_bound() {
    let ranked = CompositionKernel::new(id("ranking.fixture.v1"))
        .compose(
            &FusionStageInput {
                profile: profile(),
                lanes: composition_lanes(vec![
                    (
                        RetrieverKind::ExactLiteral,
                        RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
                    ),
                    (
                        RetrieverKind::Lexical,
                        RetrieverOutcome::Complete(batch(
                            vec![
                                candidate(RetrieverKind::Lexical, "first", 900_000, 0),
                                candidate(RetrieverKind::Lexical, "second", 800_000, 1),
                                candidate(RetrieverKind::Lexical, "third", 700_000, 2),
                            ],
                            "lexical",
                        )),
                    ),
                    (
                        RetrieverKind::Graph,
                        RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
                    ),
                ]),
            },
            &no_caps(),
        )
        .unwrap()
        .ranked_candidates;
    let mut bounded_budget = budget();
    bounded_budget.max_hydrated_results = 2;
    bounded_budget.max_hydration_bytes = 8;
    let mut source = PartialHydrationSource::default();
    let page = CanonicalLateHydration::new(&mut source)
        .hydrate(&request(), &ranked, &bounded_budget)
        .unwrap();

    assert_eq!(source.reads, vec!["anchor.first", "anchor.second"]);
    assert_eq!(page.results.len(), 2);
    assert!(matches!(
        page.results[1].outcome,
        HydrationOutcomeV1::Partial {
            reason: HydrationUnavailableV1::Stale,
            ..
        }
    ));
    assert_eq!(
        page.receipts
            .iter()
            .map(|receipt| receipt.bytes_hydrated)
            .sum::<u64>(),
        7
    );
}

#[derive(Clone, Copy)]
struct FixedHydrationExecutionControl {
    elapsed_micros: u64,
    cancelled: bool,
}

impl HydrationExecutionControlV1 for FixedHydrationExecutionControl {
    fn elapsed_micros(&self) -> u64 {
        self.elapsed_micros
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled
    }
}

struct PreflightHydrationSource {
    authorizations: usize,
    preflights: usize,
    reads: usize,
    estimated_bytes: u64,
    mismatched_receipt: bool,
    remaining_deadlines: Vec<Option<u64>>,
}

impl LateHydrationSource<String> for PreflightHydrationSource {
    fn authorize(
        &mut self,
        _request: &tracedecay_domain::RetrievalRequest,
        _candidate: &tracedecay_domain::RankedCandidate,
    ) -> HydrationAuthorizationV1 {
        self.authorizations += 1;
        HydrationAuthorizationV1::Authorized
    }

    fn preflight_authorized(
        &mut self,
        _request: &tracedecay_domain::RetrievalRequest,
        candidate: &tracedecay_domain::RankedCandidate,
        permit: &HydrationWorkPermitV1,
    ) -> HydrationPreflightOutcomeV1 {
        self.preflights += 1;
        self.remaining_deadlines
            .push(permit.remaining_deadline_micros);
        assert_eq!(permit.anchor_id, candidate.candidate.anchor_id);
        assert!(!permit.source_occurrence_ids.is_empty());
        HydrationPreflightOutcomeV1::Ready {
            estimated_bytes: self.estimated_bytes,
        }
    }

    fn hydrate_authorized(
        &mut self,
        _request: &tracedecay_domain::RetrievalRequest,
        candidate: &tracedecay_domain::RankedCandidate,
        permit: &HydrationWorkPermitV1,
    ) -> HydrationReadOutcomeV1<String> {
        assert!(self.estimated_bytes <= permit.remaining_bytes);
        self.reads += 1;
        self.remaining_deadlines
            .push(permit.remaining_deadline_micros);
        let mut receipt = receipt(candidate, self.estimated_bytes);
        if self.mismatched_receipt {
            receipt.source_occurrence_id = id("occurrence.not-permitted");
        }
        HydrationReadOutcomeV1::Complete {
            payload: candidate.candidate.anchor_id.as_str().to_owned(),
            receipt,
        }
    }
}

fn single_ranked_candidate() -> Vec<tracedecay_domain::RankedCandidate> {
    CompositionKernel::new(id("ranking.fixture.v1"))
        .compose(
            &FusionStageInput {
                profile: profile(),
                lanes: composition_lanes(vec![
                    (
                        RetrieverKind::ExactLiteral,
                        RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
                    ),
                    (
                        RetrieverKind::Lexical,
                        RetrieverOutcome::Complete(batch(
                            vec![candidate(RetrieverKind::Lexical, "hydration", 900_000, 0)],
                            "lexical",
                        )),
                    ),
                    (
                        RetrieverKind::Graph,
                        RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
                    ),
                ]),
            },
            &no_caps(),
        )
        .expect("composition succeeds")
        .ranked_candidates
}

#[test]
fn hydration_preflights_deadline_bytes_and_cancellation_before_payload_work() {
    let request = request();
    let ranked = single_ranked_candidate();
    let active = FixedHydrationExecutionControl {
        elapsed_micros: 0,
        cancelled: false,
    };

    let mut byte_budget = budget();
    byte_budget.max_hydration_bytes = 4;
    let mut byte_source = PreflightHydrationSource {
        authorizations: 0,
        preflights: 0,
        reads: 0,
        estimated_bytes: 5,
        mismatched_receipt: false,
        remaining_deadlines: Vec::new(),
    };
    let byte_page = CanonicalLateHydration::new(&mut byte_source)
        .hydrate_with_control(&request, &ranked, &byte_budget, &active)
        .expect("preflight byte rejection is a positional result");
    assert_eq!(byte_source.authorizations, 1);
    assert_eq!(byte_source.preflights, 1);
    assert_eq!(byte_source.reads, 0);
    assert!(matches!(
        byte_page.results[0].outcome,
        HydrationOutcomeV1::Unavailable(HydrationUnavailableV1::BudgetExceeded)
    ));
    assert!(byte_page.receipts.is_empty());

    let cancelled = FixedHydrationExecutionControl {
        elapsed_micros: 0,
        cancelled: true,
    };
    let mut cancelled_source = PreflightHydrationSource {
        authorizations: 0,
        preflights: 0,
        reads: 0,
        estimated_bytes: 1,
        mismatched_receipt: false,
        remaining_deadlines: Vec::new(),
    };
    let cancelled_page = CanonicalLateHydration::new(&mut cancelled_source)
        .hydrate_with_control(&request, &ranked, &budget(), &cancelled)
        .expect("cancellation is a positional result");
    assert_eq!(
        (
            cancelled_source.authorizations,
            cancelled_source.preflights,
            cancelled_source.reads,
        ),
        (0, 0, 0)
    );
    assert!(matches!(
        cancelled_page.results[0].outcome,
        HydrationOutcomeV1::Unavailable(HydrationUnavailableV1::Cancelled)
    ));

    let mut deadline_budget = budget();
    deadline_budget.deadline_micros = Some(1);
    let expired = FixedHydrationExecutionControl {
        elapsed_micros: 1,
        cancelled: false,
    };
    let mut deadline_source = PreflightHydrationSource {
        authorizations: 0,
        preflights: 0,
        reads: 0,
        estimated_bytes: 1,
        mismatched_receipt: false,
        remaining_deadlines: Vec::new(),
    };
    let deadline_page = CanonicalLateHydration::new(&mut deadline_source)
        .hydrate_with_control(&request, &ranked, &deadline_budget, &expired)
        .expect("expired deadline is a positional result");
    assert_eq!(
        (
            deadline_source.authorizations,
            deadline_source.preflights,
            deadline_source.reads,
        ),
        (0, 0, 0)
    );
    assert!(matches!(
        deadline_page.results[0].outcome,
        HydrationOutcomeV1::Unavailable(HydrationUnavailableV1::BudgetExceeded)
    ));
}

#[test]
fn hydration_deadline_is_request_relative_for_older_generation_and_still_cancellable() {
    let mut request = request();
    request.snapshot.captured_at = tracedecay_domain::UtcMicros(1);
    let ranked = single_ranked_candidate();
    let mut deadline_budget = budget();
    deadline_budget.deadline_micros = Some(10);
    let active = FixedHydrationExecutionControl {
        elapsed_micros: 1,
        cancelled: false,
    };
    let mut active_source = PreflightHydrationSource {
        authorizations: 0,
        preflights: 0,
        reads: 0,
        estimated_bytes: 1,
        mismatched_receipt: false,
        remaining_deadlines: Vec::new(),
    };

    let active_page = CanonicalLateHydration::new(&mut active_source)
        .hydrate_with_control(&request, &ranked, &deadline_budget, &active)
        .expect("an older generation does not consume the request deadline");

    assert_eq!(
        (
            active_source.authorizations,
            active_source.preflights,
            active_source.reads,
        ),
        (1, 1, 1)
    );
    assert!(matches!(
        active_page.results[0].outcome,
        HydrationOutcomeV1::Complete(_)
    ));
    assert_eq!(active_source.remaining_deadlines, vec![Some(9), Some(9)]);

    let cancelled = FixedHydrationExecutionControl {
        elapsed_micros: 1,
        cancelled: true,
    };
    let mut cancelled_source = PreflightHydrationSource {
        authorizations: 0,
        preflights: 0,
        reads: 0,
        estimated_bytes: 1,
        mismatched_receipt: false,
        remaining_deadlines: Vec::new(),
    };
    let cancelled_page = CanonicalLateHydration::new(&mut cancelled_source)
        .hydrate_with_control(&request, &ranked, &deadline_budget, &cancelled)
        .expect("cancellation remains a positional result");

    assert_eq!(
        (
            cancelled_source.authorizations,
            cancelled_source.preflights,
            cancelled_source.reads,
        ),
        (0, 0, 0)
    );
    assert!(matches!(
        cancelled_page.results[0].outcome,
        HydrationOutcomeV1::Unavailable(HydrationUnavailableV1::Cancelled)
    ));
}

#[test]
fn default_hydration_control_does_not_charge_generation_age_to_request_deadline() {
    let mut request = request();
    request.snapshot.captured_at = tracedecay_domain::UtcMicros(0);
    let ranked = single_ranked_candidate();
    let mut deadline_budget = budget();
    deadline_budget.deadline_micros = Some(1_000_000);
    let mut source = PreflightHydrationSource {
        authorizations: 0,
        preflights: 0,
        reads: 0,
        estimated_bytes: 1,
        mismatched_receipt: false,
        remaining_deadlines: Vec::new(),
    };

    let page = CanonicalLateHydration::new(&mut source)
        .hydrate(&request, &ranked, &deadline_budget)
        .expect("an older generation does not consume the request deadline");

    assert_eq!(
        (source.authorizations, source.preflights, source.reads),
        (1, 1, 1)
    );
    assert!(matches!(
        page.results[0].outcome,
        HydrationOutcomeV1::Complete(_)
    ));
}

#[test]
fn hydration_rejects_receipts_outside_the_issued_work_permit() {
    let request = request();
    let ranked = single_ranked_candidate();
    let control = FixedHydrationExecutionControl {
        elapsed_micros: 0,
        cancelled: false,
    };
    let mut source = PreflightHydrationSource {
        authorizations: 0,
        preflights: 0,
        reads: 0,
        estimated_bytes: 1,
        mismatched_receipt: true,
        remaining_deadlines: Vec::new(),
    };

    assert!(matches!(
        CanonicalLateHydration::new(&mut source).hydrate_with_control(
            &request,
            &ranked,
            &budget(),
            &control,
        ),
        Err(HydrationStageError::Contract(_))
    ));
    assert_eq!(
        (source.authorizations, source.preflights, source.reads),
        (1, 1, 1)
    );
}

struct MutableHydrationExecutionControl {
    elapsed_micros: u64,
    cancelled: Rc<Cell<bool>>,
}

impl HydrationExecutionControlV1 for MutableHydrationExecutionControl {
    fn elapsed_micros(&self) -> u64 {
        self.elapsed_micros
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.get()
    }
}

struct CancellingHydrationSource {
    cancelled: Rc<Cell<bool>>,
}

impl LateHydrationSource<String> for CancellingHydrationSource {
    fn authorize(
        &mut self,
        _request: &tracedecay_domain::RetrievalRequest,
        _candidate: &tracedecay_domain::RankedCandidate,
    ) -> HydrationAuthorizationV1 {
        HydrationAuthorizationV1::Authorized
    }

    fn preflight_authorized(
        &mut self,
        _request: &tracedecay_domain::RetrievalRequest,
        _candidate: &tracedecay_domain::RankedCandidate,
        _permit: &HydrationWorkPermitV1,
    ) -> HydrationPreflightOutcomeV1 {
        HydrationPreflightOutcomeV1::Ready { estimated_bytes: 1 }
    }

    fn hydrate_authorized(
        &mut self,
        _request: &tracedecay_domain::RetrievalRequest,
        candidate: &tracedecay_domain::RankedCandidate,
        _permit: &HydrationWorkPermitV1,
    ) -> HydrationReadOutcomeV1<String> {
        self.cancelled.set(true);
        HydrationReadOutcomeV1::Complete {
            payload: "must not publish".to_owned(),
            receipt: receipt(candidate, 1),
        }
    }
}

#[test]
fn hydration_rechecks_cancellation_after_source_work_before_publishing_payload() {
    let request = request();
    let ranked = single_ranked_candidate();
    let cancelled = Rc::new(Cell::new(false));
    let control = MutableHydrationExecutionControl {
        elapsed_micros: 0,
        cancelled: Rc::clone(&cancelled),
    };
    let mut source = CancellingHydrationSource { cancelled };

    let page = CanonicalLateHydration::new(&mut source)
        .hydrate_with_control(&request, &ranked, &budget(), &control)
        .expect("cancellation is a positional result");

    assert!(matches!(
        page.results[0].outcome,
        HydrationOutcomeV1::Unavailable(HydrationUnavailableV1::Cancelled)
    ));
    assert!(page.receipts.is_empty());
}
