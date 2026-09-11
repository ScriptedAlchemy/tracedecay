use tracedecay_domain::{
    EvidenceRole, ExactClass, FreshnessCompatibilityV1, FusedCandidate, OccurrenceProvenance,
    RankingDecision, RankingDecisionKind, RetrievalAnchorId, SourceFreshness, UtcMicros,
};

use super::{composition_lanes, corpus_lanes, id, mixed_caps, no_caps, profile};
use crate::retrieval::fusion::{
    CompositionKernel, CompositionOutputV1, DeterministicFixedPointFusion, FusionStageInput,
    digest_candidate_set,
};
use crate::retrieval::ordering::{compare_fused, decision_cmp};
use crate::retrieval::stage_counters;

fn compose_corpus(policy: &tracedecay_domain::DiversityPolicy) -> CompositionOutputV1 {
    CompositionKernel::new(id("ranking.fixture.v1"))
        .compose(
            &FusionStageInput {
                profile: profile(),
                lanes: composition_lanes(corpus_lanes()),
            },
            policy,
        )
        .expect("corpus composes")
}

fn provenance_count(candidate: &FusedCandidate) -> usize {
    candidate
        .decisions
        .iter()
        .filter(|decision| decision.kind == RankingDecisionKind::ComparatorProvenance)
        .count()
}

fn is_sorted_by<T>(items: &[T], compare: impl Fn(&T, &T) -> std::cmp::Ordering) -> bool {
    items
        .windows(2)
        .all(|pair| compare(&pair[0], &pair[1]) != std::cmp::Ordering::Greater)
}

fn decisions_are_sorted(decisions: &[RankingDecision]) -> bool {
    is_sorted_by(decisions, decision_cmp)
}

#[test]
fn composition_sorts_once_and_keeps_one_provenance_per_fused_candidate() {
    stage_counters::reset();
    let output = compose_corpus(&no_caps());
    let work = stage_counters::snapshot();

    let collapsed = output
        .dedupe_decisions
        .iter()
        .flat_map(|decision| &decision.collapsed_candidates)
        .collect::<Vec<_>>();
    // The corpus must actually exercise ties, clusters, and collapse.
    assert_eq!(output.ranked_candidates.len(), 18);
    assert_eq!(collapsed.len(), 8);

    assert_eq!(
        work.fused_sorts, 1,
        "fused candidates are sorted exactly once"
    );
    assert_eq!(
        work.comparator_records,
        output.ranked_candidates.len() + collapsed.len(),
        "one comparator record per fused candidate, surviving or collapsed"
    );

    // Survivors are in comparator order without any later re-sort.
    let mut reference = output
        .ranked_candidates
        .iter()
        .map(|ranked| ranked.candidate.clone())
        .collect::<Vec<_>>();
    reference.sort_by(compare_fused);
    assert!(
        output
            .ranked_candidates
            .iter()
            .map(|ranked| &ranked.candidate)
            .eq(reference.iter())
    );
    assert!(
        output
            .ranked_candidates
            .iter()
            .enumerate()
            .all(|(ordinal, ranked)| ranked.final_ordinal as usize == ordinal)
    );

    // The reported comparator records are the ones built at ordering time
    // and match the ranked candidates field for field.
    let fusion = DeterministicFixedPointFusion::new(id("ranking.fixture.v1"));
    assert_eq!(
        output.comparator_records.len(),
        output.ranked_candidates.len()
    );
    for (record, ranked) in output
        .comparator_records
        .iter()
        .zip(&output.ranked_candidates)
    {
        assert_eq!(*record, fusion.comparator_record(&ranked.candidate));
    }

    for candidate in output
        .ranked_candidates
        .iter()
        .map(|ranked| &ranked.candidate)
        .chain(collapsed.iter().copied())
    {
        assert_eq!(
            provenance_count(candidate),
            1,
            "{} carries exactly one comparator provenance decision",
            candidate.anchor_id
        );
        assert!(
            decisions_are_sorted(&candidate.decisions),
            "{} keeps its decisions in canonical order",
            candidate.anchor_id
        );
    }
}

