//! Measured semantic acceptance cut for the evaluation profile matrix.
//!
//! The semantic lane's contribution to fusion is dropped when its calibrated
//! feature falls under `FusionProfile::minimum_calibrated_feature_micros`.
//! That cut used to be a checked-in constant, and a constant is wrong here for
//! the same reason it is wrong in
//! [`tracedecay_usecases::semantic_runtime::acceptance_calibration`]: a cosine
//! cut-off is a property of the *model*, the *corpus*, and the *query regime*,
//! not of the fixture that declares it. A declared number also invites the
//! failure this module exists to end — the field was re-tuned five times in
//! one day (`700000` → `690000` → `700000` → `400000` → `635000`) to move a
//! failing activation gate, because moving it was cheaper than measuring it.
//!
//! # What is measured
//!
//! The evaluation workload carries labelled data on both sides of the cut:
//! queries whose label names the relevant `anchors`, and `no_answer` queries
//! whose label names an `absence_literal` or `forbidden_documents` and whose
//! every returned candidate is therefore wrong. Those labels are split into a
//! `train` and a `validation` partition.
//!
//! This module derives the cut from the **train** partition's measured
//! separation between positive and negative semantic scores, and then requires
//! the derived cut to hold on the **validation** partition. The scores are the
//! calibrated features fusion itself would see, taken from the production
//! exact-flat oracle, which is captured *before* the cut is applied — so the
//! derivation observes the same quantity it gates, and observing it costs no
//! additional model work.
//!
//! Unlike `acceptance_calibration`'s code↔code background distribution, these
//! are the natural-language↔code scores the gate actually decides. That is the
//! regime gap `acceptance_calibration` documents as unmeasurable from the
//! committed generation alone; the labelled workload is what closes it.
//!
//! # The method, and why this one
//!
//! Fusion admits a candidate when `calibrated_feature >= cut`, so a cut is
//! only ever one of the observed scores: every value between two adjacent
//! observations partitions the data identically. The derivation sweeps those
//! observations and picks the cut with the greatest measured separation,
//! [Youden's J][j] — the true-positive rate it keeps minus the false-positive
//! rate it admits.
//!
//! Ties break toward the **most admitting** (lowest) cut. When two cuts
//! separate the labelled data equally well, the one that drops fewer
//! candidates is strictly safer: a cut that drops candidates without
//! separating anything is exactly the silent degradation to the lexical
//! baseline that this whole lane is meant to avoid.
//!
//! A consequence worth stating plainly: when the labelled data shows no
//! separation at all, the sweep derives `0` — admit everything — because that
//! is what the measurement says. That is a derived result, not a fallback, and
//! it is why this cannot be tuned to move a gate.
//!
//! [j]: https://en.wikipedia.org/wiki/Youden%27s_J_statistic
//!
//! # What is *not* claimed
//!
//! The cut is derived from one corpus, one embedding model, and one labelled
//! workload. It is honest for that generation and no further, which is why the
//! derived value travels with [`SemanticCutProvenanceV1`] instead of being
//! restated as a literal: a reviewer can see the partition sizes and the
//! measured rates that produced it, and a re-derivation on different evidence
//! is expected to produce a different number.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::nearest_rank;

/// Full width of the calibrated feature range, in parts per million.
///
/// `ScoreDomainCalibrationV1::calibrate` maps every raw score into
/// `0..=CALIBRATED_FEATURE_SCALE_PPM`, and under the evaluation semantic score
/// domain that value is nonnegative cosine similarity expressed directly in
/// ppm.
pub const CALIBRATED_FEATURE_SCALE_PPM: u32 = 1_000_000;

/// The cut used whenever no measurement stands behind one.
///
/// Admitting every candidate keeps an uncalibrated profile behaving like the
/// lexical-plus-semantic union rather than silently collapsing to lexical
/// only. This mirrors `acceptance_calibration`'s uncalibrated bound
/// deliberately: abstention is the job of a measured cut, never of a missing
/// one, and never of a guess standing in for one.
pub const ADMIT_EVERY_CANDIDATE_THRESHOLD_PPM: u32 = 0;

