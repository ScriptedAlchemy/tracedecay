//! Lexical route behavior: anchor validation bounds, preferred-symbol token
//! extraction, and the deterministic merge of route batches into one lexical
//! lane input with anchor-named evidence.

use std::collections::BTreeMap;
use std::fmt;

use tracedecay_domain::{
    CodeGenerationId, CompactCandidate, EvidenceRole, FixedPointScore, FreshnessCompatibilityV1,
    RetrievalBudget, RetrievalFailure, RetrieverBatch, RetrieverContinuation, RetrieverCoverage,
    RetrieverKind, RetrieverOutcome, SourceFreshness, UtcMicros,
};

use super::{
    LexicalAnchorV1, LexicalRouteErrorV1, LexicalRouteKindV1, LexicalRouteOutcomeV1,
    LexicalRoutePlanV1, LexicalRoutingV1, MAX_LEXICAL_ANCHOR_BYTES_V1, MAX_LEXICAL_ANCHORS_V1,
    MAX_PREFERRED_SYMBOL_TOKENS_V1, merge_lexical_routes, preferred_symbol_tokens,
};
use crate::retrieval::lexical::{LexicalFieldFilterV1, LexicalFieldV1, LexicalLaneEvidence};
use crate::retrieval::ports::{CodeCandidateBindingV1, CodeOccurrenceRefV1, RetrievalPortError};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid fixture identity")
}

