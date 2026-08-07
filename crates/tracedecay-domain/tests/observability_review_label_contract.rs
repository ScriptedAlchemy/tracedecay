//! Fixtures for the canonical review and outcome label vocabulary.
//!
//! These exhaust the label set, the independence/judgment combinations, the
//! legal evidence requirement, the runtime-versus-outcome distinction, the
//! censored-versus-unknown case, and late correction, as required by the
//! "Canonical review and outcome labels" section of
//! `docs/plans/tracedecay-v2/26-observability-accounting-and-usage.md`.

use tracedecay_domain::{
    CoverageStateV1, EvidenceHorizonV1, IndependentReviewEvidenceV1, LabelConflictProvenanceV1,
    LabelConflictResolutionV1, ObservationCutoffV1, OutcomeEvidenceSourceV1,
    REVIEW_OUTCOME_ANCHOR_LIMIT, REVIEW_OUTCOME_LABEL_SCHEMA_REVISION, ReviewIndependenceV1,
    ReviewJudgmentV1, ReviewOutcomeDispositionV1, ReviewOutcomeIdentityV1, ReviewOutcomeLabelV1,
    ReviewOutcomeSubjectV1, RuntimeOutcomeEvidenceV1, TaskOutcomeLabelV1,
};

const VALID_FROM_MICROS: i64 = 1_000;
const OBSERVATION_TIME_MICROS: i64 = 2_000;

fn subject() -> ReviewOutcomeSubjectV1 {
    ReviewOutcomeSubjectV1 {
        work_ref: "work:one".into(),
        attempt_ref: "attempt:one".into(),
        acceptance_ref: Some("acceptance:one".into()),
        decomposition_ref: Some("decomposition:one".into()),
    }
}

fn identity(
    label_revision: u64,
    supersedes_label_revision: Option<u64>,
) -> ReviewOutcomeIdentityV1 {
    ReviewOutcomeIdentityV1 {
        subject: subject(),
        label_revision,
        supersedes_label_revision,
        valid_from_micros: VALID_FROM_MICROS,
        observation_time_micros: OBSERVATION_TIME_MICROS,
    }
}

fn runtime_evidence(
    source: OutcomeEvidenceSourceV1,
    horizon: EvidenceHorizonV1,
) -> RuntimeOutcomeEvidenceV1 {
    RuntimeOutcomeEvidenceV1::new(source, horizon, CoverageStateV1::Known)
        .expect("runtime evidence is not independent review")
}

fn independent_review(judgment: ReviewJudgmentV1) -> IndependentReviewEvidenceV1 {
    IndependentReviewEvidenceV1::new(
        "reviewer:independent-one",
        ReviewIndependenceV1::Independent,
        judgment,
        EvidenceHorizonV1::complete(OBSERVATION_TIME_MICROS),
        CoverageStateV1::Known,
    )
    .expect("identified independent review over a closed horizon")
}