/// Labelled observations required, on each side, before a rate means anything.
///
/// A true- or false-positive rate over fewer than eight observations moves by
/// more than twelve percent per observation, so its resolution would be
/// coarser than the separation it claims to measure.
pub const MINIMUM_LABELLED_OBSERVATIONS: usize = 8;

/// Validation positives the derived cut is allowed to drop, in ppm of the
/// validation positive population.
///
/// Zero. Dropping a candidate the workload labels relevant is precisely the
/// regression an unmeasured cut caused, so a cut derived on train that cannot
/// keep every labelled validation positive has not been shown to generalize.
/// Exceeding this is a typed qualification failure, never a reason to move the
/// cut.
pub const MAX_VALIDATION_POSITIVE_LOSS_PPM: u32 = 0;

/// Human-facing instruction attached to a declaration/derivation mismatch.
///
/// The remedy for a mismatch is always to stamp what the run measured, never
/// to edit the number toward whatever makes a gate pass.
pub const RESTAMP_INSTRUCTION: &str =
    "stamp the derivation this run measured into the profile matrix; do not edit the cut by hand";

/// Lower tail of the positive distribution retained as provenance.
const POSITIVE_FLOOR_PERCENTILE: usize = 5;
/// Upper tail of the negative distribution retained as provenance.
const NEGATIVE_CEILING_PERCENTILE: usize = 95;
/// Midpoint of each distribution retained as provenance.
const MEDIAN_PERCENTILE: usize = 50;

/// Identifier for the derivation this module implements, retained in
/// provenance so a stored cut names the procedure that produced it.
pub const SEPARATION_SWEEP_METHOD_V1: &str = "train-separation-sweep.youden-j.v1";

/// Which side of the labelled separation one observed score sits on.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum LabelledSemanticRelevanceV1 {
    /// The candidate is one of the query's checked-in relevant anchors.
    Positive,
    /// The candidate is one the query's label rules out: a `no_answer` query
    /// names no relevant anchor at all, so every candidate it returns is
    /// wrong, and any query may additionally forbid specific anchors or
    /// documents.
    Negative,
}

/// One labelled semantic observation taken from a query's exact-flat oracle.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LabelledSemanticScoreV1 {
    pub query_id: String,
    /// The query's checked-in strata, retained so a validation failure can
    /// name the stratum that failed rather than only the aggregate.
    pub strata: Vec<String>,
    /// The candidate's calibrated semantic feature, exactly as
    /// `ScoreDomainCalibrationV1::calibrate` produces it for fusion.
    pub calibrated_feature_micros: u32,
    pub relevance: LabelledSemanticRelevanceV1,
}

/// Why a labelled partition could not support a derivation.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticCutUnderpoweredReasonV1 {
    /// Fewer than [`MINIMUM_LABELLED_OBSERVATIONS`] labelled relevant scores.
    TooFewLabelledPositives,
    /// Fewer than [`MINIMUM_LABELLED_OBSERVATIONS`] labelled wrong scores.
    TooFewLabelledNegatives,
}

/// Measured separation statistics retained beside a derived cut.
///
/// This exists so a reviewer can audit the number without re-running the
/// model: the partition sizes state how much evidence stood behind it, and the
/// distribution tails state how far apart the two populations actually sat.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticCutProvenanceV1 {
    /// The derivation that produced the cut. See
    /// [`SEPARATION_SWEEP_METHOD_V1`].
    pub method: String,
    pub train_positive_count: u32,
    pub train_negative_count: u32,
    /// [`POSITIVE_FLOOR_PERCENTILE`] of the train positives.
    pub train_positive_floor_ppm: u32,
    pub train_positive_median_ppm: u32,
    pub train_negative_median_ppm: u32,
    /// [`NEGATIVE_CEILING_PERCENTILE`] of the train negatives.
    pub train_negative_ceiling_ppm: u32,
    /// Train positives the cut keeps, in ppm of the train positives.
    pub train_true_positive_rate_ppm: u32,
    /// Train negatives the cut admits, in ppm of the train negatives.
    pub train_false_positive_rate_ppm: u32,
    /// Measured separation at the chosen cut: the true-positive rate minus the
    /// false-positive rate. Zero means the labelled data showed no separation
    /// and the derivation therefore admits everything.
    pub train_separation_ppm: u32,
    /// How the cut behaved on the held-out partition. `None` only when the cut
    /// gates nothing, in which case there is nothing to hold out against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation: Option<SemanticCutValidationV1>,
}

