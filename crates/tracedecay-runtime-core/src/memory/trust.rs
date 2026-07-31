//! Trust score helpers for bounded confidence and feedback.

use super::types::FeedbackAction;

pub const HELPFUL_DELTA: f64 = 0.05;
pub const UNHELPFUL_DELTA: f64 = -0.10;
pub const TRUST_MIN: f64 = 0.0;
pub const TRUST_MAX: f64 = 1.0;
pub const DEFAULT_TRUST: f64 = 0.5;
pub const DEFAULT_MIN_TRUST: f64 = 0.3;
/// Lower bound of the "high" bucket in [`trust_bucket`]; scores in
/// `[DEFAULT_MIN_TRUST, HIGH_TRUST_THRESHOLD)` are "medium".
pub const HIGH_TRUST_THRESHOLD: f64 = 0.75;
/// Representative score for a "low" trust label, inside the low bucket.
pub const LOW_TRUST_REPRESENTATIVE: f64 = 0.15;
/// Representative score for a "high" trust label, inside the high bucket.
/// `DEFAULT_TRUST` is the representative for "medium".
pub const HIGH_TRUST_REPRESENTATIVE: f64 = 0.85;

pub fn clamp_trust(score: f64) -> f64 {
    score.clamp(TRUST_MIN, TRUST_MAX)
}

pub fn apply_feedback(current_trust: f64, action: FeedbackAction) -> f64 {
    let delta = match action {
        FeedbackAction::Helpful => HELPFUL_DELTA,
        FeedbackAction::Unhelpful => UNHELPFUL_DELTA,
    };

    clamp_trust(current_trust + delta)
}

pub fn trust_bucket(score: f64) -> &'static str {
    let clamped = clamp_trust(score);
    if clamped < DEFAULT_MIN_TRUST {
        "low"
    } else if clamped < HIGH_TRUST_THRESHOLD {
        "medium"
    } else {
        "high"
    }
}

pub fn trust_distribution(scores: &[f64]) -> (usize, usize, usize) {
    scores.iter().fold((0, 0, 0), |(low, medium, high), score| {
        match trust_bucket(*score) {
            "low" => (low + 1, medium, high),
            "medium" => (low, medium + 1, high),
            _ => (low, medium, high + 1),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards the label representative scores against bucket-boundary drift:
    /// each representative must map back onto its own bucket.
    #[test]
    fn label_representatives_map_onto_their_buckets() {
        assert_eq!(trust_bucket(LOW_TRUST_REPRESENTATIVE), "low");
        assert_eq!(trust_bucket(DEFAULT_TRUST), "medium");
        assert_eq!(trust_bucket(HIGH_TRUST_REPRESENTATIVE), "high");
    }
}
