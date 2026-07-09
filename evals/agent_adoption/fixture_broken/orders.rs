//! Order placement flow — BROKEN variant for the diagnostics eval.
//!
//! This file replaces `src/orders.rs` in the `fixture_broken` copy. The only
//! difference from the clean version is the planted type error in
//! `place_order`: it passes `item.sku` (a `String`) as the quantity argument
//! to `reserve_stock`, which expects a `u32`. `cargo check` reports
//! "mismatched types: expected `u32`, found `String`" pointing at this call.

use crate::inventory::{reserve_stock, Warehouse};
use crate::pricing::{compute_total, LineItem};

/// A customer order.
pub struct Order {
    pub id: u64,
    pub customer: String,
    pub items: Vec<LineItem>,
}

/// Place an order. Contains a deliberate type error (see module docs).
pub fn place_order(wh: &mut Warehouse, order: &Order) -> Result<u64, String> {
    for item in &order.items {
        // BUG: `item.sku` is a String; reserve_stock's third arg is a u32.
        reserve_stock(wh, &item.sku, item.sku)?;
    }
    Ok(compute_total(&order.items))
}
