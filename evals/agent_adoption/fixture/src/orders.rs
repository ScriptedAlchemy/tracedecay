//! Order placement flow.

use crate::inventory::{reserve_stock, Warehouse};
use crate::pricing::{compute_total, LineItem};

/// A customer order.
pub struct Order {
    pub id: u64,
    pub customer: String,
    pub items: Vec<LineItem>,
}

/// Place an order: reserve stock for every line item, then return the order
/// total in cents. Reserving stock can fail, in which case the error is
/// propagated to the caller.
///
/// `place_order` is the sole caller of [`compute_total`] and one of the
/// callers of [`reserve_stock`], which makes it the ground truth for the
/// call-tracing and impact evals.
pub fn place_order(wh: &mut Warehouse, order: &Order) -> Result<u64, String> {
    for item in &order.items {
        reserve_stock(wh, &item.sku, item.quantity)?;
    }
    Ok(compute_total(&order.items))
}