#[test]
fn every_label_independence_and_judgment_combination_round_trips() {
    let mut combinations = 0_usize;
    let mut legal = Vec::new();

    for outcome in TaskOutcomeLabelV1::ALL {
        for independence in ReviewIndependenceV1::ALL {
            for judgment in ReviewJudgmentV1::ALL {
                let disposition = ReviewOutcomeDispositionV1::new(outcome, independence, judgment);
                let encoded = serde_json::to_vec(&disposition).expect("serialize disposition");
                assert_eq!(
                    serde_json::from_slice::<ReviewOutcomeDispositionV1>(&encoded)
                        .expect("deserialize disposition"),
                    disposition,
                    "{disposition:?} must round-trip"
                );

                combinations += 1;
                if disposition.validate().is_ok() {
                    legal.push(disposition);
                }
            }
        }
    }

    assert_eq!(combinations, 7 * 5 * 4, "the vocabulary stays exhaustive");

    // Accepted and Rejected exist only as an independent judgment of the same
    // name; Pending and Reviewable state that no judgment exists yet.
    let legal_for = |outcome: TaskOutcomeLabelV1| {
        legal
            .iter()
            .filter(|disposition| disposition.outcome == outcome)
            .count()
    };
    assert_eq!(
        legal
            .iter()
            .filter(|disposition| disposition.outcome == TaskOutcomeLabelV1::Accepted)
            .copied()
            .collect::<Vec<_>>(),
        vec![ReviewOutcomeDispositionV1::new(
            TaskOutcomeLabelV1::Accepted,
            ReviewIndependenceV1::Independent,
            ReviewJudgmentV1::Accepted,
        )]
    );
    assert_eq!(
        legal
            .iter()
            .filter(|disposition| disposition.outcome == TaskOutcomeLabelV1::Rejected)
            .copied()
            .collect::<Vec<_>>(),
        vec![ReviewOutcomeDispositionV1::new(
            TaskOutcomeLabelV1::Rejected,
            ReviewIndependenceV1::Independent,
            ReviewJudgmentV1::Rejected,
        )]
    );
    assert_eq!(legal_for(TaskOutcomeLabelV1::Pending), 2);
    assert_eq!(legal_for(TaskOutcomeLabelV1::Reviewable), 5);
    // Measurement state stays orthogonal to judgment, so every judgment
    // remains representable alongside these three.
    assert_eq!(legal_for(TaskOutcomeLabelV1::ObservedPartial), 20);
    assert_eq!(legal_for(TaskOutcomeLabelV1::Censored), 20);
    assert_eq!(legal_for(TaskOutcomeLabelV1::Unknown), 20);
    assert_eq!(legal.len(), 69);
}

#[test]
fn label_wire_values_are_closed_and_stable() {
    let outcomes = [
        (TaskOutcomeLabelV1::Pending, "\"pending\""),
        (TaskOutcomeLabelV1::ObservedPartial, "\"observed_partial\""),
        (TaskOutcomeLabelV1::Reviewable, "\"reviewable\""),
        (TaskOutcomeLabelV1::Accepted, "\"accepted\""),
        (TaskOutcomeLabelV1::Rejected, "\"rejected\""),
        (TaskOutcomeLabelV1::Censored, "\"censored\""),
        (TaskOutcomeLabelV1::Unknown, "\"unknown\""),
    ];
    for (value, expected) in outcomes {
        assert_eq!(serde_json::to_string(&value).expect("serialize"), expected);
    }

    let independence = [
        (ReviewIndependenceV1::Independent, "\"independent\""),
        (ReviewIndependenceV1::NonIndependent, "\"non_independent\""),
        (ReviewIndependenceV1::Conflicted, "\"conflicted\""),
        (ReviewIndependenceV1::Missing, "\"missing\""),
        (ReviewIndependenceV1::Unknown, "\"unknown\""),
    ];
    for (value, expected) in independence {
        assert_eq!(serde_json::to_string(&value).expect("serialize"), expected);
    }

    let judgments = [
        (ReviewJudgmentV1::Accepted, "\"accepted\""),
        (ReviewJudgmentV1::Rejected, "\"rejected\""),
        (ReviewJudgmentV1::Partial, "\"partial\""),
        (ReviewJudgmentV1::Unknown, "\"unknown\""),
    ];
    for (value, expected) in judgments {
        assert_eq!(serde_json::to_string(&value).expect("serialize"), expected);
    }

    let sources = [
        (
            OutcomeEvidenceSourceV1::RuntimeTerminal,
            "\"runtime_terminal\"",
        ),
        (
            OutcomeEvidenceSourceV1::ProviderOutcome,
            "\"provider_outcome\"",
        ),
        (
            OutcomeEvidenceSourceV1::WorkerSelfReport,
            "\"worker_self_report\"",
        ),
        (
            OutcomeEvidenceSourceV1::IndependentReview,
            "\"independent_review\"",
        ),
        (OutcomeEvidenceSourceV1::Unknown, "\"unknown\""),
    ];
    for (value, expected) in sources {
        assert_eq!(serde_json::to_string(&value).expect("serialize"), expected);
    }

    let cutoffs = [
        (ObservationCutoffV1::Cancelled, "\"cancelled\""),
        (ObservationCutoffV1::Superseded, "\"superseded\""),
        (ObservationCutoffV1::LostAuthority, "\"lost_authority\""),
        (
            ObservationCutoffV1::UnfinishedHorizon,
            "\"unfinished_horizon\"",
        ),
        (ObservationCutoffV1::Unknown, "\"unknown\""),
    ];
    for (value, expected) in cutoffs {
        assert_eq!(serde_json::to_string(&value).expect("serialize"), expected);
    }

    // An unrecognized spelling is rejected, never folded into another cohort.
    assert!(serde_json::from_str::<TaskOutcomeLabelV1>("\"succeeded\"").is_err());
    assert!(serde_json::from_str::<TaskOutcomeLabelV1>("\"completed\"").is_err());
    assert!(serde_json::from_str::<ReviewIndependenceV1>("\"self\"").is_err());
    assert!(serde_json::from_str::<ReviewJudgmentV1>("\"approved\"").is_err());
    assert!(
        serde_json::from_str::<ReviewOutcomeDispositionV1>(
            r#"{"outcome":"accepted","independence":"independent","judgment":"accepted","note":"x"}"#
        )
        .is_err(),
        "unknown fields are denied"
    );
}

