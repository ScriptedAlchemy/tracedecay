//! Typed, secret-free evidence carried by privacy findings.
//!
//! Assessments are deliberately separate from detector execution: detector
//! findings remain valid even when a score cannot be represented or a claimed
//! calibration is invalid, in which case producers abstain from the optional
//! assessment rather than weakening the sink boundary.

use serde::{Deserialize, Serialize};

use super::detect::DetectionConfidenceV1;

/// The comparison set an ordinal rank is drawn from. An enum rather than a
/// string so a rank can never smuggle out document content.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SanitizationComparisonSetV1 {
    /// Every sensitive field the parse found in one structured document.
    StructuredDocumentFields,
}

/// A deterministic input a rank was computed from.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SanitizationRankComponentV1 {
    KeySemantics,
    ValueLength,
    DecodedValuePattern,
}

/// A named, versioned heuristic scale. Nothing on this scale is a probability
/// and nothing renders as one.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SanitizationHeuristicScaleV1 {
    /// Shannon entropy of the matched token, in bits per character.
    ShannonEntropyBitsPerCharacter,
}

impl SanitizationHeuristicScaleV1 {
    const fn ceiling_per_mille(self) -> u32 {
        match self {
            // log2(256) bits per character.
            Self::ShannonEntropyBitsPerCharacter => 8_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SanitizationScaleRevisionV1 {
    V1,
}

/// The detector population a calibration profile was fitted on.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SanitizationDetectorCohortV1 {
    CredentialPattern,
    EntropyToken,
    SensitiveField,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SanitizationCalibrationDriftV1 {
    Valid,
    Stale,
    Shifted,
    UnderSupported,
}

/// Smallest held-out sample count a calibration profile may claim before its
/// probability output is meaningless.
const MIN_CALIBRATION_SUPPORT: u32 = 384;
/// Largest held-out calibration error a profile may carry, per mille.
const MAX_CALIBRATION_ERROR_PER_MILLE: u32 = 100;

/// A held-out calibration profile. Probability and interval assessments are
/// only admissible with one attached and valid.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SanitizationCalibrationProfileV1 {
    cohort: SanitizationDetectorCohortV1,
    /// Evaluation horizon the profile was fitted over, in whole days.
    horizon_days: u32,
    /// Held-out sample count backing the profile.
    support: u32,
    /// Calibration error on the held-out split, per mille.
    error_per_mille: u32,
    drift: SanitizationCalibrationDriftV1,
}

impl SanitizationCalibrationProfileV1 {
    pub fn new(
        cohort: SanitizationDetectorCohortV1,
        horizon_days: u32,
        support: u32,
        error_per_mille: u32,
        drift: SanitizationCalibrationDriftV1,
    ) -> Self {
        Self {
            cohort,
            horizon_days,
            support,
            error_per_mille,
            drift,
        }
    }

    fn validate(&self) -> Result<(), &'static str> {
        if self.horizon_days == 0 {
            return Err("sanitization calibration profile names no evaluation horizon");
        }
        if self.support < MIN_CALIBRATION_SUPPORT {
            return Err("sanitization calibration profile is under-supported");
        }
        if self.error_per_mille > MAX_CALIBRATION_ERROR_PER_MILLE {
            return Err("sanitization calibration profile error exceeds its admissible bound");
        }
        if self.drift != SanitizationCalibrationDriftV1::Valid {
            return Err("sanitization calibration profile drift is not valid");
        }
        Ok(())
    }
}

/// Optional typed assessment carried by a finding.
///
/// Scores are fixed-point per-mille integers, never floats: findings are
/// sorted, deduplicated, and compared for equality, and a float would make all
/// three unsound while adding no precision a detector can actually justify.
///
/// No variant has a field for matched text, so an assessment cannot leak the
/// value it describes.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SanitizationAssessmentV1 {
    /// Position within a named comparison set, computed from named
    /// deterministic components. Not a score and not comparable across sets.
    OrdinalRank {
        comparison_set: SanitizationComparisonSetV1,
        components: Vec<SanitizationRankComponentV1>,
        rank: u32,
        of: u32,
    },
    /// A value on a named, versioned heuristic scale.
    HeuristicScore {
        scale: SanitizationHeuristicScaleV1,
        scale_revision: SanitizationScaleRevisionV1,
        score_per_mille: u32,
    },
    /// A calibrated probability. Admissible only with a valid held-out profile.
    CalibratedProbability {
        profile: SanitizationCalibrationProfileV1,
        probability_per_mille: u32,
    },
    /// A calibrated interval. Admissible only with a valid held-out profile.
    CalibratedInterval {
        profile: SanitizationCalibrationProfileV1,
        low_per_mille: u32,
        high_per_mille: u32,
    },
}

/// Rejects an assessment that claims more than its evidence supports.
///
/// Producers abstain on failure (the finding keeps its structural evidence and
/// drops the assessment); the wire decoder rejects, so a stale or
/// under-supported calibration can never enter through deserialization.
pub(super) fn validate_assessment(
    confidence: DetectionConfidenceV1,
    assessment: &SanitizationAssessmentV1,
) -> Result<(), &'static str> {
    match assessment {
        SanitizationAssessmentV1::OrdinalRank {
            components,
            rank,
            of,
            ..
        } => {
            if components.is_empty() {
                return Err("sanitization rank names no deterministic components");
            }
            let mut canonical = components.clone();
            canonical.sort();
            canonical.dedup();
            if &canonical != components {
                return Err("sanitization rank components are not canonically ordered");
            }
            if *rank == 0 || *of == 0 || rank > of {
                return Err("sanitization rank falls outside its comparison set");
            }
            Ok(())
        }
        SanitizationAssessmentV1::HeuristicScore {
            scale,
            score_per_mille,
            ..
        } => {
            if confidence != DetectionConfidenceV1::Heuristic {
                return Err("sanitization heuristic score requires a heuristic finding");
            }
            if *score_per_mille > scale.ceiling_per_mille() {
                return Err("sanitization heuristic score exceeds its named scale");
            }
            Ok(())
        }
        SanitizationAssessmentV1::CalibratedProbability {
            profile,
            probability_per_mille,
        } => {
            profile.validate()?;
            if *probability_per_mille > 1_000 {
                return Err("sanitization calibrated probability exceeds unity");
            }
            Ok(())
        }
        SanitizationAssessmentV1::CalibratedInterval {
            profile,
            low_per_mille,
            high_per_mille,
        } => {
            profile.validate()?;
            if low_per_mille > high_per_mille || *high_per_mille > 1_000 {
                return Err("sanitization calibrated interval is not a bounded ordered interval");
            }
            Ok(())
        }
    }
}
