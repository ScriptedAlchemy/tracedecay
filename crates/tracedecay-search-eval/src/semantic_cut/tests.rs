use super::*;

fn score(
    query_id: &str,
    stratum: &str,
    calibrated_feature_micros: u32,
    relevance: LabelledSemanticRelevanceV1,
) -> LabelledSemanticScoreV1 {
    LabelledSemanticScoreV1 {
        query_id: query_id.to_owned(),
        strata: vec![stratum.to_owned()],
        calibrated_feature_micros,
        relevance,
    }
}

fn positives(stratum: &str, values: &[u32]) -> Vec<LabelledSemanticScoreV1> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            score(
                &format!("positive-{index}"),
                stratum,
                *value,
                LabelledSemanticRelevanceV1::Positive,
            )
        })
        .collect()
}

fn negatives(stratum: &str, values: &[u32]) -> Vec<LabelledSemanticScoreV1> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            score(
                &format!("negative-{index}"),
                stratum,
                *value,
                LabelledSemanticRelevanceV1::Negative,
            )
        })
        .collect()
}

/// Twelve positives sitting well above twelve negatives, with a clean gap
/// between `520_000` and `700_000`.
fn separated_partition() -> Vec<LabelledSemanticScoreV1> {
    let mut scores = positives(
        "natural_language",
        &[
            700_000, 710_000, 720_000, 730_000, 740_000, 750_000, 760_000, 770_000, 780_000,
            790_000, 800_000, 810_000,
        ],
    );
    scores.extend(negatives(
        "no_answer",
        &[
            300_000, 320_000, 340_000, 360_000, 380_000, 400_000, 420_000, 440_000, 460_000,
            480_000, 500_000, 520_000,
        ],
    ));
    scores
}

#[test]
fn separated_labels_derive_the_cut_at_the_measured_boundary() {
    let train = separated_partition();
    let SemanticCutV1::Derived {
        threshold_ppm,
        provenance,
    } = derive_semantic_cut(&train)
    else {
        panic!("cleanly separated labels must derive a cut");
    };

    // The sweep admits on `>= cut`, so the lowest positive is the most
    // admitting cut that still rejects every negative.
    assert_eq!(threshold_ppm, 700_000);
    assert_eq!(provenance.train_positive_count, 12);
    assert_eq!(provenance.train_negative_count, 12);
    assert_eq!(
        provenance.train_true_positive_rate_ppm,
        CALIBRATED_FEATURE_SCALE_PPM
    );
    assert_eq!(provenance.train_false_positive_rate_ppm, 0);
    assert_eq!(
        provenance.train_separation_ppm, CALIBRATED_FEATURE_SCALE_PPM,
        "perfect separation must be reported as such"
    );
    assert_eq!(provenance.method, SEPARATION_SWEEP_METHOD_V1);
    assert!(
        provenance.train_positive_floor_ppm > provenance.train_negative_ceiling_ppm,
        "provenance must let a reviewer see the two populations do not overlap"
    );
}

/// The property that makes this un-tunable: the cut follows the measurement.
#[test]
fn moving_the_measured_scores_moves_the_derived_cut() {
    let baseline = derive_semantic_cut(&separated_partition()).threshold_ppm();

    let mut shifted = separated_partition();
    for entry in &mut shifted {
        if entry.relevance == LabelledSemanticRelevanceV1::Positive {
            entry.calibrated_feature_micros -= 200_000;
        }
    }
    let moved = derive_semantic_cut(&shifted).threshold_ppm();

    // Shifted down, the positives now span `500_000..=610_000` and overlap the
    // negatives' upper tail. The sweep trades the two rates off rather than
    // clearing the negatives outright: keeping every positive at `500_000`
    // admits two negatives for a separation of `833_334`, which beats
    // `540_000`, where rejecting every negative costs a third of the
    // positives for `666_666`.
    assert_eq!(baseline, 700_000);
    assert_eq!(moved, 500_000, "the cut tracks the scores it was measured on");
    assert_ne!(baseline, moved);
}

#[test]
fn fully_overlapping_labels_derive_an_admit_everything_cut() {
    let overlapping: Vec<_> = positives("natural_language", &[500_000; 12])
        .into_iter()
        .chain(negatives("no_answer", &[500_000; 12]))
        .collect();

    let cut = derive_semantic_cut(&overlapping);
    let SemanticCutV1::Derived {
        threshold_ppm,
        provenance,
    } = &cut
    else {
        panic!("a measured non-separation is still a measurement");
    };
    assert_eq!(*threshold_ppm, ADMIT_EVERY_CANDIDATE_THRESHOLD_PPM);
    assert_eq!(provenance.train_separation_ppm, 0);
    assert!(
        !cut.gates_candidates(),
        "no measured separation must gate nothing rather than guess a cut"
    );
}

