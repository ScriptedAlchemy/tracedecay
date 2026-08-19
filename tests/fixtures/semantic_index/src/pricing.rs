//! Bundle pricing for the demo storefront.

use crate::OrderLine;

/// Flat demo price per unit, in cents.
const UNIT_PRICE_CENTS: u64 = 1_250;

/// Ten-percent discount once a bundle reaches this many units.
const BUNDLE_DISCOUNT_THRESHOLD: u64 = 10;

/// Price a reserved bundle in cents, applying the volume discount.
pub fn quote_bundle_price_in_cents(lines: &[OrderLine]) -> u64 {
    let units: u64 = lines.iter().map(|line| u64::from(line.quantity)).sum();
    let gross = units * UNIT_PRICE_CENTS;
    if units >= BUNDLE_DISCOUNT_THRESHOLD {
        gross - gross / 10
    } else {
        gross
    }
}
