//! Trust score helpers for bounded confidence, feedback, and aging.

use super::types::FeedbackAction;

pub const HELPFUL_DELTA: f64 = 0.05;
pub const UNHELPFUL_DELTA: f64 = -0.10;
pub const TRUST_MIN: f64 = 0.0;
pub const TRUST_MAX: f64 = 1.0;
pub const DEFAULT_TRUST: f64 = 0.5;
pub const DEFAULT_MIN_TRUST: f64 = 0.3;

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
    } else if clamped < 0.75 {
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

pub fn temporal_decay(current_trust: f64, age_days: f64) -> f64 {
    let age = age_days.max(0.0);
    let decay_weight = 1.0 - (-age / 180.0).exp();
    let decayed = current_trust.mul_add(1.0 - decay_weight, DEFAULT_TRUST * decay_weight);

    clamp_trust(decayed)
}