/// How a train-derived cut behaved on the held-out validation partition.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticCutValidationV1 {
    pub positive_count: u32,
    pub negative_count: u32,
    /// Labelled-relevant validation candidates the cut dropped. Bounded by
    /// [`MAX_VALIDATION_POSITIVE_LOSS_PPM`].
    pub positives_dropped: u32,
    /// Labelled-wrong validation candidates the cut rejected. This is what the
    /// cut earns; a cut that rejects none is doing no work.
    pub negatives_rejected: u32,
}

/// A profile's semantic acceptance cut, as a measured state rather than a
/// number.
///
/// The state is what the checked-in profile matrix carries. There is
/// deliberately no variant holding an unexplained value: a cut either came
/// from a measurement and travels with the measurement, or the labelled
/// evidence was too thin and the profile admits everything and says so.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum SemanticCutV1 {
    /// No native evaluation has produced labelled semantic scores for this
    /// profile yet, so nothing has been measured and the profile admits every
    /// semantic candidate. See [`ADMIT_EVERY_CANDIDATE_THRESHOLD_PPM`].
    ///
    /// This is the honest state for a profile whose packaged native
    /// qualification is still unavailable: the measured
    /// `SemanticCalibrationProfileV1::maximum_distance_micros` remains the
    /// semantic lane's only abstention until a native run derives this one.
    Unmeasured,
    /// A native run produced labelled scores, but one side of the labelled
    /// separation was too thin for a cut to mean anything, so the profile
    /// admits every semantic candidate.
    Underpowered {
        reason: SemanticCutUnderpoweredReasonV1,
        train_positive_count: u32,
        train_negative_count: u32,
    },
    /// Derived from the train partition's measured separation and held on
    /// validation.
    Derived {
        threshold_ppm: u32,
        provenance: SemanticCutProvenanceV1,
    },
}

impl SemanticCutV1 {
    /// The cut that reaches fusion for this state.
    pub const fn threshold_ppm(&self) -> u32 {
        match self {
            Self::Unmeasured | Self::Underpowered { .. } => ADMIT_EVERY_CANDIDATE_THRESHOLD_PPM,
            Self::Derived { threshold_ppm, .. } => *threshold_ppm,
        }
    }

    /// Whether this cut actually gates anything. An admit-everything cut is a
    /// truthful measured outcome, but it has no held-out behaviour to check.
    pub const fn gates_candidates(&self) -> bool {
        self.threshold_ppm() > ADMIT_EVERY_CANDIDATE_THRESHOLD_PPM
    }

    /// Reject a state that could not have come from this module.
    ///
    /// A cut outside the calibrated feature range is rejected by fusion at
    /// runtime; catching it at workload load keeps that from being discovered
    /// mid-evaluation.
    pub fn validate(&self) -> Result<(), SemanticCutContractErrorV1> {
        match self {
            Self::Unmeasured | Self::Underpowered { .. } => Ok(()),
            Self::Derived {
                threshold_ppm,
                provenance,
            } => {
                if *threshold_ppm > CALIBRATED_FEATURE_SCALE_PPM {
                    return Err(SemanticCutContractErrorV1::ThresholdOutOfRange {
                        threshold_ppm: *threshold_ppm,
                    });
                }
                if provenance.method != SEPARATION_SWEEP_METHOD_V1 {
                    return Err(SemanticCutContractErrorV1::UnknownMethod {
                        method: provenance.method.clone(),
                    });
                }
                let positives = provenance.train_positive_count as usize;
                let negatives = provenance.train_negative_count as usize;
                if positives < MINIMUM_LABELLED_OBSERVATIONS
                    || negatives < MINIMUM_LABELLED_OBSERVATIONS
                {
                    return Err(SemanticCutContractErrorV1::UnderpoweredProvenance {
                        train_positive_count: provenance.train_positive_count,
                        train_negative_count: provenance.train_negative_count,
                    });
                }
                if self.gates_candidates() && provenance.validation.is_none() {
                    return Err(SemanticCutContractErrorV1::MissingValidation {
                        threshold_ppm: *threshold_ppm,
                    });
                }
                Ok(())
            }
        }
    }
}

