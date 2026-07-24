use std::collections::{BTreeMap, BTreeSet};

use tracedecay_domain::{
    CopyProofV1, LogicalCopyRecordV1, MessageOccurrenceIdV1, ObservationId, RetrievalAnchorId,
    SessionAuthorityClassV1, SessionId, SessionSummaryIdV1, SessionSummaryRecordV1,
    SummarySourceHorizonV1, TemporalAssertionKindV1, TemporalAssertionRecordV1, TemporalModeV1,
    TemporalValidityV1, UtcMicros,
};

use super::super::ports::{ExecutionControl, TemporalPortError};
use super::resolver::{
    resolve_temporal, resolve_temporal_controlled, resolve_temporal_with_checkpoints,
};
use super::summary::{
    SummaryLineageRejection, SummaryOmission, SummarySourceState,
    evaluate_summary_lineage_eligibility, evaluate_summary_lineage_eligibility_controlled,
};
use super::types::{
    ResolutionAssertion, ResolutionCertainty, ResolutionCheckpoint, ResolutionEvidence,
    ResolutionInputError, ResolutionLineageEdgeKind, ResolutionOccurrence, ValidatedAuthorization,
};

fn occurrence_id(byte: char) -> MessageOccurrenceIdV1 {
    MessageOccurrenceIdV1::new(format!("sha256:{}", byte.to_string().repeat(64)))
        .expect("valid occurrence id")
}

fn anchor(value: &str) -> RetrievalAnchorId {
    serde_json::from_str(&format!("\"{value}\"")).expect("valid anchor")
}

fn occurrence(
    id: char,
    anchor_id: &str,
    knowledge_at: i64,
    valid_time: TemporalValidityV1,
) -> ResolutionOccurrence {
    ResolutionOccurrence {
        occurrence_id: occurrence_id(id),
        anchor_id: anchor(anchor_id),
        knowledge_at: UtcMicros(knowledge_at),
        valid_time,
        evidence: ResolutionEvidence::new(
            SessionAuthorityClassV1::CanonicalObservation,
            ValidatedAuthorization::Authorized,
        ),
    }
}

fn assertion(
    kind: TemporalAssertionKindV1,
    subject: &str,
    object: &str,
    knowledge_at: i64,
) -> ResolutionAssertion {
    ResolutionAssertion {
        kind,
        subject_anchor_id: anchor(subject),
        object_anchor_id: anchor(object),
        knowledge_at: UtcMicros(knowledge_at),
        valid_time: TemporalValidityV1::Known {
            valid_at: UtcMicros(knowledge_at),
        },
        evidence: ResolutionEvidence::new(
            SessionAuthorityClassV1::CanonicalObservation,
            ValidatedAuthorization::Authorized,
        ),
    }
}

fn summary(
    id: &str,
    anchor_id: &str,
    source_anchor: &str,
    knowledge_through: i64,
    valid_through: i64,
) -> SessionSummaryRecordV1 {
    summary_with_sources(
        id,
        anchor_id,
        &[source_anchor],
        knowledge_through,
        valid_through,
    )
}

fn summary_with_sources(
    id: &str,
    anchor_id: &str,
    source_anchors: &[&str],
    knowledge_through: i64,
    valid_through: i64,
) -> SessionSummaryRecordV1 {
    let session_id: SessionId = serde_json::from_str("\"session-1\"").expect("valid session id");
    SessionSummaryRecordV1::new(
        SessionSummaryIdV1::new(id).expect("valid summary id"),
        session_id,
        anchor(anchor_id),
        source_anchors
            .iter()
            .map(|source_anchor| anchor(source_anchor))
            .collect(),
        SummarySourceHorizonV1 {
            knowledge_through: UtcMicros(knowledge_through),
            valid_through: Some(UtcMicros(valid_through)),
        },
        UtcMicros(knowledge_through),
    )
    .expect("valid summary")
}

fn covered_source(knowledge_at: i64, valid_at: i64) -> SummarySourceState {
    SummarySourceState::Covered {
        knowledge_at: UtcMicros(knowledge_at),
        valid_time: TemporalValidityV1::Known {
            valid_at: UtcMicros(valid_at),
        },
    }
}

#[test]
fn only_explicit_copy_evidence_collapses_repetitions() {
    let first = occurrence(
        'a',
        "a",
        1,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1),
        },
    );
    let copied = occurrence(
        'b',
        "b",
        2,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );
    let independent = occurrence(
        'c',
        "c",
        2,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );
    let provider_record_id: ObservationId =
        serde_json::from_str("\"provider-record\"").expect("valid observation id");
    let copy = LogicalCopyRecordV1 {
        occurrence_id: copied.occurrence_id.clone(),
        copied_from_occurrence_id: first.occurrence_id.clone(),
        proof: CopyProofV1::ProviderLinkage {
            source_occurrence_id: first.occurrence_id.clone(),
            provider_record_id,
        },
        knowledge_at: copied.knowledge_at,
        valid_time: copied.valid_time,
    };

    let resolved = resolve_temporal(
        &[first, copied, independent],
        &[copy],
        &[],
        TemporalModeV1::Current,
    )
    .expect("resolution succeeds");

    assert_eq!(resolved.len(), 2);
    assert!(
        resolved
            .iter()
            .any(|item| item.occurrence.anchor_id == anchor("a"))
    );
    assert!(
        resolved
            .iter()
            .any(|item| item.occurrence.anchor_id == anchor("c"))
    );
}

#[test]
fn as_of_requires_known_valid_and_knowledge_time() {
    let resolved = resolve_temporal(
        &[
            occurrence(
                'a',
                "known",
                5,
                TemporalValidityV1::Known {
                    valid_at: UtcMicros(4),
                },
            ),
            occurrence('b', "unknown", 3, TemporalValidityV1::Unknown),
            occurrence(
                'c',
                "late",
                7,
                TemporalValidityV1::Known {
                    valid_at: UtcMicros(3),
                },
            ),
        ],
        &[],
        &[],
        TemporalModeV1::AsOf {
            cutoff: UtcMicros(5),
        },
    )
    .expect("resolution succeeds");

    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].occurrence.anchor_id, anchor("known"));
}

#[test]
fn current_applies_corrections_and_exposes_conflicts() {
    let original = occurrence(
        'a',
        "original",
        1,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1),
        },
    );
    let correction = occurrence(
        'b',
        "correction",
        2,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );
    let rival = occurrence(
        'c',
        "rival",
        2,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );
    let assertions = [
        ResolutionAssertion {
            kind: TemporalAssertionKindV1::Corrects,
            subject_anchor_id: correction.anchor_id.clone(),
            object_anchor_id: original.anchor_id.clone(),
            knowledge_at: UtcMicros(2),
            valid_time: TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
            evidence: ResolutionEvidence::new(
                SessionAuthorityClassV1::CanonicalObservation,
                ValidatedAuthorization::Authorized,
            ),
        },
        ResolutionAssertion {
            kind: TemporalAssertionKindV1::Contradicts,
            subject_anchor_id: correction.anchor_id.clone(),
            object_anchor_id: rival.anchor_id.clone(),
            knowledge_at: UtcMicros(2),
            valid_time: TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
            evidence: ResolutionEvidence::new(
                SessionAuthorityClassV1::CanonicalObservation,
                ValidatedAuthorization::Authorized,
            ),
        },
    ];

    let resolved = resolve_temporal(
        &[original, correction, rival],
        &[],
        &assertions,
        TemporalModeV1::Current,
    )
    .expect("resolution succeeds");

    assert_eq!(resolved.len(), 2);
    assert!(resolved.iter().all(|item| item.conflicted));
    assert!(
        !resolved
            .iter()
            .any(|item| item.occurrence.anchor_id == anchor("original"))
    );
}