#[test]
fn a_thin_positive_stratum_is_underpowered_and_admits_everything() {
    let thin: Vec<_> = positives("natural_language", &[700_000, 710_000])
        .into_iter()
        .chain(negatives("no_answer", &[100_000; 12]))
        .collect();

    let cut = derive_semantic_cut(&thin);
    assert_eq!(
        cut,
        SemanticCutV1::Underpowered {
            reason: SemanticCutUnderpoweredReasonV1::TooFewLabelledPositives,
            train_positive_count: 2,
            train_negative_count: 12,
        }
    );
    assert_eq!(cut.threshold_ppm(), ADMIT_EVERY_CANDIDATE_THRESHOLD_PPM);
    assert!(!cut.gates_candidates());
}

#[test]
fn a_thin_negative_stratum_is_underpowered_and_admits_everything() {
    let thin: Vec<_> = positives("natural_language", &[700_000; 12])
        .into_iter()
        .chain(negatives("no_answer", &[100_000, 120_000]))
        .collect();

    let cut = derive_semantic_cut(&thin);
    assert_eq!(
        cut,
        SemanticCutV1::Underpowered {
            reason: SemanticCutUnderpoweredReasonV1::TooFewLabelledNegatives,
            train_positive_count: 12,
            train_negative_count: 2,
        }
    );
    assert_eq!(cut.threshold_ppm(), ADMIT_EVERY_CANDIDATE_THRESHOLD_PPM);
}

#[test]
fn unmeasured_admits_everything_and_gates_nothing() {
    let cut = SemanticCutV1::Unmeasured;
    assert_eq!(cut.threshold_ppm(), ADMIT_EVERY_CANDIDATE_THRESHOLD_PPM);
    assert!(!cut.gates_candidates());
    cut.validate().expect("an unmeasured profile is a valid state");
}

#[test]
fn a_train_derived_cut_that_holds_on_validation_records_its_held_out_behaviour() {
    let train = separated_partition();
    let validation = separated_partition();

    let cut = derive_and_validate_semantic_cut(&train, &validation).expect("cut holds");
    let SemanticCutV1::Derived {
        threshold_ppm,
        provenance,
    } = cut
    else {
        panic!("expected a derived cut");
    };
    assert_eq!(threshold_ppm, 700_000);
    let validation = provenance.validation.expect("a gating cut records validation");
    assert_eq!(validation.positives_dropped, 0);
    assert_eq!(
        validation.negatives_rejected, 12,
        "the cut must be shown to earn its keep on held-out negatives"
    );
}

#[test]
fn a_cut_that_drops_validation_positives_fails_and_names_the_stratum() {
    let train = separated_partition();

    // The held-out partition carries a relevant natural-language candidate the
    // train-derived cut would drop.
    let mut validation = separated_partition();
    validation.push(score(
        "validation-006",
        "natural_language",
        410_000,
        LabelledSemanticRelevanceV1::Positive,
    ));

    let error = derive_and_validate_semantic_cut(&train, &validation)
        .expect_err("a cut that drops a labelled positive must not qualify");
    let SemanticCutValidationErrorV1::ValidationPositivesDropped {
        threshold_ppm,
        positives_dropped,
        worst_stratum,
        lowest_dropped_ppm,
        ..
    } = &error
    else {
        panic!("expected a dropped-positive failure, got {error}");
    };
    assert_eq!(*threshold_ppm, 700_000);
    assert_eq!(*positives_dropped, 1);
    assert_eq!(worst_stratum, "natural_language");
    assert_eq!(*lowest_dropped_ppm, 410_000);

    let rendered = error.to_string();
    for expected in [
        "train-derived semantic cut 700000 ppm",
        "1 of 13 labelled validation positives",
        "worst stratum natural_language",
        "410000 ppm",
    ] {
        assert!(
            rendered.contains(expected),
            "diagnostic must name the derivation, the held-out performance, and the failing \
             stratum; {expected:?} missing from {rendered:?}"
        );
    }
}

#[test]
fn a_gating_cut_cannot_qualify_against_a_thin_validation_partition() {
    let train = separated_partition();
    let validation: Vec<_> = positives("natural_language", &[900_000, 910_000])
        .into_iter()
        .chain(negatives("no_answer", &[100_000, 120_000]))
        .collect();

    let error = derive_and_validate_semantic_cut(&train, &validation)
        .expect_err("a gating cut needs held-out evidence");
    assert_eq!(
        error,
        SemanticCutValidationErrorV1::ValidationUnderpowered {
            threshold_ppm: 700_000,
            positive_count: 2,
            negative_count: 2,
        }
    );
}