fn anchors(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn anchor(value: &str) -> LexicalAnchorV1 {
    LexicalRoutingV1::new(anchors(&[value]), false)
        .expect("valid anchor")
        .anchors
        .remove(0)
}

#[test]
fn routing_bounds_anchor_count_and_bytes() {
    let too_many: Vec<String> = (0..=MAX_LEXICAL_ANCHORS_V1)
        .map(|index| format!("anchor_{index}"))
        .collect();
    assert_eq!(
        LexicalRoutingV1::new(too_many, false),
        Err(LexicalRouteErrorV1::TooManyAnchors {
            max: MAX_LEXICAL_ANCHORS_V1,
            actual: MAX_LEXICAL_ANCHORS_V1 + 1,
        })
    );
    let exactly_max: Vec<String> = (0..MAX_LEXICAL_ANCHORS_V1)
        .map(|index| format!("anchor_{index}"))
        .collect();
    assert_eq!(
        LexicalRoutingV1::new(exactly_max, false)
            .expect("the bound is inclusive")
            .anchors
            .len(),
        MAX_LEXICAL_ANCHORS_V1
    );

    let long = "a".repeat(MAX_LEXICAL_ANCHOR_BYTES_V1 + 1);
    assert_eq!(
        LexicalRoutingV1::new(vec!["fine".to_owned(), long], false),
        Err(LexicalRouteErrorV1::AnchorTooLong {
            index: 1,
            max: MAX_LEXICAL_ANCHOR_BYTES_V1,
        })
    );
}

#[test]
fn routing_rejects_empty_multi_term_control_and_duplicate_anchors() {
    assert_eq!(
        LexicalRoutingV1::new(anchors(&["reserve_stock", ""]), false),
        Err(LexicalRouteErrorV1::EmptyAnchor { index: 1 })
    );
    for invalid in ["reserve stock", " reserve_stock", "reserve\tstock", "!!!"] {
        assert_eq!(
            LexicalRoutingV1::new(anchors(&[invalid]), false),
            Err(LexicalRouteErrorV1::AnchorNotOneTerm { index: 0 }),
            "{invalid:?} is not one technical term"
        );
    }
    assert_eq!(
        LexicalRoutingV1::new(anchors(&["Foo::bar", "Foo::bar"]), false),
        Err(LexicalRouteErrorV1::DuplicateAnchor { index: 1 })
    );
    let accepted = LexicalRoutingV1::new(anchors(&["Foo::bar", "p/q.rs", "E0308"]), true)
        .expect("qualified names, paths, and codes are one technical term each");
    assert_eq!(accepted.anchors.len(), 3);
    assert!(accepted.prefer_symbol);
    assert!(!accepted.is_query_only());
    assert!(LexicalRoutingV1::query_only().is_query_only());
}

#[test]
fn preferred_symbol_tokens_follow_the_identifier_grammar_and_stoplist() {
    let cases: &[(&str, &[&str])] = &[
        (
            "explain the struct VectorWatermark and where merge_max is called",
            &["VectorWatermark", "merge_max", "called"],
        ),
        ("find Foo::bar and Baz.qux", &["bar", "qux"]),
        ("how does this function work", &["work"]),
        ("what is the type of x", &[]),
        ("CLASS Struct ENUM interface", &[]),
        (
            "retry_budget retry_budget RetryBudget",
            &["retry_budget", "RetryBudget"],
        ),
        (
            "trailing dot Foo. and 42 and _private",
            &["trailing", "dot", "Foo", "_private"],
        ),
        ("aa::bb::", &["bb"]),
        ("a::b::", &[]),
    ];
    for (query, expected) in cases {
        assert_eq!(
            preferred_symbol_tokens(query),
            expected
                .iter()
                .map(|token| (*token).to_owned())
                .collect::<Vec<_>>(),
            "query {query:?}"
        );
    }

    let many = (0..MAX_PREFERRED_SYMBOL_TOKENS_V1 + 4)
        .map(|index| format!("symbol_{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let tokens = preferred_symbol_tokens(&many);
    assert_eq!(tokens.len(), MAX_PREFERRED_SYMBOL_TOKENS_V1);
    assert_eq!(tokens[0], "symbol_0");
    assert_eq!(
        tokens[MAX_PREFERRED_SYMBOL_TOKENS_V1 - 1],
        format!("symbol_{}", MAX_PREFERRED_SYMBOL_TOKENS_V1 - 1),
        "the bound keeps the first tokens in query order"
    );
}

#[test]
fn route_plan_adds_one_route_per_anchor_and_a_symbol_route_only_with_tokens() {
    let routing =
        LexicalRoutingV1::new(anchors(&["reserve_stock", "Allocator"]), true).expect("routing");
    let plan =
        LexicalRoutePlanV1::plan("how does inventory allocation work", &routing).expect("plan");
    let kinds = plan.descriptors();
    assert_eq!(kinds.len(), 4);
    assert_eq!(kinds[0], LexicalRouteKindV1::Query);
    assert_eq!(
        kinds[1],
        LexicalRouteKindV1::Anchor {
            anchor: anchor("reserve_stock"),
        }
    );
    assert_eq!(
        kinds[2],
        LexicalRouteKindV1::Anchor {
            anchor: anchor("Allocator"),
        }
    );
    assert_eq!(
        kinds[3],
        LexicalRouteKindV1::PreferredSymbol {
            tokens: vec![
                "inventory".to_owned(),
                "allocation".to_owned(),
                "work".to_owned(),
            ],
        }
    );
    let symbol_route = &plan.routes()[3];
    assert_eq!(
        symbol_route.field_filters,
        vec![LexicalFieldFilterV1 {
            field: LexicalFieldV1::SymbolName,
            include: true,
        }]
    );
    assert!(symbol_route.parts.subtokens.is_empty());
    assert!(symbol_route.parts.phrases.is_empty());
    let anchor_route = &plan.routes()[1];
    assert_eq!(anchor_route.parts.whole_terms, ["reserve_stock"]);
    assert!(anchor_route.parts.subtokens.is_empty());

    let no_identifiers = LexicalRoutePlanV1::plan(
        "what is the type of a",
        &LexicalRoutingV1::new(Vec::new(), true).expect("routing"),
    )
    .expect("plan");
    assert_eq!(
        no_identifiers.descriptors(),
        vec![LexicalRouteKindV1::Query],
        "no identifier-shaped token means no preferred-symbol route"
    );
}

fn budget(max_candidates_per_lane: u32) -> RetrievalBudget {
    RetrievalBudget {
        max_candidates_per_lane,
        max_fused_candidates: 16,
        max_hydrated_results: 8,
        max_hydration_bytes: 65_536,
        deadline_micros: None,
    }
}

fn generation() -> CodeGenerationId {
    id("generation.routes")
}

fn freshness() -> SourceFreshness {
    SourceFreshness {
        source_namespace: id("ns.code.fixture"),
        source_instance: id("instance.fixture"),
        source_watermark: Some(7),
        projection_watermark: Some(7),
        observed_at: UtcMicros(7),
        source_generation: Some(1),
        generation_lag: Some(0),
        compatibility: FreshnessCompatibilityV1::Current,
        policy_revision: id("policy.fixture.v1"),
    }
}

fn pair(
    occurrence: &str,
    field_scores: &[(LexicalFieldV1, u64)],
    matched_whole_terms: &[&str],
) -> (CompactCandidate, LexicalLaneEvidence) {
    let raw_score = field_scores.iter().map(|(_, score)| *score).sum();
    let candidate = CompactCandidate {
        anchor_id: id(&format!("anchor.{occurrence}")),
        logical_evidence_id: id(&format!("logical.{occurrence}")),
        source_occurrence_id: id(occurrence),
        file_occurrence_id: None,
        source_namespace: id("ns.code.fixture"),
        repository_id: None,
        session_or_thread_id: None,
        logical_copy_cluster_id: None,
        logical_copy_evidence_anchor: None,
        evidence_role: EvidenceRole::Primary,
        retriever: RetrieverKind::Lexical,
        retriever_revision: id("retriever.lexical.v1"),
        score_domain: id(crate::retrieval::QUERY_LEXICAL_SCORE_DOMAIN_V1),
        raw_score: FixedPointScore(raw_score),
        ordinal_rank: 0,
        exact_admission_proof: None,
        retriever_evidence_anchor: id(&format!("evidence-anchor.{occurrence}")),
        freshness: freshness(),
    };
    let evidence = LexicalLaneEvidence {
        binding: CodeCandidateBindingV1 {
            candidate_anchor: candidate.anchor_id.clone(),
            occurrence: CodeOccurrenceRefV1 {
                generation: generation(),
                file: id(&format!("file.{occurrence}")),
                symbol: Some(id(&format!("symbol.{occurrence}"))),
                chunk: Some(id(&format!("chunk.{occurrence}"))),
            },
            language_descriptor_revision: id("language.rust.v1"),
            matched_term_kinds: Vec::new(),
            source_occurrence: candidate.source_occurrence_id.clone(),
        },
        field_scores_micros: field_scores.to_vec(),
        matched_whole_terms: matched_whole_terms
            .iter()
            .map(|term| (*term).to_owned())
            .collect(),
        matched_subtokens: Vec::new(),
        matched_phrases: Vec::new(),
        typo_recovery_applied: false,
        echo_penalty_applied: false,
    };
    (candidate, evidence)
}

/// A lane-shaped batch: score-descending order, sequential ordinals, and an
/// exhausted lexical continuation.
fn lane_batch(
    mut pairs: Vec<(CompactCandidate, LexicalLaneEvidence)>,
) -> RetrieverBatch<LexicalLaneEvidence> {
    pairs.sort_by(|left, right| right.0.raw_score.cmp(&left.0.raw_score));
    let mut candidates = Vec::new();
    let mut evidence_by_occurrence = BTreeMap::new();
    for (ordinal, (mut candidate, evidence)) in pairs.into_iter().enumerate() {
        candidate.ordinal_rank = ordinal as u32;
        evidence_by_occurrence.insert(candidate.source_occurrence_id.clone(), evidence);
        candidates.push(candidate);
    }
    let examined = candidates.len() as u64;
    RetrieverBatch {
        candidates,
        evidence_by_occurrence,
        coverage: RetrieverCoverage {
            examined,
            eligible: examined,
            excluded: 0,
            capped: 0,
            unknown: 0,
        },
        continuation: Some(RetrieverContinuation {
            lane: RetrieverKind::Lexical,
            checkpoint_digest: id(&format!("sha256:{}", "c".repeat(64))),
            exhausted: true,
        }),
    }
}

fn route(
    kind: LexicalRouteKindV1,
    batch: RetrieverBatch<LexicalLaneEvidence>,
) -> LexicalRouteOutcomeV1 {
    LexicalRouteOutcomeV1 {
        kind,
        outcome: RetrieverOutcome::Complete(batch),
    }
}

fn anchor_kind(value: &str) -> LexicalRouteKindV1 {
    LexicalRouteKindV1::Anchor {
        anchor: anchor(value),
    }
}

fn order(batch: &RetrieverBatch<LexicalLaneEvidence>) -> Vec<&str> {
    batch
        .candidates
        .iter()
        .map(|candidate| candidate.source_occurrence_id.as_str())
        .collect()
}

#[test]
fn query_only_routing_passes_the_lane_batch_through_untouched() {
    let batch = lane_batch(vec![
        pair(
            "occ.a",
            &[(LexicalFieldV1::BodyText, 300_000)],
            &["inventory"],
        ),
        pair(
            "occ.b",
            &[(LexicalFieldV1::BodyText, 200_000)],
            &["inventory"],
        ),
    ]);
    let (outcome, receipt) = merge_lexical_routes(
        &generation(),
        &budget(8),
        &budget(8),
        vec![route(LexicalRouteKindV1::Query, batch.clone())],
    )
    .expect("merge");
    assert_eq!(outcome, RetrieverOutcome::Complete(batch));
    assert_eq!(receipt.routes, vec![LexicalRouteKindV1::Query]);
    assert!(receipt.matches_by_anchor.is_empty());
    assert!(!receipt.has_additional_routes());
}

#[test]
fn anchor_route_reranks_and_names_itself_in_the_evidence() {
    // The natural-language query alone ranks `occ.allocate` first and does
    // not reach `occ.reserve` at all.
    let query_batch = lane_batch(vec![
        pair(
            "occ.allocate",
            &[(LexicalFieldV1::BodyText, 400_000)],
            &["inventory"],
        ),
        pair(
            "occ.other",
            &[(LexicalFieldV1::BodyText, 100_000)],
            &["inventory"],
        ),
    ]);
    let anchor_batch = lane_batch(vec![
        pair(
            "occ.reserve",
            &[(LexicalFieldV1::SymbolName, 900_000)],
            &["reserve_stock"],
        ),
        pair(
            "occ.allocate",
            &[(LexicalFieldV1::BodyText, 50_000)],
            &["reserve_stock"],
        ),
    ]);
    let (outcome, receipt) = merge_lexical_routes(
        &generation(),
        &budget(8),
        &budget(8),
        vec![
            route(LexicalRouteKindV1::Query, query_batch),
            route(anchor_kind("reserve_stock"), anchor_batch),
        ],
    )
    .expect("merge");
    let RetrieverOutcome::Complete(batch) = outcome else {
        panic!("every route completed, so the merged lane is complete");
    };
    assert_eq!(order(&batch), ["occ.reserve", "occ.allocate", "occ.other"]);
    assert_eq!(
        batch.candidates[1].raw_score,
        FixedPointScore(450_000),
        "a candidate ranked by two routes carries the checked sum of both"
    );
    assert_eq!(
        batch
            .candidates
            .iter()
            .map(|candidate| candidate.ordinal_rank)
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
    let allocate_evidence =
        &batch.evidence_by_occurrence[&id::<tracedecay_domain::SourceOccurrenceId>("occ.allocate")];
    assert_eq!(
        allocate_evidence.field_scores_micros,
        [(LexicalFieldV1::BodyText, 450_000)]
    );
    assert_eq!(
        allocate_evidence.matched_whole_terms,
        ["inventory", "reserve_stock"]
    );
    assert!(receipt.has_additional_routes());
    let reserve_matches = &receipt.matches_by_anchor
        [&id::<tracedecay_domain::RetrievalAnchorId>("anchor.occ.reserve")];
    assert_eq!(reserve_matches.len(), 1);
    assert_eq!(reserve_matches[0].route, anchor_kind("reserve_stock"));
    assert_eq!(reserve_matches[0].score_micros, 900_000);
    assert_eq!(reserve_matches[0].matched_terms, ["reserve_stock"]);
    let allocate_matches = &receipt.matches_by_anchor
        [&id::<tracedecay_domain::RetrievalAnchorId>("anchor.occ.allocate")];
    assert_eq!(
        allocate_matches
            .iter()
            .map(|m| m.route.clone())
            .collect::<Vec<_>>(),
        [LexicalRouteKindV1::Query, anchor_kind("reserve_stock")]
    );
    let continuation = batch
        .continuation
        .expect("merged lane commits a checkpoint");
    assert_eq!(continuation.lane, RetrieverKind::Lexical);
    assert!(continuation.exhausted);
    assert_eq!(batch.coverage.eligible, 3);
    assert_eq!(batch.coverage.examined, 4);
}

#[test]
fn merged_prefix_is_independent_of_route_order_and_honors_the_lane_cap() {
    let query_batch = lane_batch(vec![
        pair("occ.a", &[(LexicalFieldV1::BodyText, 300_000)], &["x"]),
        pair("occ.b", &[(LexicalFieldV1::BodyText, 200_000)], &["x"]),
    ]);
    let y_batch = lane_batch(vec![pair(
        "occ.c",
        &[(LexicalFieldV1::SymbolName, 250_000)],
        &["y"],
    )]);
    let z_batch = lane_batch(vec![pair(
        "occ.d",
        &[(LexicalFieldV1::SymbolName, 100_000)],
        &["z"],
    )]);
    let (capped, _) = merge_lexical_routes(
        &generation(),
        &budget(3),
        &budget(8),
        vec![
            route(LexicalRouteKindV1::Query, query_batch.clone()),
            route(anchor_kind("y"), y_batch.clone()),
            route(anchor_kind("z"), z_batch.clone()),
        ],
    )
    .expect("merge");
    let RetrieverOutcome::Complete(capped) = capped else {
        panic!("complete");
    };
    assert_eq!(order(&capped), ["occ.a", "occ.c", "occ.b"]);
    assert_eq!(capped.coverage.capped, 1);
    assert_eq!(capped.coverage.eligible, 4);
    assert!(
        !capped.continuation.as_ref().expect("checkpoint").exhausted,
        "a truncated merge is not exhausted"
    );

    let (reordered, _) = merge_lexical_routes(
        &generation(),
        &budget(3),
        &budget(8),
        vec![
            route(LexicalRouteKindV1::Query, query_batch),
            route(anchor_kind("z"), z_batch),
            route(anchor_kind("y"), y_batch),
        ],
    )
    .expect("merge");
    let RetrieverOutcome::Complete(reordered) = reordered else {
        panic!("complete");
    };
    assert_eq!(
        reordered, capped,
        "additive route order cannot select a different committed prefix"
    );
}

#[test]
fn a_failed_additive_route_degrades_to_partial_without_hiding_the_query_route() {
    let query_batch = lane_batch(vec![pair(
        "occ.a",
        &[(LexicalFieldV1::BodyText, 300_000)],
        &["x"],
    )]);
    let (outcome, receipt) = merge_lexical_routes(
        &generation(),
        &budget(8),
        &budget(8),
        vec![
            route(LexicalRouteKindV1::Query, query_batch),
            LexicalRouteOutcomeV1 {
                kind: anchor_kind("missing"),
                outcome: RetrieverOutcome::Unavailable(RetrievalFailure::AuthorityUnavailable {
                    detail: "postings offline".to_owned(),
                }),
            },
        ],
    )
    .expect("merge");
    let RetrieverOutcome::Partial { value, reason } = outcome else {
        panic!("a failed additive route must not be reported as complete recall");
    };
    assert_eq!(order(&value), ["occ.a"]);
    assert_eq!(
        reason,
        RetrievalFailure::AuthorityUnavailable {
            detail: "postings offline".to_owned(),
        }
    );
    assert!(
        !value.continuation.expect("checkpoint").exhausted,
        "recall through the failed route is unknown"
    );
    assert_eq!(receipt.routes.len(), 2);
}

#[test]
fn an_unserved_query_route_is_returned_unchanged() {
    let stale = RetrieverOutcome::Stale(freshness());
    let (outcome, receipt) = merge_lexical_routes(
        &generation(),
        &budget(8),
        &budget(8),
        vec![
            LexicalRouteOutcomeV1 {
                kind: LexicalRouteKindV1::Query,
                outcome: stale.clone(),
            },
            route(
                anchor_kind("reserve_stock"),
                lane_batch(vec![pair(
                    "occ.reserve",
                    &[(LexicalFieldV1::SymbolName, 900_000)],
                    &["reserve_stock"],
                )]),
            ),
        ],
    )
    .expect("merge");
    assert_eq!(outcome, stale);
    assert!(receipt.matches_by_anchor.is_empty());
}

#[test]
fn routes_that_disagree_on_occurrence_identity_are_a_contract_violation() {
    let query_batch = lane_batch(vec![pair(
        "occ.a",
        &[(LexicalFieldV1::BodyText, 300_000)],
        &["x"],
    )]);
    let (mut candidate, evidence) = pair("occ.a", &[(LexicalFieldV1::SymbolName, 1)], &["y"]);
    candidate.anchor_id = id("anchor.somewhere-else");
    let mut forged = lane_batch(Vec::new());
    forged.candidates.push(candidate);
    forged.evidence_by_occurrence.insert(id("occ.a"), evidence);
    let error = merge_lexical_routes(
        &generation(),
        &budget(8),
        &budget(8),
        vec![
            route(LexicalRouteKindV1::Query, query_batch),
            route(anchor_kind("y"), forged),
        ],
    )
    .expect_err("identity drift between routes must fail closed");
    assert!(
        matches!(error, RetrievalPortError::Contract(_)),
        "{error:?}"
    );

    let error = merge_lexical_routes(&generation(), &budget(8), &budget(8), Vec::new())
        .expect_err("the query route is required");
    assert!(
        matches!(error, RetrievalPortError::Contract(_)),
        "{error:?}"
    );
}
