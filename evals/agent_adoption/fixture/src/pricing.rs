//! Order pricing helpers.

/// A single line item in an order. Prices are stored in integer cents.
pub struct LineItem {
    pub sku: String,
    pub unit_price: u64,
    pub quantity: u32,
}

/// Compute the total price (in cents) for a set of line items.
pub fn compute_total(items: &[LineItem]) -> u64 {
    let mut total = 0u64;
    for item in items {
        total += item.unit_price * item.quantity as u64;
    }
    total
}

/// Near-duplicate of [`compute_total`], planted for the duplicate-hunting eval.
///
/// The body is byte-for-byte identical to `compute_total`; only the name
/// differs. A good deduplication pass should flag these two as redundant.
pub fn compute_grand_total(items: &[LineItem]) -> u64 {
    let mut total = 0u64;
    for item in items {
        total += item.unit_price * item.quantity as u64;
    }
    total
}