/// An admit-everything cut drops nothing, so there is no held-out behaviour to
/// confirm and a thin validation partition must not fail it.
#[test]
fn an_admit_everything_cut_needs_no_validation_partition() {
    let train: Vec<_> = positives("natural_language", &[500_000; 12])
        .into_iter()
        .chain(negatives("no_answer", &[500_000; 12]))
        .collect();

    let cut = derive_and_validate_semantic_cut(&train, &[]).expect("nothing to hold out against");
    assert_eq!(cut.threshold_ppm(), ADMIT_EVERY_CANDIDATE_THRESHOLD_PPM);
}

#[test]
fn a_declared_cut_without_a_derivation_behind_it_is_rejected() {
    let honest = derive_and_validate_semantic_cut(&separated_partition(), &separated_partition())
        .expect("cut holds");
    honest.validate().expect("a real derivation validates");

    let SemanticCutV1::Derived { provenance, .. } = honest else {
        panic!("expected a derived cut");
    };

    // A hand-written cut that claims a derivation from too little evidence.
    let mut thin = provenance.clone();
    thin.train_positive_count = 2;
    assert_eq!(
        SemanticCutV1::Derived {
            threshold_ppm: 635_000,
            provenance: thin,
        }
        .validate(),
        Err(SemanticCutContractErrorV1::UnderpoweredProvenance {
            train_positive_count: 2,
            train_negative_count: 12,
        })
    );

    // A gating cut with the held-out evidence stripped off.
    let mut unvalidated = provenance.clone();
    unvalidated.validation = None;
    assert_eq!(
        SemanticCutV1::Derived {
            threshold_ppm: 635_000,
            provenance: unvalidated,
        }
        .validate(),
        Err(SemanticCutContractErrorV1::MissingValidation {
            threshold_ppm: 635_000,
        })
    );

    // A cut outside the calibrated feature range.
    assert_eq!(
        SemanticCutV1::Derived {
            threshold_ppm: CALIBRATED_FEATURE_SCALE_PPM + 1,
            provenance: provenance.clone(),
        }
        .validate(),
        Err(SemanticCutContractErrorV1::ThresholdOutOfRange {
            threshold_ppm: CALIBRATED_FEATURE_SCALE_PPM + 1,
        })
    );

    // A cut claiming some other derivation.
    let mut foreign = provenance;
    foreign.method = "hand-tuned.v1".to_owned();
    assert_eq!(
        SemanticCutV1::Derived {
            threshold_ppm: 635_000,
            provenance: foreign,
        }
        .validate(),
        Err(SemanticCutContractErrorV1::UnknownMethod {
            method: "hand-tuned.v1".to_owned(),
        })
    );
}

#[test]
fn the_derivation_is_deterministic_in_its_labelled_input() {
    let train = separated_partition();
    let mut shuffled = train.clone();
    shuffled.reverse();

    assert_eq!(derive_semantic_cut(&train), derive_semantic_cut(&shuffled));
}

#[test]
fn ties_break_toward_the_most_admitting_cut() {
    // Every negative sits at zero, so every cut from `1` upward rejects all of
    // them; the separation is identical for each, and the safest of those is
    // the one that drops the fewest positives.
    let train: Vec<_> = positives(
        "natural_language",
        &[
            200_000, 300_000, 400_000, 500_000, 600_000, 700_000, 800_000, 900_000, 210_000,
            310_000, 410_000, 510_000,
        ],
    )
    .into_iter()
    .chain(negatives("no_answer", &[0; 12]))
    .collect();

    assert_eq!(
        derive_semantic_cut(&train).threshold_ppm(),
        200_000,
        "among equally separating cuts the derivation keeps the most candidates"
    );
}

#[test]
fn the_declared_state_round_trips_through_the_workload_encoding() {
    let cut = derive_and_validate_semantic_cut(&separated_partition(), &separated_partition())
        .expect("cut holds");
    let encoded = serde_json::to_string(&cut).expect("encode");
    assert!(encoded.contains("\"state\":\"derived\""));
    assert!(
        encoded.contains("\"train_positive_count\":12"),
        "the stored cut must carry the evidence a reviewer audits: {encoded}"
    );
    assert_eq!(
        serde_json::from_str::<SemanticCutV1>(&encoded).expect("decode"),
        cut
    );

    let unmeasured = serde_json::to_string(&SemanticCutV1::Unmeasured).expect("encode");
    assert_eq!(unmeasured, "{\"state\":\"unmeasured\"}");
    assert_eq!(
        serde_json::from_str::<SemanticCutV1>(&unmeasured).expect("decode"),
        SemanticCutV1::Unmeasured
    );
}