/// A declared cut that no derivation could have produced.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SemanticCutContractErrorV1 {
    #[error(
        "declared semantic cut {threshold_ppm} ppm exceeds the calibrated feature range of {} ppm",
        CALIBRATED_FEATURE_SCALE_PPM
    )]
    ThresholdOutOfRange { threshold_ppm: u32 },
    #[error(
        "declared semantic cut names derivation {method}, which is not {}",
        SEPARATION_SWEEP_METHOD_V1
    )]
    UnknownMethod { method: String },
    #[error(
        "declared semantic cut claims a derivation from {train_positive_count} labelled \
         positives and {train_negative_count} labelled negatives, below the {} each side \
         needs; an underpowered partition must declare the underpowered state, not a derived cut",
        MINIMUM_LABELLED_OBSERVATIONS
    )]
    UnderpoweredProvenance {
        train_positive_count: u32,
        train_negative_count: u32,
    },
    #[error(
        "declared semantic cut {threshold_ppm} ppm gates candidates but carries no validation \
         evidence; a cut that drops candidates must be shown to hold on the held-out partition"
    )]
    MissingValidation { threshold_ppm: u32 },
}

/// A train-derived cut that did not hold on the validation partition.
///
/// Every variant names the train-derived cut, the measured validation
/// behaviour, and — where one exists — the stratum that failed, so the
/// diagnostic identifies the derivation rather than only reporting that a
/// number was rejected.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SemanticCutValidationErrorV1 {
    #[error(
        "train-derived semantic cut {threshold_ppm} ppm drops {positives_dropped} of \
         {positive_count} labelled validation positives ({observed_loss_ppm} ppm loss, bound \
         {} ppm); worst stratum {worst_stratum} loses {worst_stratum_dropped} of \
         {worst_stratum_total}, and the lowest-scoring dropped positive sat at \
         {lowest_dropped_ppm} ppm",
        MAX_VALIDATION_POSITIVE_LOSS_PPM
    )]
    ValidationPositivesDropped {
        threshold_ppm: u32,
        positive_count: u32,
        positives_dropped: u32,
        observed_loss_ppm: u32,
        worst_stratum: String,
        worst_stratum_dropped: u32,
        worst_stratum_total: u32,
        lowest_dropped_ppm: u32,
    },
    #[error(
        "train-derived semantic cut {threshold_ppm} ppm cannot be checked: the validation \
         partition holds {positive_count} labelled positives and {negative_count} labelled \
         negatives, below the {} each side needs; a gating cut without held-out evidence is \
         not qualified",
        MINIMUM_LABELLED_OBSERVATIONS
    )]
    ValidationUnderpowered {
        threshold_ppm: u32,
        positive_count: u32,
        negative_count: u32,
    },
}

/// Derive the cut from one labelled partition's measured separation.
///
/// Deterministic in its input: the same labelled scores always yield the same
/// state, so the evaluator that measures a run and a reviewer re-deriving from
/// the retained scores reach the identical cut.
pub fn derive_semantic_cut(train: &[LabelledSemanticScoreV1]) -> SemanticCutV1 {
    let (positives, negatives) = split_scores(train);
    let train_positive_count = positives.len() as u32;
    let train_negative_count = negatives.len() as u32;
    if positives.len() < MINIMUM_LABELLED_OBSERVATIONS {
        return SemanticCutV1::Underpowered {
            reason: SemanticCutUnderpoweredReasonV1::TooFewLabelledPositives,
            train_positive_count,
            train_negative_count,
        };
    }
    if negatives.len() < MINIMUM_LABELLED_OBSERVATIONS {
        return SemanticCutV1::Underpowered {
            reason: SemanticCutUnderpoweredReasonV1::TooFewLabelledNegatives,
            train_positive_count,
            train_negative_count,
        };
    }

    let (threshold_ppm, separation_ppm) = best_separating_cut(&positives, &negatives);
    let kept = at_or_above(&positives, threshold_ppm);
    let admitted = at_or_above(&negatives, threshold_ppm);
    SemanticCutV1::Derived {
        threshold_ppm,
        provenance: SemanticCutProvenanceV1 {
            method: SEPARATION_SWEEP_METHOD_V1.to_owned(),
            train_positive_count,
            train_negative_count,
            train_positive_floor_ppm: percentile_ppm(&positives, POSITIVE_FLOOR_PERCENTILE),
            train_positive_median_ppm: percentile_ppm(&positives, MEDIAN_PERCENTILE),
            train_negative_median_ppm: percentile_ppm(&negatives, MEDIAN_PERCENTILE),
            train_negative_ceiling_ppm: percentile_ppm(&negatives, NEGATIVE_CEILING_PERCENTILE),
            train_true_positive_rate_ppm: rate_ppm(kept, positives.len()),
            train_false_positive_rate_ppm: rate_ppm(admitted, negatives.len()),
            train_separation_ppm: separation_ppm,
            validation: None,
        },
    }
}