#[test]
fn independent_review_is_the_only_path_to_accepted() {
    let evidence = independent_review(ReviewJudgmentV1::Accepted);
    let label = ReviewOutcomeLabelV1::from_independent_review(
        identity(1, None),
        TaskOutcomeLabelV1::Accepted,
        &evidence,
    )
    .expect("independent review can accept");

    assert_eq!(label.schema_revision, REVIEW_OUTCOME_LABEL_SCHEMA_REVISION);
    assert_eq!(label.disposition.outcome, TaskOutcomeLabelV1::Accepted);
    assert_eq!(
        label.disposition.independence,
        ReviewIndependenceV1::Independent
    );
    assert_eq!(label.disposition.judgment, ReviewJudgmentV1::Accepted);
    assert_eq!(
        label.evidence_source,
        OutcomeEvidenceSourceV1::IndependentReview
    );
    assert!(label.evidence_horizon.complete);
    assert_eq!(
        label.reviewer_ref.as_deref(),
        Some("reviewer:independent-one")
    );
    assert_eq!(label.validate(), Ok(()));

    let encoded = serde_json::to_vec(&label).expect("serialize label");
    assert_eq!(
        serde_json::from_slice::<ReviewOutcomeLabelV1>(&encoded).expect("deserialize label"),
        label
    );

    // The reviewer's judgment, not the caller's, decides the label: an
    // independent rejection cannot be recorded as acceptance.
    let rejecting = independent_review(ReviewJudgmentV1::Rejected);
    assert_eq!(
        ReviewOutcomeLabelV1::from_independent_review(
            identity(1, None),
            TaskOutcomeLabelV1::Accepted,
            &rejecting,
        ),
        Err("review_outcome_disposition")
    );
    assert!(
        ReviewOutcomeLabelV1::from_independent_review(
            identity(1, None),
            TaskOutcomeLabelV1::Rejected,
            &rejecting,
        )
        .is_ok()
    );
}