#[test]
fn forensic_retains_uncertain_copies_in_stable_order() {
    let first = occurrence('a', "a", 2, TemporalValidityV1::Unknown);
    let copied = occurrence('b', "b", 1, TemporalValidityV1::Unknown);
    let mut unauthorized = occurrence('c', "denied", 0, TemporalValidityV1::Unknown);
    unauthorized.evidence = ResolutionEvidence::new(
        SessionAuthorityClassV1::CanonicalObservation,
        ValidatedAuthorization::Unauthorized,
    );

    let resolved = resolve_temporal(
        &[first, copied, unauthorized],
        &[],
        &[],
        TemporalModeV1::Forensic,
    )
    .expect("resolution succeeds");

    assert_eq!(resolved.len(), 2);
    assert!(resolved.iter().all(|item| item.uncertain));
    assert_eq!(resolved[0].occurrence.anchor_id, anchor("b"));
    assert_eq!(resolved[1].occurrence.anchor_id, anchor("a"));
}

#[test]
fn current_does_not_let_unsupported_correction_erase_supported_evidence() {
    let mut original = occurrence(
        'a',
        "original",
        1,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1),
        },
    );
    original.evidence.authority = SessionAuthorityClassV1::ProviderNative;
    let mut correction = occurrence(
        'b',
        "correction",
        2,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );
    correction.evidence.authority = SessionAuthorityClassV1::DerivedProjection;
    let witness = occurrence(
        'c',
        "witness",
        3,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(3),
        },
    );
    let mut assertions = [
        assertion(TemporalAssertionKindV1::Supports, "witness", "original", 3),
        assertion(
            TemporalAssertionKindV1::Corrects,
            "correction",
            "original",
            2,
        ),
    ];
    assertions[1].evidence.authority = SessionAuthorityClassV1::DerivedProjection;

    let resolved = resolve_temporal(
        &[original, correction, witness],
        &[],
        &assertions,
        TemporalModeV1::Current,
    )
    .expect("resolution succeeds");

    assert!(
        resolved
            .iter()
            .any(|item| item.occurrence.anchor_id == anchor("original"))
    );
    assert!(
        resolved
            .iter()
            .find(|item| item.occurrence.anchor_id == anchor("original"))
            .is_some_and(|item| item.supporting_anchor_ids.contains(&anchor("witness")))
    );
}

#[test]
fn current_conflict_precedence_retains_the_authoritative_side() {
    let mut authoritative = occurrence(
        'a',
        "authoritative",
        1,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1),
        },
    );
    authoritative.evidence.authority = SessionAuthorityClassV1::ProviderNative;
    let mut weak = occurrence(
        'b',
        "weak",
        2,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );
    weak.evidence.authority = SessionAuthorityClassV1::DerivedProjection;
    let mut contradiction = assertion(
        TemporalAssertionKindV1::Contradicts,
        "authoritative",
        "weak",
        3,
    );
    contradiction.evidence.authority = SessionAuthorityClassV1::ProviderNative;

    let resolved = resolve_temporal(
        &[authoritative, weak],
        &[],
        &[contradiction],
        TemporalModeV1::Current,
    )
    .expect("resolution succeeds");

    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].occurrence.anchor_id, anchor("authoritative"));
    assert!(!resolved[0].conflicted);
}

#[test]
fn evolution_orders_the_correction_chain_not_incidental_timestamps() {
    let original = occurrence(
        'a',
        "original",
        30,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1),
        },
    );
    let correction = occurrence(
        'b',
        "correction",
        20,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );
    let superseding = occurrence(
        'c',
        "superseding",
        10,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(3),
        },
    );
    let assertions = [
        assertion(
            TemporalAssertionKindV1::Corrects,
            "correction",
            "original",
            31,
        ),
        assertion(
            TemporalAssertionKindV1::Supersedes,
            "superseding",
            "correction",
            32,
        ),
    ];

    let resolved = resolve_temporal(
        &[original, correction, superseding],
        &[],
        &assertions,
        TemporalModeV1::Evolution,
    )
    .expect("resolution succeeds");

    assert_eq!(
        resolved
            .iter()
            .map(|item| item.occurrence.anchor_id.clone())
            .collect::<Vec<_>>(),
        vec![
            anchor("original"),
            anchor("correction"),
            anchor("superseding")
        ]
    );
}

#[test]
fn resolution_checks_live_work_budget_during_occurrence_consumption() {
    let occurrences = [
        occurrence('a', "a", 1, TemporalValidityV1::Unknown),
        occurrence('b', "b", 2, TemporalValidityV1::Unknown),
    ];
    let control = ExecutionControl::default().with_work_limit(1);

    assert_eq!(
        resolve_temporal_controlled(&occurrences, &[], &[], TemporalModeV1::Forensic, &control,),
        Err(TemporalPortError::BudgetExceeded {
            resource: "work units"
        })
    );
}

#[test]
fn weak_correction_cannot_erase_authoritative_current_evidence() {
    let mut original = occurrence(
        'a',
        "original",
        1,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1),
        },
    );
    original.evidence.authority = SessionAuthorityClassV1::ProviderNative;
    let mut correction = occurrence(
        'b',
        "correction",
        2,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );
    correction.evidence.authority = SessionAuthorityClassV1::DerivedProjection;
    let mut correction_edge = assertion(
        TemporalAssertionKindV1::Corrects,
        "correction",
        "original",
        2,
    );
    correction_edge.evidence.authority = SessionAuthorityClassV1::DerivedProjection;

    let resolved = resolve_temporal(
        &[original, correction],
        &[],
        &[correction_edge],
        TemporalModeV1::Current,
    )
    .expect("resolution succeeds");

    assert_eq!(resolved.occurrences.len(), 2);
    assert!(resolved.occurrences.iter().all(|item| item.conflicted));
}

#[test]
fn strong_correction_suppresses_weaker_current_evidence() {
    let original = occurrence(
        'a',
        "original",
        1,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1),
        },
    );
    let mut correction = occurrence(
        'b',
        "correction",
        2,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );
    correction.evidence.authority = SessionAuthorityClassV1::ProviderNative;
    let mut correction_edge = assertion(
        TemporalAssertionKindV1::Corrects,
        "correction",
        "original",
        2,
    );
    correction_edge.evidence.authority = SessionAuthorityClassV1::ProviderNative;

    let resolved = resolve_temporal(
        &[original, correction],
        &[],
        &[correction_edge],
        TemporalModeV1::Current,
    )
    .expect("resolution succeeds");

    assert_eq!(resolved.occurrences.len(), 1);
    assert_eq!(
        resolved.occurrences[0].occurrence.anchor_id,
        anchor("correction")
    );
}

