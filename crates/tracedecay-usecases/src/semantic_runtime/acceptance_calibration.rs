//! Measured semantic acceptance calibration for one committed vector
//! generation.
//!
//! The semantic lane abstains when the best candidate's cosine distance
//! exceeds `SemanticCalibrationProfileV1::maximum_distance_micros`. That bound
//! used to be a hard-coded constant, which is wrong for the same reason any
//! guessed limit is wrong: a cosine cut-off is a property of the *model* and
//! the *corpus*, not of the source code that gates on it. A number tuned for
//! one embedding model silently drops every candidate under another, and the
//! hybrid lane then degrades to the lexical baseline with no signal that it
//! did.
//!
//! This module derives the bound from the committed generation instead. The
//! generation is immutable, so the derivation is a pure, deterministic
//! function of its vectors and both the proposing and the certifying side
//! recompute the identical value.
//!
//! # What is measured
//!
//! We sample the corpus's own *background* distribution: the cosine distance
//! between pairs of unrelated chunks in the committed generation. That
//! distribution states, in the model's own units, how far apart two points of
//! this corpus actually sit. The acceptance bound is its upper tail
//! ([`ACCEPTANCE_PERCENTILE_NUMERATOR`]/[`ACCEPTANCE_PERCENTILE_DENOMINATOR`]),
//! so a result is rejected only when it is farther away than essentially every
//! corpus-internal pair — a match that carries no information relative to the
//! spread the model produces on this data.
//!
//! # Why the upper tail, and the gap it works around
//!
//! A tighter cut (say the 1st percentile of background) would be the natural
//! choice for a "better than chance" test, and it is deliberately *not* what
//! this does. The background here is a code↔code distribution, while the gate
//! it feeds runs on natural-language↔code queries, and those live in different
//! score regimes: code chunks under a code embedding model cluster tightly
//! against each other, whereas an NL query's best genuine match sits much
//! farther out. Cutting at a low percentile of the code↔code background would
//! therefore reject correct NL results — reintroducing exactly the silent
//! whole-lane drop this replaces.
//!
//! The honest statement of the limitation: the committed generation carries no
//! query-side vectors and no labelled positive pairs, so no in-regime positive
//! distribution can be measured from it today. The upper tail is the strongest
//! bound derivable from what is actually committed. When the evaluation
//! workload grows labelled NL→code positives, the bound should be re-derived
//! from that positive-pair distribution and this module is where that change
//! belongs.

use std::collections::BTreeMap;

use tracedecay_domain::CodeSearchChunkId;
use tracedecay_semantic::projector::ProjectedChunkVectorV1;

/// Fixed-point scale shared with the redundancy tier's distance encoding.
const SEMANTIC_DISTANCE_SCALE: f64 = 1_000_000_000.0;

/// Distance encoding of cosine `-1.0`, i.e. "admit every candidate".
///
/// This is also the inclusive upper bound the redundancy authority accepts for
/// a committed calibration, so every value this module produces stays inside
/// the range that tier validates.
pub const MAX_COSINE_DISTANCE_MICROS: i64 = 2_000_000_000;

/// Conservative bound used when the generation cannot support a measurement.
///
/// Admitting everything keeps an unmeasured generation behaving like the
/// lexical-plus-semantic union rather than silently collapsing to lexical
/// only. Abstention stays the job of the measured bound, never of a missing
/// one.
pub const UNCALIBRATED_MAXIMUM_DISTANCE_MICROS: i64 = MAX_COSINE_DISTANCE_MICROS;

/// Background pairs sampled from the committed generation.
const BACKGROUND_SAMPLE_PAIRS: usize = 4_096;

/// Vectors required before a background distribution means anything.
const MINIMUM_CALIBRATION_VECTORS: usize = 32;

/// Usable sampled pairs required before a percentile means anything.
const MINIMUM_BACKGROUND_SAMPLES: usize = 256;

/// Upper-tail percentile of the background distance distribution.
const ACCEPTANCE_PERCENTILE_NUMERATOR: usize = 99;
const ACCEPTANCE_PERCENTILE_DENOMINATOR: usize = 100;

