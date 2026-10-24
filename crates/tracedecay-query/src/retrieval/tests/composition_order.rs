use std::cmp::{Ordering, Reverse};
use std::collections::BTreeMap;

use tracedecay_domain::{
    CandidateContribution, EvidenceRole, ExactClass, FixedPointScore, FreshnessCompatibilityV1,
    FusedCandidate, OccurrenceProvenance, RankedCandidate, RankingDecision, RankingDecisionKind,
    RetrievalAnchorId, RetrievalSourceRoleV1, RetrieverKind, SourceFreshness, UtcMicros,
};

use super::{composition_lanes, corpus_lanes, id, mixed_caps, no_caps, profile};
use crate::retrieval::fusion::{
    CompositionKernel, CompositionOutputV1, DeterministicFixedPointFusion, FusionStageInput,
    digest_candidate_set,
};
use crate::retrieval::ordering::{OrderedFusedCandidates, decision_cmp};
use crate::retrieval::stage_counters;

// The former comparator remains an independent test oracle. Production
// comparison reads only prepared records; this intentionally reconstructs
// each tie-break vector from the candidate, as the original implementation did.
fn compare_fused(left: &FusedCandidate, right: &FusedCandidate) -> Ordering {
    fn canonical<T: Ord>(items: impl Iterator<Item = T>) -> Vec<T> {
        let mut items = items.collect::<Vec<_>>();
        items.sort();
        items.dedup();
        items
    }
    let exact = |class| -> u8 {
        match class {
            ExactClass::ExactMessage => 0,
            ExactClass::ExactLiteralPhrase => 1,
            ExactClass::Approximate => 2,
        }
    };
    let validity = |candidate: &FusedCandidate| -> u8 {
        candidate
            .freshness
            .iter()
            .map(|freshness| match freshness.compatibility {
                FreshnessCompatibilityV1::Current => 4,
                FreshnessCompatibilityV1::Unknown => 3,
                FreshnessCompatibilityV1::Stale => 2,
                FreshnessCompatibilityV1::Missing => 1,
                FreshnessCompatibilityV1::Incompatible => 0,
            })
            .max()
            .unwrap_or(0)
    };
    exact(left.exact_class)
        .cmp(&exact(right.exact_class))
        .then_with(|| {
            left.source_role
                .admission_rank()
                .cmp(&right.source_role.admission_rank())
        })
        .then_with(|| right.utility_micros.cmp(&left.utility_micros))
        .then_with(|| validity(right).cmp(&validity(left)))
        .then_with(|| {
            canonical(
                left.contributions
                    .iter()
                    .filter(|value| value.weight_micros > 0)
                    .map(|value| {
                        (
                            value.retriever,
                            &value.score_domain,
                            Reverse(value.raw_score),
                        )
                    }),
            )
            .cmp(&canonical(
                right
                    .contributions
                    .iter()
                    .filter(|value| value.weight_micros > 0)
                    .map(|value| {
                        (
                            value.retriever,
                            &value.score_domain,
                            Reverse(value.raw_score),
                        )
                    }),
            ))
        })
        .then_with(|| {
            canonical(
                left.occurrences
                    .iter()
                    .map(|value| &value.retriever_evidence_anchor),
            )
            .cmp(&canonical(
                right
                    .occurrences
                    .iter()
                    .map(|value| &value.retriever_evidence_anchor),
            ))
        })
        .then_with(|| {
            canonical(
                left.occurrences
                    .iter()
                    .map(|value| &value.source_occurrence_id),
            )
            .cmp(&canonical(
                right
                    .occurrences
                    .iter()
                    .map(|value| &value.source_occurrence_id),
            ))
        })
}