#[test]
fn unresolved_conflict_preserves_both_sides_and_a_typed_edge() {
    let left = occurrence(
        'a',
        "left",
        1,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1),
        },
    );
    let right = occurrence(
        'b',
        "right",
        2,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );

    let resolved = resolve_temporal(
        &[left, right],
        &[],
        &[assertion(
            TemporalAssertionKindV1::Contradicts,
            "right",
            "left",
            3,
        )],
        TemporalModeV1::Current,
    )
    .expect("resolution succeeds");

    assert_eq!(resolved.occurrences.len(), 2);
    assert!(resolved.occurrences.iter().all(|item| item.conflicted));
    assert_eq!(resolved.lineage_edges.len(), 1);
    assert_eq!(
        resolved.lineage_edges[0].kind,
        ResolutionLineageEdgeKind::Contradiction
    );
}

#[test]
fn evolution_returns_ordered_occurrences_and_typed_lineage_chain() {
    let original = occurrence(
        'a',
        "original",
        30,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1),
        },
    );
    let correction = occurrence(
        'b',
        "correction",
        20,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );
    let successor = occurrence(
        'c',
        "successor",
        10,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(3),
        },
    );

    let resolved = resolve_temporal(
        &[original, correction, successor],
        &[],
        &[
            assertion(
                TemporalAssertionKindV1::Corrects,
                "correction",
                "original",
                31,
            ),
            assertion(
                TemporalAssertionKindV1::Supersedes,
                "successor",
                "correction",
                32,
            ),
        ],
        TemporalModeV1::Evolution,
    )
    .expect("resolution succeeds");

    assert_eq!(
        resolved
            .occurrences
            .iter()
            .map(|item| item.occurrence.anchor_id.clone())
            .collect::<Vec<_>>(),
        vec![
            anchor("original"),
            anchor("correction"),
            anchor("successor")
        ]
    );
    assert_eq!(
        resolved
            .lineage_edges
            .iter()
            .map(|edge| edge.kind)
            .collect::<Vec<_>>(),
        vec![
            ResolutionLineageEdgeKind::Correction,
            ResolutionLineageEdgeKind::Supersession,
        ]
    );
}

#[test]
fn forensic_preserves_authorized_uncertainty_as_a_typed_state() {
    let unknown = occurrence('a', "unknown", 1, TemporalValidityV1::Unknown);
    let mut unauthorized = occurrence('b', "unauthorized", 2, TemporalValidityV1::Unknown);
    unauthorized.evidence = ResolutionEvidence::new(
        SessionAuthorityClassV1::CanonicalObservation,
        ValidatedAuthorization::Unauthorized,
    );

    let resolved = resolve_temporal(&[unknown, unauthorized], &[], &[], TemporalModeV1::Forensic)
        .expect("resolution succeeds");

    assert_eq!(resolved.occurrences.len(), 1);
    assert_eq!(
        resolved.occurrences[0].certainty(),
        ResolutionCertainty::AuthorizedUnknown
    );
}

#[test]
fn cancellation_and_hook_budget_errors_propagate() {
    let input = [occurrence('a', "a", 1, TemporalValidityV1::Unknown)];
    let cancelled = ExecutionControl::default();
    cancelled.cancel();
    assert_eq!(
        resolve_temporal_controlled(&input, &[], &[], TemporalModeV1::Forensic, &cancelled,),
        Err(TemporalPortError::Cancelled)
    );

    let mut hook = |_checkpoint: ResolutionCheckpoint| {
        Err(TemporalPortError::BudgetExceeded {
            resource: "lineage traversal",
        })
    };
    assert_eq!(
        resolve_temporal_with_checkpoints(
            &input,
            &[],
            &[],
            TemporalModeV1::Forensic,
            &ExecutionControl::default(),
            &mut hook,
        ),
        Err(TemporalPortError::BudgetExceeded {
            resource: "lineage traversal",
        })
    );
}

#[test]
fn unauthorized_assertion_conversion_fails_without_copying_lineage_metadata() {
    let record: TemporalAssertionRecordV1 = serde_json::from_str(
        r#"{
            "assertion_id":"assertion.explicit",
            "kind":"supports",
            "subject_anchor_id":"subject",
            "object_anchor_id":"object",
            "knowledge_at":10,
            "valid_time":{"kind":"known","valid_at":10},
            "evidence":{
                "authority":"explicit_anchor_assertion",
                "evidence_class":"provider_declared",
                "source_anchor_id":"private-lineage",
                "sanitization_receipt":{
                    "receipt_id":"receipt.explicit",
                    "sanitizer_version":"sanitizer.explicit"
                }
            }
        }"#,
    )
    .expect("valid assertion fixture");

    assert_eq!(
        ResolutionAssertion::from_record(&record, ValidatedAuthorization::Unauthorized),
        Err(ResolutionInputError::UnauthorizedAssertion)
    );
}

#[test]
fn summary_source_and_predecessor_traversal_preserve_control_errors() {
    let session_id: SessionId = serde_json::from_str("\"session-1\"").expect("valid session id");
    let predecessor = summary("predecessor", "old", "source-old", 5, 5);
    let successor = summary("successor", "new", "source-new", 6, 6)
        .with_predecessor(predecessor.summary_id().clone())
        .expect("valid predecessor");
    let states = [
        (anchor("source-old"), covered_source(5, 5)),
        (anchor("source-new"), covered_source(6, 6)),
    ]
    .into_iter()
    .collect();

    let cancelled = ExecutionControl::default();
    cancelled.cancel();
    assert_eq!(
        evaluate_summary_lineage_eligibility_controlled(
            &[predecessor.clone(), successor.clone()],
            &states,
            &session_id,
            TemporalModeV1::Current,
            &cancelled,
        ),
        Err(TemporalPortError::Cancelled)
    );

    let bounded = ExecutionControl::default().with_work_limit(6);
    assert_eq!(
        evaluate_summary_lineage_eligibility_controlled(
            &[predecessor, successor],
            &states,
            &session_id,
            TemporalModeV1::Current,
            &bounded,
        ),
        Err(TemporalPortError::BudgetExceeded {
            resource: "work units"
        })
    );
}

#[test]
fn unrelated_newer_occurrence_does_not_stale_summary() {
    let session_id: SessionId = serde_json::from_str("\"session-1\"").expect("valid session id");
    let summaries = [summary("summary-a", "summary-a", "source-a", 7, 6)];
    let source_states = [
        (anchor("source-a"), covered_source(7, 6)),
        (anchor("unrelated"), covered_source(99, 99)),
    ]
    .into_iter()
    .collect();

    let eligibility = evaluate_summary_lineage_eligibility(
        &summaries,
        &source_states,
        &session_id,
        TemporalModeV1::Current,
    )
    .expect("eligibility");

    assert_eq!(
        eligibility.eligible_anchor_ids,
        [anchor("summary-a")].into_iter().collect()
    );
    assert!(eligibility.rejections.is_empty());
}