#[test]
fn runtime_completed_or_worker_self_report_cannot_construct_accepted() {
    let horizon = EvidenceHorizonV1::complete(OBSERVATION_TIME_MICROS);

    for source in [
        OutcomeEvidenceSourceV1::RuntimeTerminal,
        OutcomeEvidenceSourceV1::ProviderOutcome,
        OutcomeEvidenceSourceV1::WorkerSelfReport,
        OutcomeEvidenceSourceV1::Unknown,
    ] {
        let evidence = runtime_evidence(source, horizon);
        for independence in ReviewIndependenceV1::ALL {
            for (outcome, judgment) in [
                (TaskOutcomeLabelV1::Accepted, ReviewJudgmentV1::Accepted),
                (TaskOutcomeLabelV1::Rejected, ReviewJudgmentV1::Rejected),
            ] {
                let result = ReviewOutcomeLabelV1::from_runtime_evidence(
                    identity(1, None),
                    ReviewOutcomeDispositionV1::new(outcome, independence, judgment),
                    evidence,
                    None,
                );
                assert!(
                    result.is_err(),
                    "{source:?} evidence must not produce {outcome:?}"
                );
                if independence.is_independent() {
                    assert_eq!(
                        result,
                        Err("review_outcome_independent_evidence"),
                        "{source:?} evidence must fail the independent-evidence rule"
                    );
                }
            }
        }

        // The same evidence remains fully usable for measurement state.
        assert!(
            ReviewOutcomeLabelV1::from_runtime_evidence(
                identity(1, None),
                ReviewOutcomeDispositionV1::new(
                    TaskOutcomeLabelV1::Reviewable,
                    ReviewIndependenceV1::Missing,
                    ReviewJudgmentV1::Unknown,
                ),
                evidence,
                None,
            )
            .is_ok(),
            "{source:?} evidence still supports measurement labels"
        );
    }

    // The witness type itself cannot be minted from a non-independent review,
    // so there is no second route into the independent-review constructor.
    for independence in [
        ReviewIndependenceV1::NonIndependent,
        ReviewIndependenceV1::Conflicted,
        ReviewIndependenceV1::Missing,
        ReviewIndependenceV1::Unknown,
    ] {
        assert_eq!(
            IndependentReviewEvidenceV1::new(
                "reviewer:self",
                independence,
                ReviewJudgmentV1::Accepted,
                horizon,
                CoverageStateV1::Known,
            )
            .err(),
            Some("review_evidence_independence")
        );
    }

    // An unfinished review horizon cannot be presented as a closed judgment.
    assert_eq!(
        IndependentReviewEvidenceV1::new(
            "reviewer:independent-one",
            ReviewIndependenceV1::Independent,
            ReviewJudgmentV1::Accepted,
            EvidenceHorizonV1::open(OBSERVATION_TIME_MICROS),
            CoverageStateV1::Known,
        )
        .err(),
        Some("review_evidence_horizon")
    );

    // And independent review cannot be relabelled as runtime evidence.
    assert_eq!(
        RuntimeOutcomeEvidenceV1::new(
            OutcomeEvidenceSourceV1::IndependentReview,
            horizon,
            CoverageStateV1::Known,
        )
        .err(),
        Some("runtime_evidence_source")
    );
}

