use std::collections::BTreeMap;

use tracedecay_domain::{
    DiversityPolicy, EvidenceRole, ExactClass, HydrationReceipt, PublicRetrieverStatus,
    RankingDecisionKind, RetrievalFailure, RetrieverKind, RetrieverOutcome,
};

use super::{
    batch, budget, candidate, composition_lanes, exact_candidate, id, no_caps, profile, request,
};
use crate::query::retrieval::fusion::{CompositionKernel, FusionStageInput};
use crate::query::retrieval::hydrate::{
    DeterministicLateHydration, HydrationAuthorizationV1, HydrationOutcomeV1,
    HydrationReadOutcomeV1, HydrationUnavailableV1, LateHydrationSource,
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
    primary.freshness.source_instance = id("file.same");

    let mut copy = candidate(RetrieverKind::Lexical, "copy", 800_000, 1);
    copy.logical_copy_cluster_id = Some(id("copy.same"));
    copy.freshness.source_instance = id("file.same");

    let mut contradiction = candidate(RetrieverKind::Lexical, "contradiction", 700_000, 2);
    contradiction.logical_copy_cluster_id = Some(id("copy.same"));
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
            .cloned()
            .unwrap_or(HydrationAuthorizationV1::Authorized)
    }

    fn hydrate_authorized(
        &mut self,
        _request: &tracedecay_domain::RetrievalRequest,
        candidate: &tracedecay_domain::RankedCandidate,
        _remaining_bytes: u64,
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
    let page = DeterministicLateHydration::new(&mut source)
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
    let unavailable_page = DeterministicLateHydration::new(&mut unavailable_source)
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

    fn hydrate_authorized(
        &mut self,
        _request: &tracedecay_domain::RetrievalRequest,
        candidate: &tracedecay_domain::RankedCandidate,
        remaining_bytes: u64,
    ) -> HydrationReadOutcomeV1<String> {
        let anchor = candidate.candidate.anchor_id.as_str().to_owned();
        self.reads.push(anchor.clone());
        let bytes = if anchor == "anchor.first" { 4 } else { 3 };
        assert!(bytes <= remaining_bytes);
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
    let page = DeterministicLateHydration::new(&mut source)
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