#[test]
fn representative_selection_preserves_order_and_moves_collapsed_copies() {
    let output = compose_corpus(&no_caps());
    let copy_decisions = output
        .dedupe_decisions
        .iter()
        .filter(|decision| {
            decision.decision.kind == RankingDecisionKind::LogicalCopyRepresentativeSelection
        })
        .collect::<Vec<_>>();

    // Four clusters collapse two copies each; the cluster holding the
    // contradiction and the corroboration keeps every member independent.
    assert_eq!(
        copy_decisions
            .iter()
            .map(|decision| decision
                .copy_cluster
                .as_ref()
                .map(|cluster| cluster.as_str()))
            .collect::<Vec<_>>(),
        vec![
            Some("copy.0"),
            Some("copy.1"),
            Some("copy.3"),
            Some("copy.4")
        ]
    );
    for decision in &copy_decisions {
        assert_eq!(decision.collapsed_candidates.len(), 2);
        let representative = output
            .ranked_candidates
            .iter()
            .map(|ranked| &ranked.candidate)
            .find(|candidate| {
                candidate.occurrences[0].source_occurrence_id == decision.kept_occurrence
            })
            .expect("representative survives");
        assert!(representative.decisions.contains(&decision.decision));
        assert!(is_sorted_by(&decision.collapsed_candidates, compare_fused));
        assert!(decision.collapsed_candidates.iter().all(|collapsed| {
            compare_fused(representative, collapsed) == std::cmp::Ordering::Less
        }));
        assert!(
            decision
                .collapsed_occurrences
                .iter()
                .all(|occurrence| *occurrence != decision.kept_occurrence)
        );
    }

    let survivor = |name: &str| {
        output
            .ranked_candidates
            .iter()
            .map(|ranked| &ranked.candidate)
            .find(|candidate| candidate.anchor_id.as_str() == name)
    };
    let contradiction = survivor("anchor.c11").expect("contradiction is preserved");
    assert!(
        contradiction
            .decisions
            .iter()
            .any(|decision| { decision.kind == RankingDecisionKind::ContradictionPreservation })
    );
    assert!(decisions_are_sorted(&contradiction.decisions));
    assert!(
        survivor("anchor.c12").is_some(),
        "corroboration stays independent"
    );
    assert!(
        survivor("anchor.c10").is_some(),
        "lone cluster member is its own representative"
    );
    assert!(
        survivor("anchor.c01").is_none(),
        "collapsed copy leaves the ranking"
    );
}

#[test]
fn capped_composition_is_lane_order_invariant_and_digest_stable() {
    let kernel = CompositionKernel::new(id("ranking.fixture.v1"));
    let expected = compose_corpus(&mixed_caps());
    assert!(
        !expected.diversity_decisions.is_empty(),
        "mixed caps must bite on the corpus"
    );
    let expected_digest = digest_candidate_set(&expected.ranked_candidates).unwrap();

    for iteration in 0..6 {
        let mut lanes = corpus_lanes();
        let offset = iteration % lanes.len();
        lanes.rotate_left(offset);
        if iteration % 2 == 1 {
            lanes.reverse();
        }
        let output = kernel
            .compose(
                &FusionStageInput {
                    profile: profile(),
                    lanes: composition_lanes(lanes),
                },
                &mixed_caps(),
            )
            .expect("permuted corpus composes");
        assert_eq!(
            output, expected,
            "lane permutation {iteration} changed composition"
        );
        assert_eq!(
            digest_candidate_set(&output.ranked_candidates).unwrap(),
            expected_digest
        );
    }
}