#[test]
fn invalid_successor_does_not_suppress_eligible_predecessor() {
    let session_id: SessionId = serde_json::from_str("\"session-1\"").expect("valid session id");
    let predecessor = summary("predecessor", "summary-old", "source-old", 5, 5);
    let successor = summary("successor", "summary-new", "source-new", 7, 7)
        .with_predecessor(predecessor.summary_id().clone())
        .expect("valid predecessor");
    let source_states = [
        (anchor("source-old"), covered_source(5, 5)),
        (anchor("source-new"), SummarySourceState::Stale),
    ]
    .into_iter()
    .collect();

    let eligibility = evaluate_summary_lineage_eligibility(
        &[predecessor, successor],
        &source_states,
        &session_id,
        TemporalModeV1::Current,
    )
    .expect("eligibility");

    assert_eq!(
        eligibility.eligible_anchor_ids,
        [anchor("summary-old")].into_iter().collect()
    );
    assert!(eligibility.suppressed_summary_ids.is_empty());
    assert!(matches!(
        eligibility
            .rejections
            .get(&SessionSummaryIdV1::new("successor").expect("valid id")),
        Some(SummaryLineageRejection::StaleSource { .. })
    ));
}

#[test]
fn summary_lineage_cycles_are_ineligible() {
    let session_id: SessionId = serde_json::from_str("\"session-1\"").expect("valid session id");
    let first = summary("first", "summary-first", "source-first", 5, 5)
        .with_predecessor(SessionSummaryIdV1::new("second").expect("valid id"))
        .expect("non-self predecessor");
    let second = summary("second", "summary-second", "source-second", 6, 6)
        .with_predecessor(SessionSummaryIdV1::new("first").expect("valid id"))
        .expect("non-self predecessor");
    let source_states = [
        (anchor("source-first"), covered_source(5, 5)),
        (anchor("source-second"), covered_source(6, 6)),
    ]
    .into_iter()
    .collect();

    let eligibility = evaluate_summary_lineage_eligibility(
        &[first, second],
        &source_states,
        &session_id,
        TemporalModeV1::Current,
    )
    .expect("eligibility");

    assert!(eligibility.eligible_anchor_ids.is_empty());
    assert_eq!(
        eligibility
            .rejections
            .values()
            .filter(|reason| matches!(reason, SummaryLineageRejection::Cycle))
            .count(),
        2
    );
}

#[test]
fn source_specific_horizon_rejects_only_the_out_of_coverage_summary() {
    let session_id: SessionId = serde_json::from_str("\"session-1\"").expect("valid session id");
    let covered = summary("covered", "summary-covered", "covered-source", 7, 7);
    let stale_horizon = summary("stale-horizon", "summary-stale", "advanced-source", 7, 7);
    let source_states = [
        (anchor("covered-source"), covered_source(7, 7)),
        (anchor("advanced-source"), covered_source(8, 7)),
    ]
    .into_iter()
    .collect();

    let eligibility = evaluate_summary_lineage_eligibility(
        &[covered, stale_horizon],
        &source_states,
        &session_id,
        TemporalModeV1::Current,
    )
    .expect("eligibility");

    assert_eq!(
        eligibility.eligible_anchor_ids,
        [anchor("summary-covered")].into_iter().collect()
    );
    assert!(matches!(
        eligibility
            .rejections
            .get(&SessionSummaryIdV1::new("stale-horizon").expect("valid id")),
        Some(SummaryLineageRejection::SourceBeyondKnowledgeHorizon { .. })
    ));
}

#[test]
fn all_summary_source_states_have_distinct_eligibility_or_rejections() {
    let session_id: SessionId = serde_json::from_str("\"session-1\"").expect("valid session id");
    let summaries = [
        summary("covered", "summary-covered", "covered-source", 7, 7),
        summary("stale", "summary-stale", "stale-source", 7, 7),
        summary("deleted", "summary-deleted", "deleted-source", 7, 7),
        summary("redacted", "summary-redacted", "redacted-source", 7, 7),
        summary("missing", "summary-missing", "missing-source", 7, 7),
        summary(
            "unauthorized",
            "summary-unauthorized",
            "unauthorized-source",
            7,
            7,
        ),
        summary("locked", "summary-locked", "locked-source", 7, 7),
        summary("expired", "summary-expired", "expired-source", 7, 7),
        summary(
            "unavailable",
            "summary-unavailable",
            "unavailable-source",
            7,
            7,
        ),
        summary("cycle-source", "summary-cycle", "cycle-source", 7, 7),
    ];
    let source_states = [
        (anchor("covered-source"), covered_source(7, 7)),
        (anchor("stale-source"), SummarySourceState::Stale),
        (anchor("deleted-source"), SummarySourceState::Deleted),
        (anchor("redacted-source"), SummarySourceState::Redacted),
        (anchor("missing-source"), SummarySourceState::Missing),
        (
            anchor("unauthorized-source"),
            SummarySourceState::Unauthorized,
        ),
        (anchor("locked-source"), SummarySourceState::Locked),
        (anchor("expired-source"), SummarySourceState::Expired),
        (
            anchor("unavailable-source"),
            SummarySourceState::Unavailable,
        ),
        (anchor("cycle-source"), SummarySourceState::Cycle),
    ]
    .into_iter()
    .collect();

    let eligibility = evaluate_summary_lineage_eligibility(
        &summaries,
        &source_states,
        &session_id,
        TemporalModeV1::Current,
    )
    .expect("eligibility");

    assert_eq!(
        eligibility.eligible_anchor_ids,
        [anchor("summary-covered")].into_iter().collect()
    );
    assert_eq!(eligibility.omissions.len(), 9);
    assert!(matches!(
        eligibility
            .rejections
            .get(&SessionSummaryIdV1::new("stale").expect("valid id")),
        Some(SummaryLineageRejection::StaleSource { .. })
    ));
    assert!(matches!(
        eligibility
            .rejections
            .get(&SessionSummaryIdV1::new("deleted").expect("valid id")),
        Some(SummaryLineageRejection::DeletedSource { .. })
    ));
    assert!(matches!(
        eligibility
            .rejections
            .get(&SessionSummaryIdV1::new("redacted").expect("valid id")),
        Some(SummaryLineageRejection::RedactedSource { .. })
    ));
    assert!(matches!(
        eligibility
            .rejections
            .get(&SessionSummaryIdV1::new("missing").expect("valid id")),
        Some(SummaryLineageRejection::MissingSource { .. })
    ));
    assert!(matches!(
        eligibility
            .rejections
            .get(&SessionSummaryIdV1::new("unauthorized").expect("valid id")),
        Some(SummaryLineageRejection::UnauthorizedSource { .. })
    ));
    assert!(matches!(
        eligibility
            .rejections
            .get(&SessionSummaryIdV1::new("locked").expect("valid id")),
        Some(SummaryLineageRejection::LockedSource { .. })
    ));
    assert!(matches!(
        eligibility
            .rejections
            .get(&SessionSummaryIdV1::new("expired").expect("valid id")),
        Some(SummaryLineageRejection::ExpiredSource { .. })
    ));
    assert!(matches!(
        eligibility
            .rejections
            .get(&SessionSummaryIdV1::new("unavailable").expect("valid id")),
        Some(SummaryLineageRejection::UnavailableSource { .. })
    ));
    assert!(matches!(
        eligibility
            .rejections
            .get(&SessionSummaryIdV1::new("cycle-source").expect("valid id")),
        Some(SummaryLineageRejection::CycleSource { .. })
    ));
}