#[test]
fn censored_outcome_is_distinguishable_from_unknown() {
    let evidence = runtime_evidence(
        OutcomeEvidenceSourceV1::RuntimeTerminal,
        EvidenceHorizonV1::open(OBSERVATION_TIME_MICROS),
    );
    let disposition = |outcome| {
        ReviewOutcomeDispositionV1::new(
            outcome,
            ReviewIndependenceV1::Missing,
            ReviewJudgmentV1::Unknown,
        )
    };

    let censored = ReviewOutcomeLabelV1::from_runtime_evidence(
        identity(1, None),
        disposition(TaskOutcomeLabelV1::Censored),
        evidence,
        Some(ObservationCutoffV1::Cancelled),
    )
    .expect("a censored label carries its cutoff");
    let unknown = ReviewOutcomeLabelV1::from_runtime_evidence(
        identity(1, None),
        disposition(TaskOutcomeLabelV1::Unknown),
        evidence,
        None,
    )
    .expect("an unknown label carries no cutoff");

    assert_ne!(censored, unknown);
    assert_eq!(
        censored.observation_cutoff,
        Some(ObservationCutoffV1::Cancelled)
    );
    assert_eq!(unknown.observation_cutoff, None);

    let censored_wire = serde_json::to_string(&censored).expect("serialize censored");
    let unknown_wire = serde_json::to_string(&unknown).expect("serialize unknown");
    assert!(censored_wire.contains("\"censored\"") && censored_wire.contains("\"cancelled\""));
    assert!(unknown_wire.contains("\"unknown\"") && !unknown_wire.contains("observation_cutoff"));

    // Neither label can borrow the other's shape.
    assert_eq!(
        ReviewOutcomeLabelV1::from_runtime_evidence(
            identity(1, None),
            disposition(TaskOutcomeLabelV1::Censored),
            evidence,
            None,
        ),
        Err("review_outcome_observation_cutoff")
    );
    assert_eq!(
        ReviewOutcomeLabelV1::from_runtime_evidence(
            identity(1, None),
            disposition(TaskOutcomeLabelV1::Unknown),
            evidence,
            Some(ObservationCutoffV1::Cancelled),
        ),
        Err("review_outcome_observation_cutoff")
    );

    // Every cutoff reason stays representable on a censored label.
    for cutoff in ObservationCutoffV1::ALL {
        assert!(
            ReviewOutcomeLabelV1::from_runtime_evidence(
                identity(1, None),
                disposition(TaskOutcomeLabelV1::Censored),
                evidence,
                Some(cutoff),
            )
            .is_ok(),
            "{cutoff:?} must be representable"
        );
    }

    // An unfinished horizon cannot be claimed over a closed one.
    assert_eq!(
        ReviewOutcomeLabelV1::from_runtime_evidence(
            identity(1, None),
            disposition(TaskOutcomeLabelV1::Censored),
            runtime_evidence(
                OutcomeEvidenceSourceV1::RuntimeTerminal,
                EvidenceHorizonV1::complete(OBSERVATION_TIME_MICROS),
            ),
            Some(ObservationCutoffV1::UnfinishedHorizon),
        ),
        Err("review_outcome_evidence_horizon")
    );
}

#[test]
fn late_correction_appends_a_revision_and_leaves_the_prior_label_queryable() {
    let prior = ReviewOutcomeLabelV1::from_runtime_evidence(
        identity(1, None),
        ReviewOutcomeDispositionV1::new(
            TaskOutcomeLabelV1::Reviewable,
            ReviewIndependenceV1::Missing,
            ReviewJudgmentV1::Unknown,
        ),
        runtime_evidence(
            OutcomeEvidenceSourceV1::RuntimeTerminal,
            EvidenceHorizonV1::complete(OBSERVATION_TIME_MICROS),
        ),
        None,
    )
    .expect("runtime evidence can only make the work reviewable");

    let mut corrected = ReviewOutcomeLabelV1::from_independent_review(
        identity(2, Some(1)),
        TaskOutcomeLabelV1::Rejected,
        &independent_review(ReviewJudgmentV1::Rejected),
    )
    .expect("late independent review appends a revision");
    corrected.conflict_provenance = Some(LabelConflictProvenanceV1 {
        conflicting_label_revision: 1,
        conflicting_evidence_source: OutcomeEvidenceSourceV1::RuntimeTerminal,
        resolution: LabelConflictResolutionV1::IndependentReviewOverride,
    });

    assert_eq!(corrected.validate(), Ok(()));
    assert!(corrected.is_correction_of(&prior));
    assert!(!prior.is_correction_of(&corrected));
    // The superseded revision is untouched and still readable.
    assert_eq!(prior.identity.label_revision, 1);
    assert_eq!(prior.disposition.outcome, TaskOutcomeLabelV1::Reviewable);
    assert_eq!(
        prior.evidence_source,
        OutcomeEvidenceSourceV1::RuntimeTerminal
    );
    assert_eq!(corrected.identity.supersedes_label_revision, Some(1));

    let encoded = serde_json::to_vec(&corrected).expect("serialize correction");
    assert_eq!(
        serde_json::from_slice::<ReviewOutcomeLabelV1>(&encoded).expect("deserialize correction"),
        corrected
    );

    // A correction may not reuse or precede the revision it supersedes.
    for label_revision in [0, 1] {
        let mut rewrite = corrected.clone();
        rewrite.identity.label_revision = label_revision;
        assert_eq!(rewrite.validate(), Err("review_outcome_label_revision"));
    }

    // Runtime evidence cannot claim to have overridden an independent review.
    let mut forged = prior.clone();
    forged.identity = identity(3, Some(2));
    forged.conflict_provenance = Some(LabelConflictProvenanceV1 {
        conflicting_label_revision: 2,
        conflicting_evidence_source: OutcomeEvidenceSourceV1::IndependentReview,
        resolution: LabelConflictResolutionV1::IndependentReviewOverride,
    });
    assert_eq!(forged.validate(), Err("review_outcome_conflict_provenance"));
}

