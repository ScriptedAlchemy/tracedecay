//! Demo storefront indexed by the isolated semantic embed/index fixture
//! check; its distinctive symbols give every retrieval lane a clear target.

pub mod inventory;
pub mod pricing;

pub use inventory::reserve_inventory_for_checkout;
pub use pricing::quote_bundle_price_in_cents;

/// One storefront order line the demo journey reserves and prices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderLine {
    pub sku: String,
    pub quantity: u32,
}

/// Reserve stock for every line, then price the reserved bundle.
///
/// Returns `None` when any line cannot be reserved, leaving previously
/// reserved lines released — the demo models an all-or-nothing checkout.
pub fn checkout_order(
    stock: &mut inventory::InventoryLedger,
    lines: &[OrderLine],
) -> Option<u64> {
    let mut reserved = Vec::with_capacity(lines.len());
    for line in lines {
        if !reserve_inventory_for_checkout(stock, &line.sku, line.quantity) {
            for undone in &reserved {
                stock.release(undone);
            }
            return None;
        }
        reserved.push(line.sku.clone());
    }
    Some(quote_bundle_price_in_cents(lines))
}
