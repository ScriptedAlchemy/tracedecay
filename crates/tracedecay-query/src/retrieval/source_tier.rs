//! Source-role ranking: production definitions outrank test-file references.

/// Added to a production hit's raw score so fusion keeps the source tier
/// after calibration saturates. `compare_fused` falls back to raw score
/// when calibrated utility ties.
pub(crate) const PRODUCTION_DEFINITION_TIER_MICROS: u64 = 1_000_000_000;

/// Lift a measured lane score into the production tier, or leave a test
/// reference on the measured scale.
pub(crate) fn source_tiered_score(base_micros: u64, test_reference: bool) -> u64 {
    if test_reference {
        base_micros
    } else {
        base_micros.saturating_add(PRODUCTION_DEFINITION_TIER_MICROS)
    }
}

#[cfg(test)]
mod tests {
    use super::{PRODUCTION_DEFINITION_TIER_MICROS, source_tiered_score};

    #[test]
    fn production_hits_keep_a_fusion_visible_tier_above_equal_test_hits() {
        assert_eq!(source_tiered_score(1_000_000, false), 1_001_000_000);
        assert_eq!(source_tiered_score(1_000_000, true), 1_000_000);
        assert!(
            source_tiered_score(1_000_000, false) > source_tiered_score(2_000_000, true),
            "the production tier outranks a stronger test-file match"
        );
        assert_eq!(
            source_tiered_score(u64::MAX, false),
            u64::MAX,
            "the production lift saturates instead of overflowing"
        );
        assert_eq!(PRODUCTION_DEFINITION_TIER_MICROS, 1_000_000_000);
    }
}