#[test]
fn label_records_reject_unprojectable_identity_bounds_and_coverage() {
    let base = ReviewOutcomeLabelV1::from_independent_review(
        identity(1, None),
        TaskOutcomeLabelV1::Accepted,
        &independent_review(ReviewJudgmentV1::Accepted),
    )
    .expect("baseline accepted label");

    let mut wrong_schema = base.clone();
    wrong_schema.schema_revision = REVIEW_OUTCOME_LABEL_SCHEMA_REVISION + 1;
    assert_eq!(
        wrong_schema.validate(),
        Err("review_outcome_schema_revision")
    );

    let mut blank_subject = base.clone();
    blank_subject.identity.subject.work_ref = String::new();
    assert_eq!(blank_subject.validate(), Err("review_outcome_subject"));

    let mut observed_before_valid = base.clone();
    observed_before_valid.identity.observation_time_micros = VALID_FROM_MICROS - 1;
    assert_eq!(
        observed_before_valid.validate(),
        Err("review_outcome_temporal_range")
    );

    let mut unknown_coverage = base.clone();
    unknown_coverage.coverage = CoverageStateV1::Unknown;
    assert_eq!(unknown_coverage.validate(), Err("review_outcome_coverage"));

    let mut anonymous = base.clone();
    anonymous.reviewer_ref = None;
    assert_eq!(
        anonymous.validate(),
        Err("review_outcome_independent_evidence")
    );

    let mut impossible_confidence = base.clone();
    impossible_confidence.confidence_ppm = Some(1_000_001);
    assert_eq!(
        impossible_confidence.validate(),
        Err("review_outcome_confidence")
    );

    let mut too_many_anchors = base.clone();
    too_many_anchors.retrieval_anchor_refs = (0..=REVIEW_OUTCOME_ANCHOR_LIMIT)
        .map(|index| format!("anchor:{index}"))
        .collect();
    assert_eq!(
        too_many_anchors.validate(),
        Err("review_outcome_anchor_refs")
    );

    let mut duplicate_anchors = base.clone();
    duplicate_anchors.retrieval_anchor_refs = vec!["anchor:one".into(), "anchor:one".into()];
    assert_eq!(
        duplicate_anchors.validate(),
        Err("review_outcome_anchor_refs")
    );

    let mut bounded_anchors = base.clone();
    bounded_anchors.retrieval_anchor_refs = (0..REVIEW_OUTCOME_ANCHOR_LIMIT)
        .map(|index| format!("anchor:{index}"))
        .collect();
    assert_eq!(bounded_anchors.validate(), Ok(()));

    // Pending states that no evidence has closed, so a closed horizon is not
    // representable underneath it.
    let pending = ReviewOutcomeLabelV1::from_runtime_evidence(
        identity(1, None),
        ReviewOutcomeDispositionV1::new(
            TaskOutcomeLabelV1::Pending,
            ReviewIndependenceV1::Missing,
            ReviewJudgmentV1::Unknown,
        ),
        runtime_evidence(
            OutcomeEvidenceSourceV1::RuntimeTerminal,
            EvidenceHorizonV1::complete(OBSERVATION_TIME_MICROS),
        ),
        None,
    );
    assert_eq!(pending, Err("review_outcome_evidence_horizon"));
}