#[test]
fn unauthorized_and_session_mismatch_remain_lossless_and_distinct() {
    let summary = summary(
        "privacy-state",
        "summary-privacy-state",
        "source-privacy-state",
        7,
        7,
    );
    let source_states = [(
        anchor("source-privacy-state"),
        SummarySourceState::Unauthorized,
    )]
    .into_iter()
    .collect();
    let authorized_session: SessionId =
        serde_json::from_str("\"session-1\"").expect("valid session id");
    let mismatched_session: SessionId =
        serde_json::from_str("\"session-2\"").expect("valid session id");

    let unauthorized = evaluate_summary_lineage_eligibility(
        std::slice::from_ref(&summary),
        &source_states,
        &authorized_session,
        TemporalModeV1::Current,
    )
    .expect("unauthorized eligibility");
    let mismatched = evaluate_summary_lineage_eligibility(
        std::slice::from_ref(&summary),
        &source_states,
        &mismatched_session,
        TemporalModeV1::Current,
    )
    .expect("mismatched eligibility");

    assert_eq!(
        unauthorized.omissions,
        vec![SummaryOmission {
            summary_id: summary.summary_id().clone(),
            anchor_id: summary.summary_anchor_id().clone(),
            rejection: SummaryLineageRejection::UnauthorizedSource {
                anchor_id: anchor("source-privacy-state"),
            },
        }]
    );
    assert_eq!(
        mismatched.omissions,
        vec![SummaryOmission {
            summary_id: summary.summary_id().clone(),
            anchor_id: summary.summary_anchor_id().clone(),
            rejection: SummaryLineageRejection::SessionMismatch,
        }]
    );
}

#[test]
fn unauthorized_source_dominates_all_source_order_permutations() {
    let source_states = [
        (anchor("missing"), SummarySourceState::Missing),
        (anchor("redacted"), SummarySourceState::Redacted),
        (anchor("locked"), SummarySourceState::Locked),
        (anchor("expired"), SummarySourceState::Expired),
        (anchor("deleted"), SummarySourceState::Deleted),
        (anchor("unavailable"), SummarySourceState::Unavailable),
        (anchor("stale"), SummarySourceState::Stale),
        (anchor("unauthorized"), SummarySourceState::Unauthorized),
    ]
    .into_iter()
    .collect();
    let session_id = SessionId::new("session-1").expect("valid session id");
    let forward = [
        "missing",
        "redacted",
        "locked",
        "expired",
        "deleted",
        "unavailable",
        "stale",
        "unauthorized",
    ];
    let reverse = [
        "unauthorized",
        "stale",
        "unavailable",
        "deleted",
        "expired",
        "locked",
        "redacted",
        "missing",
    ];

    for source_anchors in [forward.as_slice(), reverse.as_slice()] {
        let summary = summary_with_sources("mixed", "summary-mixed", source_anchors, 7, 7);
        let eligibility = evaluate_summary_lineage_eligibility(
            std::slice::from_ref(&summary),
            &source_states,
            &session_id,
            TemporalModeV1::Current,
        )
        .expect("mixed-source eligibility");

        assert_eq!(
            eligibility
                .rejections
                .get(&SessionSummaryIdV1::new("mixed").expect("valid id")),
            Some(&SummaryLineageRejection::UnauthorizedSource {
                anchor_id: anchor("unauthorized"),
            })
        );
    }
}

#[test]
fn non_hidden_source_precedence_is_deterministic() {
    let cases = [
        (
            SummarySourceState::Redacted,
            SummarySourceState::Locked,
            SummaryLineageRejection::RedactedSource {
                anchor_id: anchor("left"),
            },
        ),
        (
            SummarySourceState::Locked,
            SummarySourceState::Expired,
            SummaryLineageRejection::ExpiredSource {
                anchor_id: anchor("right"),
            },
        ),
        (
            SummarySourceState::Expired,
            SummarySourceState::Deleted,
            SummaryLineageRejection::DeletedSource {
                anchor_id: anchor("right"),
            },
        ),
        (
            SummarySourceState::Deleted,
            SummarySourceState::Unavailable,
            SummaryLineageRejection::DeletedSource {
                anchor_id: anchor("left"),
            },
        ),
        (
            SummarySourceState::Unavailable,
            SummarySourceState::Stale,
            SummaryLineageRejection::UnavailableSource {
                anchor_id: anchor("left"),
            },
        ),
        (
            SummarySourceState::Stale,
            SummarySourceState::Missing,
            SummaryLineageRejection::MissingSource {
                anchor_id: anchor("right"),
            },
        ),
    ];
    let session_id = SessionId::new("session-1").expect("valid session id");

    for (left, right, expected) in cases {
        for source_anchors in [["left", "right"], ["right", "left"]] {
            let summary =
                summary_with_sources("precedence", "summary-precedence", &source_anchors, 7, 7);
            let source_states = [(anchor("left"), left), (anchor("right"), right)]
                .into_iter()
                .collect();
            let eligibility = evaluate_summary_lineage_eligibility(
                std::slice::from_ref(&summary),
                &source_states,
                &session_id,
                TemporalModeV1::Current,
            )
            .expect("precedence eligibility");

            assert_eq!(
                eligibility
                    .rejections
                    .get(&SessionSummaryIdV1::new("precedence").expect("valid id")),
                Some(&expected)
            );
        }
    }
}

#[test]
fn unauthorized_source_dominates_summary_horizon_failures() {
    let summary = summary(
        "horizon-private",
        "summary-horizon-private",
        "source-horizon-private",
        20,
        20,
    );
    let source_states = [(
        anchor("source-horizon-private"),
        SummarySourceState::Unauthorized,
    )]
    .into_iter()
    .collect();
    let session_id = SessionId::new("session-1").expect("valid session id");

    let eligibility = evaluate_summary_lineage_eligibility(
        std::slice::from_ref(&summary),
        &source_states,
        &session_id,
        TemporalModeV1::AsOf {
            cutoff: UtcMicros(10),
        },
    )
    .expect("private horizon eligibility");

    assert_eq!(
        eligibility
            .rejections
            .get(&SessionSummaryIdV1::new("horizon-private").expect("valid id")),
        Some(&SummaryLineageRejection::UnauthorizedSource {
            anchor_id: anchor("source-horizon-private"),
        })
    );
}

