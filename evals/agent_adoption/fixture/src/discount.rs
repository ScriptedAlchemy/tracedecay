//! Discount policy.

/// Apply a percentage discount to a total in cents.
///
/// Per the 2026-06 pricing review, discounts are capped at 25 percent for all
/// orders; anything larger is clamped down to the cap before it is applied.
pub fn apply_discount(total: u64, percent: u8) -> u64 {
    let capped = percent.min(25) as u64;
    total - (total * capped / 100)
}