fn generation_scoped_hit(
    stable_name: &str,
    generation_tag: &str,
    occurrence_sort_key: &str,
) -> FusedCandidate {
    FusedCandidate {
        anchor_id: RetrievalAnchorId::new(format!(
            "code-symbol:symbol.v1.{generation_tag}.{occurrence_sort_key}"
        ))
        .expect("valid generation-scoped anchor"),
        logical_evidence_id: id(&format!(
            "code-rank:{stable_name}|src/{stable_name}.rs|function"
        )),
        occurrences: vec![OccurrenceProvenance {
            source_occurrence_id: id(&format!(
                "code-chunk:{generation_tag}:{occurrence_sort_key}"
            )),
            file_occurrence_id: None,
            retriever_evidence_anchor: RetrievalAnchorId::new(format!(
                "code-lexical:lexical:chunk.{stable_name}"
            ))
            .expect("valid evidence anchor"),
            source_namespace: id("namespace.code"),
            repository_id: None,
            session_or_thread_id: None,
            logical_copy_cluster_id: None,
            logical_copy_evidence_anchor: None,
            evidence_role: EvidenceRole::Primary,
            freshness: SourceFreshness {
                source_namespace: id("namespace.code"),
                source_instance: id("instance.code"),
                source_watermark: Some(7),
                projection_watermark: Some(7),
                observed_at: UtcMicros(7),
                source_generation: Some(1),
                generation_lag: Some(0),
                compatibility: FreshnessCompatibilityV1::Current,
                policy_revision: id("policy.fixture.v1"),
            },
        }],
        exact_class: ExactClass::Approximate,
        utility_micros: 4_000_000,
        contributions: Vec::new(),
        freshness: Vec::new(),
        decisions: Vec::new(),
    }
}

#[test]
fn fused_order_ignores_generation_scoped_ids_among_equal_utilities() {
    // Same utilities and the same generation-independent lexical evidence
    // anchors (`code-lexical:{lane}:{chunk_id}`). Generation A hashes sort
    // zeta before alpha; generation B reverses that. Ranking must follow the
    // chunk-stable anchors, not the rematerialized occurrence ids.
    let mut generation_a = vec![
        generation_scoped_hit("zeta", "gen-a", "aaa"),
        generation_scoped_hit("alpha", "gen-a", "zzz"),
    ];
    let mut generation_b = vec![
        generation_scoped_hit("zeta", "gen-b", "zzz"),
        generation_scoped_hit("alpha", "gen-b", "aaa"),
    ];
    generation_a.sort_by(compare_fused);
    generation_b.sort_by(compare_fused);

    let names = |candidates: &[FusedCandidate]| {
        candidates
            .iter()
            .map(|candidate| candidate.logical_evidence_id.as_str().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&generation_a), names(&generation_b));
    assert_eq!(
        names(&generation_a),
        vec![
            "code-rank:alpha|src/alpha.rs|function",
            "code-rank:zeta|src/zeta.rs|function",
        ]
    );
}

#[test]
fn comparator_record_retains_the_actual_evidence_tie_break() {
    let fusion = DeterministicFixedPointFusion::new(id("ranking.fixture.v1"));
    let mut left = generation_scoped_hit("alpha", "a", "zzz");
    let right = generation_scoped_hit("zeta", "b", "aaa");
    let mut other = left.occurrences[0].clone();
    other.retriever_evidence_anchor = id("code-lexical:lexical:chunk.omega");
    left.occurrences
        .extend([other, left.occurrences[0].clone()]);
    left.occurrences.reverse();
    let record = fusion.comparator_record(&left);
    assert_eq!(
        record.retriever_evidence_anchors,
        vec![
            id("code-lexical:lexical:chunk.alpha"),
            id("code-lexical:lexical:chunk.omega"),
        ]
    );
    assert_eq!(
        compare_fused(&left, &right),
        record
            .retriever_evidence_anchors
            .cmp(&fusion.comparator_record(&right).retriever_evidence_anchors)
    );
    let output = compose_corpus(&no_caps());
    for (record, ranked) in output
        .comparator_records
        .iter()
        .zip(&output.ranked_candidates)
    {
        let detail = &ranked
            .candidate
            .decisions
            .iter()
            .find(|decision| decision.kind == RankingDecisionKind::ComparatorProvenance)
            .unwrap()
            .detail;
        let anchors = record
            .retriever_evidence_anchors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        assert!(detail.contains(&format!("evidence_anchors=[{anchors}]")));
        assert!(!detail.contains(";anchor="));
        assert!(!detail.contains(";logical="));
    }
}