fn ranking_key_variants() -> Vec<FusedCandidate> {
    let mut base = generation_scoped_hit("shared", "generation-a", "shared");
    base.freshness = vec![base.occurrences[0].freshness.clone()];
    base.contributions = [RetrieverKind::Lexical, RetrieverKind::Graph]
        .into_iter()
        .map(|retriever| CandidateContribution {
            retriever,
            retriever_revision: id("retriever.fixture"),
            source_occurrence_id: base.occurrences[0].source_occurrence_id.clone(),
            ordinal_rank: 0,
            raw_score: FixedPointScore(1_000_000),
            score_domain: id(&format!("score.{}", retriever.as_str())),
            calibration_profile_id: id("calibration.fixture"),
            calibrated_feature_micros: 1_000_000,
            weight_micros: 1_000_000,
            weighted_contribution_micros: 1_000_000,
        })
        .collect();
    let variant = |name: &str, update: fn(&mut FusedCandidate)| {
        let mut candidate = base.clone();
        candidate.anchor_id = id(&format!("anchor.{name}"));
        candidate.logical_evidence_id = id(&format!("logical.{name}"));
        candidate.decisions.push(RankingDecision {
            kind: RankingDecisionKind::ComparatorProvenance,
            retriever: None,
            policy_anchor: None,
            evidence_anchor: None,
            detail: name.to_owned(),
        });
        update(&mut candidate);
        candidate
    };
    vec![
        variant("base", |_| {}),
        variant("exact-message", |value| {
            value.exact_class = ExactClass::ExactMessage
        }),
        variant("exact-phrase", |value| {
            value.exact_class = ExactClass::ExactLiteralPhrase
        }),
        variant("definition", |value| {
            value.source_role = RetrievalSourceRoleV1::ProductionDefinition
        }),
        variant("higher-utility", |value| value.utility_micros += 1),
        variant("unknown", |value| {
            value.freshness[0].compatibility = FreshnessCompatibilityV1::Unknown
        }),
        variant("stale", |value| {
            value.freshness[0].compatibility = FreshnessCompatibilityV1::Stale
        }),
        variant("missing", |value| {
            value.freshness[0].compatibility = FreshnessCompatibilityV1::Missing
        }),
        variant("incompatible", |value| {
            value.freshness[0].compatibility = FreshnessCompatibilityV1::Incompatible
        }),
        variant("no-freshness", |value| value.freshness.clear()),
        variant("stronger-raw", |value| {
            value.contributions[0].raw_score = FixedPointScore(2_000_000)
        }),
        variant("other-domain", |value| {
            value.contributions[0].score_domain = id("score.unrelated");
            value.contributions[0].raw_score = FixedPointScore(u64::MAX);
        }),
        variant("other-retriever", |value| {
            value.contributions[1].retriever = RetrieverKind::ExactLiteral
        }),
        variant("zero-weight", |value| {
            let mut disabled = value.contributions[0].clone();
            disabled.weight_micros = 0;
            disabled.raw_score = FixedPointScore(u64::MAX);
            value.contributions.push(disabled);
        }),
        variant("duplicate-evidence", |value| {
            value.contributions.push(value.contributions[0].clone());
            value.occurrences.push(value.occurrences[0].clone());
            value.contributions.reverse();
        }),
        variant("other-anchor", |value| {
            value.occurrences[0].retriever_evidence_anchor = id("evidence.omega")
        }),
        variant("other-generation", |value| {
            value.occurrences[0].source_occurrence_id = id("code-chunk:generation-b:shared")
        }),
        variant("equal-key-metadata", |_| {}),
        variant("higher-tier", |value| value.utility_micros = 0),
    ]
}