fn provider_copy(
    occurrence: &ResolutionOccurrence,
    source: &ResolutionOccurrence,
) -> LogicalCopyRecordV1 {
    let provider_record_id: ObservationId =
        serde_json::from_str("\"provider-record\"").expect("valid observation id");
    LogicalCopyRecordV1 {
        occurrence_id: occurrence.occurrence_id.clone(),
        copied_from_occurrence_id: source.occurrence_id.clone(),
        proof: CopyProofV1::ProviderLinkage {
            source_occurrence_id: source.occurrence_id.clone(),
            provider_record_id,
        },
        knowledge_at: occurrence.knowledge_at,
        valid_time: occurrence.valid_time,
    }
}

#[test]
fn forensic_preserves_explicit_logical_copy_occurrences() {
    let first = occurrence(
        'a',
        "a",
        1,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1),
        },
    );
    let copied = occurrence(
        'b',
        "b",
        2,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );
    let copy = provider_copy(&copied, &first);

    let resolved = resolve_temporal(&[first, copied], &[copy], &[], TemporalModeV1::Forensic)
        .expect("resolution succeeds");

    assert_eq!(resolved.len(), 2);
    assert!(
        resolved
            .iter()
            .any(|item| item.occurrence.anchor_id == anchor("a"))
    );
    assert!(
        resolved
            .iter()
            .any(|item| item.occurrence.anchor_id == anchor("b"))
    );
}

#[test]
fn as_of_requires_logical_copy_knowledge_and_valid_time() {
    for (knowledge_at, valid_time) in [
        (
            UtcMicros(6),
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        ),
        (
            UtcMicros(2),
            TemporalValidityV1::Known {
                valid_at: UtcMicros(6),
            },
        ),
        (UtcMicros(2), TemporalValidityV1::Unknown),
    ] {
        let original = occurrence(
            'a',
            "original",
            1,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(1),
            },
        );
        let copied = occurrence(
            'b',
            "copied",
            2,
            TemporalValidityV1::Known {
                valid_at: UtcMicros(2),
            },
        );
        let mut ineligible_copy = provider_copy(&copied, &original);
        ineligible_copy.knowledge_at = knowledge_at;
        ineligible_copy.valid_time = valid_time;

        let resolved = resolve_temporal(
            &[original, copied],
            &[ineligible_copy],
            &[],
            TemporalModeV1::AsOf {
                cutoff: UtcMicros(5),
            },
        )
        .expect("resolution succeeds");

        assert_eq!(resolved.len(), 2);
    }
}

#[test]
fn as_of_cutoff_is_inclusive_for_occurrences_and_assertions() {
    let boundary = occurrence(
        'a',
        "boundary",
        5,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(5),
        },
    );
    let late = occurrence(
        'b',
        "late",
        6,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(5),
        },
    );
    let witness = occurrence(
        'c',
        "witness",
        5,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(4),
        },
    );
    let support = assertion(TemporalAssertionKindV1::Supports, "witness", "boundary", 5);

    let resolved = resolve_temporal(
        &[boundary, late, witness],
        &[],
        &[support],
        TemporalModeV1::AsOf {
            cutoff: UtcMicros(5),
        },
    )
    .expect("resolution succeeds");

    assert!(
        resolved
            .iter()
            .any(|item| item.occurrence.anchor_id == anchor("boundary"))
    );
    assert!(
        !resolved
            .iter()
            .any(|item| item.occurrence.anchor_id == anchor("late"))
    );
    assert!(
        resolved
            .iter()
            .find(|item| item.occurrence.anchor_id == anchor("boundary"))
            .is_some_and(|item| item.supporting_anchor_ids.contains(&anchor("witness")))
    );
}

#[test]
fn as_of_ignores_assertions_beyond_knowledge_or_valid_cutoff() {
    let original = occurrence(
        'a',
        "original",
        1,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1),
        },
    );
    let correction = occurrence(
        'b',
        "correction",
        10,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(10),
        },
    );
    let late_edge = assertion(
        TemporalAssertionKindV1::Corrects,
        "correction",
        "original",
        10,
    );

    let resolved = resolve_temporal(
        &[original, correction],
        &[],
        &[late_edge],
        TemporalModeV1::AsOf {
            cutoff: UtcMicros(5),
        },
    )
    .expect("resolution succeeds");

    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].occurrence.anchor_id, anchor("original"));
    assert!(resolved.lineage_edges.is_empty());
}

#[test]
fn summary_as_of_enforces_created_and_source_horizon_cutoffs() {
    let session_id: SessionId = serde_json::from_str("\"session-1\"").expect("valid session id");
    let created_late = SessionSummaryRecordV1::new(
        SessionSummaryIdV1::new("created-late").expect("valid summary id"),
        session_id.clone(),
        anchor("summary-created"),
        vec![anchor("source-ok")],
        SummarySourceHorizonV1 {
            knowledge_through: UtcMicros(4),
            valid_through: Some(UtcMicros(4)),
        },
        UtcMicros(9),
    )
    .expect("valid summary");
    // Domain requires created_at >= knowledge_through, so a pure horizon breach uses
    // valid_through beyond cutoff while creation stays at/under the as-of bound.
    let horizon_late = SessionSummaryRecordV1::new(
        SessionSummaryIdV1::new("horizon-late").expect("valid summary id"),
        session_id.clone(),
        anchor("summary-horizon"),
        vec![anchor("source-ok")],
        SummarySourceHorizonV1 {
            knowledge_through: UtcMicros(4),
            valid_through: Some(UtcMicros(9)),
        },
        UtcMicros(4),
    )
    .expect("valid summary");
    let source_states = [(anchor("source-ok"), covered_source(4, 4))]
        .into_iter()
        .collect();

    let eligibility = evaluate_summary_lineage_eligibility(
        &[created_late, horizon_late],
        &source_states,
        &session_id,
        TemporalModeV1::AsOf {
            cutoff: UtcMicros(5),
        },
    )
    .expect("eligibility");

    assert!(eligibility.eligible_anchor_ids.is_empty());
    assert!(matches!(
        eligibility
            .rejections
            .get(&SessionSummaryIdV1::new("created-late").expect("valid id")),
        Some(SummaryLineageRejection::CreatedAfterCutoff)
    ));
    assert!(matches!(
        eligibility
            .rejections
            .get(&SessionSummaryIdV1::new("horizon-late").expect("valid id")),
        Some(SummaryLineageRejection::HorizonAfterCutoff)
    ));
}

#[test]
fn as_of_missing_valid_horizon_is_reported_as_missing() {
    let session_id: SessionId = serde_json::from_str("\"session-1\"").expect("valid session id");
    let missing_horizon = SessionSummaryRecordV1::new(
        SessionSummaryIdV1::new("missing-horizon").expect("valid summary id"),
        session_id.clone(),
        anchor("summary-missing-horizon"),
        vec![anchor("source-ok")],
        SummarySourceHorizonV1 {
            knowledge_through: UtcMicros(4),
            valid_through: None,
        },
        UtcMicros(4),
    )
    .expect("valid summary");
    let source_states = [(anchor("source-ok"), covered_source(4, 4))]
        .into_iter()
        .collect();

    let eligibility = evaluate_summary_lineage_eligibility(
        &[missing_horizon],
        &source_states,
        &session_id,
        TemporalModeV1::AsOf {
            cutoff: UtcMicros(5),
        },
    )
    .expect("eligibility");

    assert!(matches!(
        eligibility
            .rejections
            .get(&SessionSummaryIdV1::new("missing-horizon").expect("valid id")),
        Some(SummaryLineageRejection::MissingValidHorizon)
    ));
}

