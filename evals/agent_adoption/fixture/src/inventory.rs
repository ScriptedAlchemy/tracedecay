//! Warehouse inventory tracking.

use std::collections::HashMap;

/// Tracks on-hand stock keyed by SKU.
pub struct Warehouse {
    pub on_hand: HashMap<String, u32>,
}

impl Default for Warehouse {
    fn default() -> Self {
        Self::new()
    }
}

impl Warehouse {
    pub fn new() -> Self {
        Warehouse {
            on_hand: HashMap::new(),
        }
    }

    /// Check whether `qty` units of `sku` are currently available.
    pub fn check_availability(&self, sku: &str, qty: u32) -> bool {
        self.on_hand.get(sku).copied().unwrap_or(0) >= qty
    }
}

/// Reserve `qty` units of `sku`, decrementing on-hand stock.
///
/// Returns `Err` when there is not enough stock available. This is the entry
/// point most order flows go through, so it is the natural target for
/// "how does stock reservation work" and caller/impact questions.
pub fn reserve_stock(wh: &mut Warehouse, sku: &str, qty: u32) -> Result<(), String> {
    if !wh.check_availability(sku, qty) {
        return Err(format!("insufficient stock for {sku}"));
    }
    if let Some(count) = wh.on_hand.get_mut(sku) {
        *count -= qty;
    }
    Ok(())
}