/// Odd strides that walk the canonical vector order without short cycles.
const BACKGROUND_STRIDE_A: usize = 2_654_435_761;
const BACKGROUND_STRIDE_B: usize = 40_503;

/// Outcome of measuring one committed generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SemanticAcceptanceCalibrationV1 {
    /// Measured acceptance bound, in the shared fixed-point distance scale.
    pub maximum_distance_micros: i64,
    /// Background pairs that produced a usable distance.
    pub sampled_pairs: usize,
    /// Whether `maximum_distance_micros` came from a measurement.
    pub measured: bool,
}

impl SemanticAcceptanceCalibrationV1 {
    /// The conservative admit-everything outcome for an unmeasurable
    /// generation.
    pub const fn uncalibrated() -> Self {
        Self {
            maximum_distance_micros: UNCALIBRATED_MAXIMUM_DISTANCE_MICROS,
            sampled_pairs: 0,
            measured: false,
        }
    }
}

/// Measure the acceptance bound for one committed generation's vectors.
///
/// Deterministic in the generation: the same immutable vector map always
/// yields the same bound, so a proposing writer and a certifying reader agree
/// without persisting the statistic.
#[hotpath::measure(label = "usecases.semantic.acceptance_calibration")]
pub fn measure_acceptance_calibration(
    vectors: &BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1>,
) -> SemanticAcceptanceCalibrationV1 {
    let values = vectors
        .values()
        .map(|vector| vector.values.as_slice())
        .collect::<Vec<_>>();
    measure_acceptance_calibration_from_values(&values)
}

/// Measure the acceptance bound from canonically ordered vector values.
#[hotpath::measure(label = "usecases.semantic.acceptance_calibration_values")]
pub fn measure_acceptance_calibration_from_values(
    values: &[&[f32]],
) -> SemanticAcceptanceCalibrationV1 {
    let count = values.len();
    if count < MINIMUM_CALIBRATION_VECTORS {
        return SemanticAcceptanceCalibrationV1::uncalibrated();
    }

    let mut distances = Vec::with_capacity(BACKGROUND_SAMPLE_PAIRS);
    for index in 0..BACKGROUND_SAMPLE_PAIRS {
        let left = index.wrapping_mul(BACKGROUND_STRIDE_A) % count;
        // `1 + (… % (count - 1))` lands in `1..count`, so the offset can never
        // pair a vector with itself.
        let offset = 1 + index.wrapping_mul(BACKGROUND_STRIDE_B) % (count - 1);
        let right = (left + offset) % count;
        if let Some(distance) = background_distance_micros(values[left], values[right]) {
            distances.push(distance);
        }
    }

    if distances.len() < MINIMUM_BACKGROUND_SAMPLES {
        return SemanticAcceptanceCalibrationV1::uncalibrated();
    }

    distances.sort_unstable();
    let sampled_pairs = distances.len();
    // Nearest-rank upper tail over the sorted sample.
    let rank = (sampled_pairs - 1).saturating_mul(ACCEPTANCE_PERCENTILE_NUMERATOR)
        / ACCEPTANCE_PERCENTILE_DENOMINATOR;
    let maximum_distance_micros = distances[rank].clamp(0, MAX_COSINE_DISTANCE_MICROS);

    SemanticAcceptanceCalibrationV1 {
        maximum_distance_micros,
        sampled_pairs,
        measured: true,
    }
}

/// Fixed-point cosine distance between two vectors, or `None` when the pair
/// cannot produce one.
fn background_distance_micros(left: &[f32], right: &[f32]) -> Option<i64> {
    let cosine = cosine_similarity(left, right)?;
    let scaled = ((1.0 - cosine) * SEMANTIC_DISTANCE_SCALE).round();
    (scaled.is_finite() && scaled >= 0.0 && scaled <= MAX_COSINE_DISTANCE_MICROS as f64)
        .then_some(scaled as i64)
}