#[test]
fn non_as_of_modes_preserve_summary_with_unknown_valid_horizon() {
    let session_id: SessionId = serde_json::from_str("\"session-1\"").expect("valid session id");
    let missing_horizon = SessionSummaryRecordV1::new(
        SessionSummaryIdV1::new("missing-horizon").expect("valid summary id"),
        session_id.clone(),
        anchor("summary-missing-horizon"),
        vec![anchor("source-ok")],
        SummarySourceHorizonV1 {
            knowledge_through: UtcMicros(4),
            valid_through: None,
        },
        UtcMicros(4),
    )
    .expect("valid summary");
    let source_states = [(
        anchor("source-ok"),
        SummarySourceState::Covered {
            knowledge_at: UtcMicros(4),
            valid_time: TemporalValidityV1::Unknown,
        },
    )]
    .into_iter()
    .collect();

    for mode in [
        TemporalModeV1::Current,
        TemporalModeV1::Evolution,
        TemporalModeV1::Forensic,
    ] {
        let eligibility = evaluate_summary_lineage_eligibility(
            std::slice::from_ref(&missing_horizon),
            &source_states,
            &session_id,
            mode,
        )
        .expect("eligibility");
        assert_eq!(
            eligibility.eligible_anchor_ids,
            [anchor("summary-missing-horizon")].into_iter().collect(),
            "{mode:?} must preserve authorized unknown validity"
        );
    }
}

#[test]
fn current_suppresses_only_an_eligible_predecessor() {
    let session_id: SessionId = serde_json::from_str("\"session-1\"").expect("valid session id");
    let predecessor = summary("predecessor", "summary-old", "source-old", 5, 5);
    let successor = summary("successor", "summary-new", "source-new", 7, 7)
        .with_predecessor(predecessor.summary_id().clone())
        .expect("valid predecessor");
    let source_states = [
        (anchor("source-old"), covered_source(5, 5)),
        (anchor("source-new"), covered_source(7, 7)),
    ]
    .into_iter()
    .collect();

    let eligibility = evaluate_summary_lineage_eligibility(
        &[predecessor, successor],
        &source_states,
        &session_id,
        TemporalModeV1::Current,
    )
    .expect("eligibility");

    assert_eq!(
        eligibility.eligible_anchor_ids,
        [anchor("summary-new")].into_iter().collect()
    );
    assert_eq!(
        eligibility.suppressed_summary_ids,
        [SessionSummaryIdV1::new("predecessor").expect("valid id")]
            .into_iter()
            .collect()
    );
}

#[test]
fn non_current_summary_modes_retain_eligible_predecessors() {
    let session_id: SessionId = serde_json::from_str("\"session-1\"").expect("valid session id");
    let predecessor = summary("predecessor", "summary-old", "source-old", 5, 5);
    let successor = summary("successor", "summary-new", "source-new", 7, 7)
        .with_predecessor(predecessor.summary_id().clone())
        .expect("valid predecessor");
    let source_states = [
        (anchor("source-old"), covered_source(5, 5)),
        (anchor("source-new"), covered_source(7, 7)),
    ]
    .into_iter()
    .collect();

    for mode in [TemporalModeV1::Evolution, TemporalModeV1::Forensic] {
        let eligibility = evaluate_summary_lineage_eligibility(
            &[predecessor.clone(), successor.clone()],
            &source_states,
            &session_id,
            mode,
        )
        .expect("eligibility");
        assert_eq!(
            eligibility.eligible_anchor_ids,
            [anchor("summary-old"), anchor("summary-new")]
                .into_iter()
                .collect(),
            "{mode:?} must retain eligible predecessor summaries"
        );
        assert!(eligibility.suppressed_summary_ids.is_empty());
    }
}

#[test]
fn unknown_validity_sources_stay_eligible_while_missing_sources_reject() {
    let session_id: SessionId = serde_json::from_str("\"session-1\"").expect("valid session id");
    let missing = summary("missing", "summary-missing", "missing-source", 7, 7);
    let unknown_valid = summary("unknown-valid", "summary-unknown", "unknown-source", 7, 7);
    let source_states = [
        (
            anchor("unknown-source"),
            SummarySourceState::Covered {
                knowledge_at: UtcMicros(7),
                valid_time: TemporalValidityV1::Unknown,
            },
        ),
        // missing-source intentionally absent from the map
    ]
    .into_iter()
    .collect();

    let eligibility = evaluate_summary_lineage_eligibility(
        &[missing, unknown_valid],
        &source_states,
        &session_id,
        TemporalModeV1::Current,
    )
    .expect("eligibility");

    assert!(matches!(
        eligibility
            .rejections
            .get(&SessionSummaryIdV1::new("missing").expect("valid id")),
        Some(SummaryLineageRejection::MissingSource { .. })
    ));
    // Ingested messages carry no valid-time assertion today; that
    // uncertainty surfaces through occurrence-level coverage, not by
    // rejecting the summary's lineage outright.
    assert!(
        !eligibility
            .rejections
            .contains_key(&SessionSummaryIdV1::new("unknown-valid").expect("valid id"))
    );
    assert!(
        eligibility
            .eligible_anchor_ids
            .contains(&anchor("summary-unknown"))
    );
}

#[test]
fn evolution_marks_only_cycle_members_conflicted() {
    let cycle_a = occurrence(
        'a',
        "cycle-a",
        1,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1),
        },
    );
    let cycle_b = occurrence(
        'b',
        "cycle-b",
        2,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );
    let blocked = occurrence(
        'c',
        "blocked",
        3,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(3),
        },
    );
    let assertions = [
        assertion(TemporalAssertionKindV1::Corrects, "cycle-b", "cycle-a", 4),
        assertion(TemporalAssertionKindV1::Corrects, "cycle-a", "cycle-b", 5),
        assertion(TemporalAssertionKindV1::Supersedes, "blocked", "cycle-a", 6),
    ];

    let resolved = resolve_temporal(
        &[cycle_a, cycle_b, blocked],
        &[],
        &assertions,
        TemporalModeV1::Evolution,
    )
    .expect("resolution succeeds");

    let by_anchor = resolved
        .iter()
        .map(|item| (item.occurrence.anchor_id.clone(), item.conflicted))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(by_anchor.get(&anchor("cycle-a")), Some(&true));
    assert_eq!(by_anchor.get(&anchor("cycle-b")), Some(&true));
    assert_eq!(by_anchor.get(&anchor("blocked")), Some(&false));
    let order = resolved
        .iter()
        .map(|item| item.occurrence.anchor_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec![anchor("cycle-a"), anchor("cycle-b"), anchor("blocked")],
        "cycle SCC members must precede blocked descendants"
    );
}