#[test]
fn saturated_scores_preserve_retriever_strength_before_identity_ties() {
    use tracedecay_domain::{
        CandidateContribution, FixedPointScore, RetrieverKind, ScoreDomainCalibrationV1,
    };

    let calibration = ScoreDomainCalibrationV1 {
        calibration_profile_id: id("calibration.lexical"),
        score_domain: id("score.lexical"),
        raw_min_micros: 0,
        raw_max_micros: 1_000_000,
    };
    let contribution = |raw| CandidateContribution {
        retriever: RetrieverKind::Lexical,
        retriever_revision: id("retriever.lexical"),
        source_occurrence_id: id("occurrence.lexical"),
        ordinal_rank: 0,
        raw_score: FixedPointScore(raw),
        score_domain: calibration.score_domain.clone(),
        calibration_profile_id: calibration.calibration_profile_id.clone(),
        calibrated_feature_micros: calibration.calibrate(FixedPointScore(raw)).unwrap(),
        weight_micros: 1_000_000,
        weighted_contribution_micros: 1_000_000,
    };
    let mut weaker = generation_scoped_hit("alpha", "a", "aaa");
    let mut stronger = generation_scoped_hit("zeta", "b", "zzz");
    weaker.contributions = vec![contribution(4_000_000)];
    stronger.contributions = vec![contribution(5_000_000)];
    assert_eq!(
        weaker.contributions[0].calibrated_feature_micros,
        stronger.contributions[0].calibrated_feature_micros
    );
    assert_eq!(compare_fused(&stronger, &weaker), std::cmp::Ordering::Less);

    let fusion = DeterministicFixedPointFusion::new(id("ranking.fixture.v1"));
    assert_eq!(
        fusion.comparator_record(&stronger).domain_scores,
        vec![(
            RetrieverKind::Lexical,
            id("score.lexical"),
            FixedPointScore(5_000_000)
        ),]
    );
    let mut disabled = contribution(u64::MAX);
    disabled.weight_micros = 0;
    weaker.contributions.push(disabled);
    assert_eq!(compare_fused(&stronger, &weaker), std::cmp::Ordering::Less);
    weaker.utility_micros += 1;
    assert_eq!(compare_fused(&weaker, &stronger), std::cmp::Ordering::Less);

    // Numeric magnitudes never cross score-domain scales.
    weaker.utility_micros = stronger.utility_micros;
    weaker.contributions = vec![contribution(u64::MAX)];
    weaker.contributions[0].score_domain = id("score.unrelated");
    let domain_order = compare_fused(&stronger, &weaker);
    assert_eq!(
        domain_order,
        calibration
            .score_domain
            .cmp(&weaker.contributions[0].score_domain)
    );
    weaker.contributions[0].raw_score = FixedPointScore(0);
    assert_eq!(compare_fused(&stronger, &weaker), domain_order);

    // At identical primary utility, different lane mixes use the recorded
    // retriever tag order, not a magnitude comparison across score scales.
    weaker.contributions[0].retriever = RetrieverKind::Semantic;
    weaker.contributions[0].score_domain = id("score.semantic");
    assert_eq!(
        compare_fused(&stronger, &weaker),
        RetrieverKind::Lexical.cmp(&RetrieverKind::Semantic)
    );
}