/// Cosine similarity in the same shape the redundancy tier scores pairs with.
fn cosine_similarity(left: &[f32], right: &[f32]) -> Option<f64> {
    if left.is_empty() || left.len() != right.len() {
        return None;
    }
    let mut dot = 0.0_f64;
    let mut left_norm = 0.0_f64;
    let mut right_norm = 0.0_f64;
    for (&left, &right) in left.iter().zip(right) {
        if !left.is_finite() || !right.is_finite() {
            return None;
        }
        let left = f64::from(left);
        let right = f64::from(right);
        dot += left * right;
        left_norm += left * left;
        right_norm += right * right;
    }
    (left_norm > 0.0 && right_norm > 0.0)
        .then(|| (dot / (left_norm.sqrt() * right_norm.sqrt())).clamp(-1.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spread_corpus(count: usize, dimension: usize) -> Vec<Vec<f32>> {
        (0..count)
            .map(|index| {
                (0..dimension)
                    .map(|axis| {
                        let seed = (index * 31 + axis * 17) % 97;
                        (seed as f32 / 97.0) - 0.5
                    })
                    .collect()
            })
            .collect()
    }

    fn borrow(values: &[Vec<f32>]) -> Vec<&[f32]> {
        values.iter().map(Vec::as_slice).collect()
    }

    #[test]
    fn too_few_vectors_fall_back_to_admitting_everything() {
        let corpus = spread_corpus(MINIMUM_CALIBRATION_VECTORS - 1, 8);
        let measurement = measure_acceptance_calibration_from_values(&borrow(&corpus));
        assert!(!measurement.measured);
        assert_eq!(
            measurement.maximum_distance_micros,
            UNCALIBRATED_MAXIMUM_DISTANCE_MICROS
        );
    }

    #[test]
    fn degenerate_vectors_fall_back_to_admitting_everything() {
        let corpus = vec![vec![0.0_f32; 8]; 128];
        let measurement = measure_acceptance_calibration_from_values(&borrow(&corpus));
        assert!(!measurement.measured);
        assert_eq!(
            measurement.maximum_distance_micros,
            UNCALIBRATED_MAXIMUM_DISTANCE_MICROS
        );
    }

    #[test]
    fn measured_bound_stays_inside_the_redundancy_accepted_range() {
        let corpus = spread_corpus(512, 16);
        let measurement = measure_acceptance_calibration_from_values(&borrow(&corpus));
        assert!(measurement.measured);
        assert!(measurement.sampled_pairs >= MINIMUM_BACKGROUND_SAMPLES);
        assert!((0..=MAX_COSINE_DISTANCE_MICROS).contains(&measurement.maximum_distance_micros));
    }

    #[test]
    fn measurement_is_deterministic_for_one_generation() {
        let corpus = spread_corpus(512, 16);
        let first = measure_acceptance_calibration_from_values(&borrow(&corpus));
        let second = measure_acceptance_calibration_from_values(&borrow(&corpus));
        assert_eq!(first, second);
    }

    #[test]
    fn a_tighter_corpus_measures_a_tighter_bound() {
        // Every vector near the same direction: the background spread is small,
        // so the measured bound must be smaller than a widely spread corpus's.
        let tight = (0..512)
            .map(|index| {
                let mut values = vec![0.01_f32; 16];
                values[0] = 1.0;
                values[index % 16] += 0.02;
                values
            })
            .collect::<Vec<_>>();
        let tight = measure_acceptance_calibration_from_values(&borrow(&tight));
        let spread = spread_corpus(512, 16);
        let spread = measure_acceptance_calibration_from_values(&borrow(&spread));
        assert!(tight.measured && spread.measured);
        assert!(
            tight.maximum_distance_micros < spread.maximum_distance_micros,
            "tight={} spread={}",
            tight.maximum_distance_micros,
            spread.maximum_distance_micros
        );
    }

    #[test]
    fn the_bound_admits_the_typical_background_pair() {
        // The upper tail must not reject the corpus's own ordinary pairs.
        let corpus = spread_corpus(512, 16);
        let borrowed = borrow(&corpus);
        let measurement = measure_acceptance_calibration_from_values(&borrowed);
        let admitted = borrowed
            .iter()
            .enumerate()
            .filter_map(|(index, left)| {
                background_distance_micros(left, borrowed[(index + 1) % borrowed.len()])
            })
            .filter(|distance| *distance <= measurement.maximum_distance_micros)
            .count();
        assert!(
            admitted * 2 > borrowed.len(),
            "measured bound rejected most background pairs: {admitted}/{}",
            borrowed.len()
        );
    }
}