#[test]
fn prepared_ranking_keys_match_the_original_order_with_one_normalization() {
    let fusion = DeterministicFixedPointFusion::new(id("ranking.fixture.v1"));
    let anchor_tiers: BTreeMap<RetrievalAnchorId, u32> = BTreeMap::from([
        (id("anchor.higher-tier"), 2),
        (id("anchor.exact-message"), 1),
    ]);
    let variants = ranking_key_variants();
    let variant = |name: &str| {
        variants
            .iter()
            .find(|candidate| candidate.anchor_id.as_str() == name)
            .unwrap()
    };
    assert_eq!(
        compare_fused(variant("anchor.base"), variant("anchor.equal-key-metadata")),
        Ordering::Equal,
        "metadata and decisions must not invent another identity discriminator"
    );
    assert_eq!(
        compare_fused(variant("anchor.base"), variant("anchor.other-generation")),
        Ordering::Less,
        "generation-scoped occurrence identity remains the final discriminator"
    );
    for distinct_utilities in [false, true] {
        for permutation in 0..variants.len() * 2 {
            let mut input = variants.clone();
            if distinct_utilities {
                for (index, candidate) in input.iter_mut().enumerate() {
                    candidate.utility_micros += index as u64;
                }
            }
            input.rotate_left(permutation % variants.len());
            if permutation % 2 != 0 {
                input.reverse();
            }
            let mut expected = input.clone();
            expected.sort_by(|left, right| {
                Reverse(anchor_tiers.get(&left.anchor_id).copied().unwrap_or(0))
                    .cmp(&Reverse(
                        anchor_tiers.get(&right.anchor_id).copied().unwrap_or(0),
                    ))
                    .then_with(|| compare_fused(left, right))
            });

            stage_counters::reset();
            let (actual, records) = OrderedFusedCandidates::sort(input, |candidate| {
                fusion.comparator_record(
                    candidate,
                    anchor_tiers.get(&candidate.anchor_id).copied().unwrap_or(0),
                )
            });
            let work = stage_counters::snapshot();
            let actual = actual.into_vec();
            assert_eq!(
                actual, expected,
                "permutation {permutation}, distinct utilities {distinct_utilities}"
            );
            assert_eq!(work.comparator_records, actual.len());
            assert_eq!(work.ranking_evidence_normalizations, 3 * actual.len());
            assert_eq!(records.len(), actual.len());
            for (candidate, record) in actual.iter().zip(&records) {
                assert_eq!(
                    *record,
                    fusion.comparator_record(candidate, record.anchor_tier)
                );
                assert!(record.domain_scores.len() <= candidate.contributions.len());
                assert!(record.retriever_evidence_anchors.len() <= candidate.occurrences.len());
                assert!(record.source_occurrence_ids.len() <= candidate.occurrences.len());
            }
            let ranked = |candidates: Vec<FusedCandidate>| {
                candidates
                    .into_iter()
                    .enumerate()
                    .map(|(index, candidate)| RankedCandidate {
                        candidate,
                        final_ordinal: index as u32,
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                digest_candidate_set(&ranked(actual)).unwrap(),
                digest_candidate_set(&ranked(expected)).unwrap(),
            );
        }
    }
}

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
    assert_eq!(
        work.ranking_evidence_normalizations,
        3 * work.comparator_records,
        "domain scores, evidence anchors and occurrence identities normalize once per fused candidate"
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
        assert_eq!(*record, fusion.comparator_record(&ranked.candidate, 0));
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
        source_role: RetrievalSourceRoleV1::default(),
        utility_micros: 4_000_000,
        contributions: Vec::new(),
        freshness: Vec::new(),
        decisions: Vec::new(),
    }
}

#[test]
fn fused_order_ranks_production_definitions_ahead_of_stronger_test_hits() {
    let mut definition = generation_scoped_hit("ensure_daemon", "gen", "prod");
    let mut test_hit = generation_scoped_hit("ensure_daemon_test", "gen", "test");
    definition.source_role = tracedecay_domain::RetrievalSourceRoleV1::ProductionDefinition;
    definition.utility_micros = 100_000;
    test_hit.source_role = tracedecay_domain::RetrievalSourceRoleV1::TestReference;
    test_hit.utility_micros = 4_000_000;
    assert_eq!(
        compare_fused(&definition, &test_hit),
        std::cmp::Ordering::Less
    );

    let mut comment = generation_scoped_hit("ensure_daemon_comment", "gen", "comment");
    comment.source_role = tracedecay_domain::RetrievalSourceRoleV1::ProductionOther;
    comment.utility_micros = 100_000;
    assert_eq!(compare_fused(&test_hit, &comment), std::cmp::Ordering::Less);
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
    let record = fusion.comparator_record(&left, 0);
    assert_eq!(
        record.retriever_evidence_anchors,
        vec![
            id("code-lexical:lexical:chunk.alpha"),
            id("code-lexical:lexical:chunk.omega"),
        ]
    );
    assert_eq!(
        compare_fused(&left, &right),
        record.retriever_evidence_anchors.cmp(
            &fusion
                .comparator_record(&right, 0)
                .retriever_evidence_anchors
        )
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
        fusion.comparator_record(&stronger, 0).domain_scores,
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
    weaker.contributions[0].retriever = RetrieverKind::Graph;
    weaker.contributions[0].score_domain = id("score.graph");
    assert_eq!(
        compare_fused(&stronger, &weaker),
        RetrieverKind::Lexical.cmp(&RetrieverKind::Graph)
    );
}
