//! Trust score helpers for bounded confidence and feedback.

pub const TRUST_MIN: f64 = 0.0;
pub const TRUST_MAX: f64 = 1.0;
pub const DEFAULT_TRUST: f64 = 0.5;
pub const DEFAULT_MIN_TRUST: f64 = 0.3;
/// Representative score for a "low" trust label, inside the low bucket.
pub const LOW_TRUST_REPRESENTATIVE: f64 = 0.15;
/// Representative score for a "high" trust label, inside the high bucket.
/// `DEFAULT_TRUST` is the representative for "medium".
pub const HIGH_TRUST_REPRESENTATIVE: f64 = 0.85;