#[test]
fn current_correction_chain_keeps_only_the_tip() {
    let original = occurrence(
        'a',
        "original",
        1,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1),
        },
    );
    let mid = occurrence(
        'b',
        "mid",
        2,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );
    let tip = occurrence(
        'c',
        "tip",
        3,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(3),
        },
    );
    let resolved = resolve_temporal(
        &[original, mid, tip],
        &[],
        &[
            assertion(TemporalAssertionKindV1::Corrects, "mid", "original", 2),
            assertion(TemporalAssertionKindV1::Corrects, "tip", "mid", 3),
        ],
        TemporalModeV1::Current,
    )
    .expect("resolution succeeds");

    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].occurrence.anchor_id, anchor("tip"));
    assert!(!resolved[0].conflicted);
}

#[test]
fn current_mutual_corrections_surface_conflict_instead_of_empty_set() {
    let left = occurrence(
        'a',
        "left",
        1,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1),
        },
    );
    let right = occurrence(
        'b',
        "right",
        2,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );
    let resolved = resolve_temporal(
        &[left, right],
        &[],
        &[
            assertion(TemporalAssertionKindV1::Corrects, "right", "left", 3),
            assertion(TemporalAssertionKindV1::Corrects, "left", "right", 4),
        ],
        TemporalModeV1::Current,
    )
    .expect("resolution succeeds");

    assert_eq!(resolved.len(), 2);
    assert!(resolved.iter().all(|item| item.conflicted));
}

#[test]
fn unrelated_conflict_does_not_cancel_authoritative_supersession() {
    let old = occurrence(
        'a',
        "old",
        1,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1),
        },
    );
    let disputed = occurrence(
        'b',
        "disputed",
        2,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );
    let successor = occurrence(
        'c',
        "successor",
        3,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(3),
        },
    );

    let resolved = resolve_temporal(
        &[old, disputed, successor],
        &[],
        &[
            assertion(TemporalAssertionKindV1::Contradicts, "old", "disputed", 4),
            assertion(TemporalAssertionKindV1::Supersedes, "successor", "old", 5),
        ],
        TemporalModeV1::Current,
    )
    .expect("resolution succeeds");

    assert_eq!(
        resolved
            .iter()
            .map(|item| item.occurrence.anchor_id.clone())
            .collect::<BTreeSet<_>>(),
        [anchor("disputed"), anchor("successor")]
            .into_iter()
            .collect()
    );
    assert!(
        resolved
            .iter()
            .any(|item| { item.occurrence.anchor_id == anchor("disputed") && item.conflicted })
    );
}

#[test]
fn copy_root_does_not_traverse_ineligible_parents() {
    let mut root = occurrence(
        'a',
        "root",
        1,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1),
        },
    );
    root.evidence = ResolutionEvidence::new(
        SessionAuthorityClassV1::CanonicalObservation,
        ValidatedAuthorization::Unauthorized,
    );
    let copied = occurrence(
        'b',
        "copied",
        2,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );
    let copy = provider_copy(&copied, &root);

    let resolved = resolve_temporal(&[root, copied], &[copy], &[], TemporalModeV1::Current)
        .expect("resolution succeeds");

    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].occurrence.anchor_id, anchor("copied"));
    assert_eq!(
        resolved[0].representative_id,
        resolved[0].occurrence.occurrence_id
    );
}

#[test]
fn evolution_lineage_edges_are_order_independent() {
    let original = occurrence(
        'a',
        "original",
        1,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1),
        },
    );
    let left = occurrence(
        'b',
        "left",
        2,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );
    let right = occurrence(
        'c',
        "right",
        3,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(3),
        },
    );
    let mut forward = [
        assertion(TemporalAssertionKindV1::Corrects, "left", "original", 40),
        assertion(TemporalAssertionKindV1::Corrects, "right", "original", 30),
    ];
    let baseline = resolve_temporal(
        &[original.clone(), left.clone(), right.clone()],
        &[],
        &forward,
        TemporalModeV1::Evolution,
    )
    .expect("resolution succeeds");
    forward.reverse();
    let reversed = resolve_temporal(
        &[original, left, right],
        &[],
        &forward,
        TemporalModeV1::Evolution,
    )
    .expect("resolution succeeds");

    assert_eq!(baseline.lineage_edges, reversed.lineage_edges);
    assert_eq!(
        baseline
            .lineage_edges
            .iter()
            .map(|edge| (
                edge.subject_anchor_id.clone(),
                edge.object_anchor_id.clone(),
                edge.knowledge_at
            ))
            .collect::<Vec<_>>(),
        vec![
            (anchor("left"), anchor("original"), UtcMicros(40)),
            (anchor("right"), anchor("original"), UtcMicros(30)),
        ]
    );
}

#[test]
fn current_strong_supersession_suppresses_weaker_evidence() {
    let original = occurrence(
        'a',
        "original",
        1,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1),
        },
    );
    let mut successor = occurrence(
        'b',
        "successor",
        2,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );
    successor.evidence.authority = SessionAuthorityClassV1::ProviderNative;
    let mut edge = assertion(
        TemporalAssertionKindV1::Supersedes,
        "successor",
        "original",
        2,
    );
    edge.evidence.authority = SessionAuthorityClassV1::ProviderNative;

    let resolved = resolve_temporal(
        &[original, successor],
        &[],
        &[edge],
        TemporalModeV1::Current,
    )
    .expect("resolution succeeds");

    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].occurrence.anchor_id, anchor("successor"));
    assert_eq!(
        resolved.lineage_edges[0].kind,
        ResolutionLineageEdgeKind::Supersession
    );
}

#[test]
fn forensic_retains_all_versions_and_lineage_without_suppression() {
    let original = occurrence(
        'a',
        "original",
        1,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1),
        },
    );
    let correction = occurrence(
        'b',
        "correction",
        2,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );
    let edge = assertion(
        TemporalAssertionKindV1::Corrects,
        "correction",
        "original",
        2,
    );

    let resolved = resolve_temporal(
        &[original, correction],
        &[],
        &[edge],
        TemporalModeV1::Forensic,
    )
    .expect("resolution succeeds");

    assert_eq!(resolved.len(), 2);
    assert!(resolved.iter().all(|item| !item.conflicted));
    assert_eq!(resolved.lineage_edges.len(), 1);
}

#[test]
fn resolver_filters_directly_constructed_unauthorized_assertions() {
    let original = occurrence(
        'a',
        "original",
        1,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1),
        },
    );
    let correction = occurrence(
        'b',
        "correction",
        2,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(2),
        },
    );
    let mut edge = assertion(
        TemporalAssertionKindV1::Corrects,
        "correction",
        "original",
        2,
    );
    edge.evidence = ResolutionEvidence::new(
        SessionAuthorityClassV1::CanonicalObservation,
        ValidatedAuthorization::Unauthorized,
    );

    let resolved = resolve_temporal(
        &[original, correction],
        &[],
        &[edge],
        TemporalModeV1::Current,
    )
    .expect("resolution succeeds");

    assert_eq!(resolved.len(), 2);
    assert!(resolved.lineage_edges.is_empty());
    assert!(resolved.iter().all(|item| !item.conflicted));
}