/// Derive the cut on `train` and require it to hold on `validation`.
///
/// This is the whole contract in one call: a cut that gates candidates is
/// returned only when the held-out partition confirms it, and a cut that gates
/// nothing needs no confirmation because it drops nothing. There is no path
/// that returns a declared constant when the derivation fails.
pub fn derive_and_validate_semantic_cut(
    train: &[LabelledSemanticScoreV1],
    validation: &[LabelledSemanticScoreV1],
) -> Result<SemanticCutV1, SemanticCutValidationErrorV1> {
    let derived = derive_semantic_cut(train);
    let SemanticCutV1::Derived {
        threshold_ppm,
        mut provenance,
    } = derived
    else {
        return Ok(derived);
    };
    if threshold_ppm == ADMIT_EVERY_CANDIDATE_THRESHOLD_PPM {
        return Ok(SemanticCutV1::Derived {
            threshold_ppm,
            provenance,
        });
    }
    provenance.validation = Some(validate_semantic_cut(threshold_ppm, validation)?);
    Ok(SemanticCutV1::Derived {
        threshold_ppm,
        provenance,
    })
}

/// Measure one already-derived cut against the held-out partition.
pub fn validate_semantic_cut(
    threshold_ppm: u32,
    validation: &[LabelledSemanticScoreV1],
) -> Result<SemanticCutValidationV1, SemanticCutValidationErrorV1> {
    let (positives, negatives) = split_scores(validation);
    let positive_count = positives.len() as u32;
    let negative_count = negatives.len() as u32;
    if positives.len() < MINIMUM_LABELLED_OBSERVATIONS
        || negatives.len() < MINIMUM_LABELLED_OBSERVATIONS
    {
        return Err(SemanticCutValidationErrorV1::ValidationUnderpowered {
            threshold_ppm,
            positive_count,
            negative_count,
        });
    }
    let kept = at_or_above(&positives, threshold_ppm);
    let positives_dropped = (positives.len() - kept) as u32;
    let admitted = at_or_above(&negatives, threshold_ppm);
    let observed_loss_ppm = rate_ppm(positives.len() - kept, positives.len());
    if observed_loss_ppm > MAX_VALIDATION_POSITIVE_LOSS_PPM {
        let (worst_stratum, worst_stratum_dropped, worst_stratum_total) =
            worst_dropped_stratum(validation, threshold_ppm);
        let lowest_dropped_ppm = validation
            .iter()
            .filter(|score| {
                score.relevance == LabelledSemanticRelevanceV1::Positive
                    && score.calibrated_feature_micros < threshold_ppm
            })
            .map(|score| score.calibrated_feature_micros)
            .min()
            .unwrap_or_default();
        return Err(SemanticCutValidationErrorV1::ValidationPositivesDropped {
            threshold_ppm,
            positive_count,
            positives_dropped,
            observed_loss_ppm,
            worst_stratum,
            worst_stratum_dropped,
            worst_stratum_total,
            lowest_dropped_ppm,
        });
    }
    Ok(SemanticCutValidationV1 {
        positive_count,
        negative_count,
        positives_dropped,
        negatives_rejected: (negatives.len() - admitted) as u32,
    })
}

/// Split labelled scores into ascending positive and negative populations.
fn split_scores(scores: &[LabelledSemanticScoreV1]) -> (Vec<u64>, Vec<u64>) {
    let mut positives = Vec::new();
    let mut negatives = Vec::new();
    for score in scores {
        let value = u64::from(score.calibrated_feature_micros);
        match score.relevance {
            LabelledSemanticRelevanceV1::Positive => positives.push(value),
            LabelledSemanticRelevanceV1::Negative => negatives.push(value),
        }
    }
    positives.sort_unstable();
    negatives.sort_unstable();
    (positives, negatives)
}

/// Sweep every partition the observed scores can express and return the
/// best-separating cut with its measured separation.
///
/// Fusion admits on `calibrated_feature >= cut`, so only observed values can
/// change the partition, and `0` is always considered because admitting
/// everything must be reachable when nothing separates.
fn best_separating_cut(positives: &[u64], negatives: &[u64]) -> (u32, u32) {
    let mut candidates: Vec<u64> = Vec::with_capacity(positives.len() + negatives.len() + 1);
    candidates.push(0);
    candidates.extend_from_slice(positives);
    candidates.extend_from_slice(negatives);
    candidates.sort_unstable();
    candidates.dedup();

    let mut best_cut = 0_u64;
    let mut best_separation = 0_i64;
    for &cut in &candidates {
        let true_positive_rate = rate_ppm(at_or_above(positives, cut as u32), positives.len());
        let false_positive_rate = rate_ppm(at_or_above(negatives, cut as u32), negatives.len());
        let separation = i64::from(true_positive_rate) - i64::from(false_positive_rate);
        // Strictly greater keeps the lowest — most admitting — cut among ties.
        if separation > best_separation {
            best_separation = separation;
            best_cut = cut;
        }
    }
    (
        u32::try_from(best_cut).unwrap_or(CALIBRATED_FEATURE_SCALE_PPM),
        u32::try_from(best_separation.max(0)).unwrap_or(CALIBRATED_FEATURE_SCALE_PPM),
    )
}

/// Count observations a cut admits, from an ascending population.
fn at_or_above(sorted: &[u64], cut: u32) -> usize {
    let cut = u64::from(cut);
    sorted.len() - sorted.partition_point(|value| *value < cut)
}

fn rate_ppm(part: usize, whole: usize) -> u32 {
    if whole == 0 {
        return 0;
    }
    let scaled = (part as u128) * u128::from(CALIBRATED_FEATURE_SCALE_PPM) / (whole as u128);
    u32::try_from(scaled).unwrap_or(CALIBRATED_FEATURE_SCALE_PPM)
}

fn percentile_ppm(sorted: &[u64], percentile: usize) -> u32 {
    nearest_rank(sorted, percentile)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or_default()
}

/// Name the stratum the cut treats worst, by the share of its labelled
/// positives dropped. Ties break by name so the diagnostic is deterministic.
fn worst_dropped_stratum(
    validation: &[LabelledSemanticScoreV1],
    threshold_ppm: u32,
) -> (String, u32, u32) {
    let mut by_stratum: BTreeMap<&str, (u32, u32)> = BTreeMap::new();
    for score in validation
        .iter()
        .filter(|score| score.relevance == LabelledSemanticRelevanceV1::Positive)
    {
        let dropped = u32::from(score.calibrated_feature_micros < threshold_ppm);
        for stratum in &score.strata {
            let entry = by_stratum.entry(stratum.as_str()).or_insert((0, 0));
            entry.0 += dropped;
            entry.1 += 1;
        }
    }
    // `BTreeMap` iterates in name order and the comparison is strict, so the
    // first stratum reaching the worst rate wins and the diagnostic is stable.
    let mut worst: Option<(&str, u32, u32, u32)> = None;
    for (stratum, (dropped, total)) in by_stratum {
        if dropped == 0 {
            continue;
        }
        let rate = rate_ppm(dropped as usize, total as usize);
        if worst.is_none_or(|(_, _, _, best)| rate > best) {
            worst = Some((stratum, dropped, total, rate));
        }
    }
    worst.map_or_else(
        || ("<unlabelled>".to_owned(), 0, 0),
        |(stratum, dropped, total, _)| (stratum.to_owned(), dropped, total),
    )
}

#[cfg(test)]
mod tests;
