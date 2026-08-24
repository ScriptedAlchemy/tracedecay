//! Stock ledger for the demo storefront.

use std::collections::BTreeMap;

/// Available and reserved counts per SKU.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InventoryLedger {
    available: BTreeMap<String, u32>,
    reserved: BTreeMap<String, u32>,
}

impl InventoryLedger {
    pub fn with_stock(entries: &[(&str, u32)]) -> Self {
        let mut ledger = Self::default();
        for (sku, count) in entries {
            ledger.available.insert((*sku).to_owned(), *count);
        }
        ledger
    }

    pub fn available(&self, sku: &str) -> u32 {
        self.available.get(sku).copied().unwrap_or(0)
    }

    pub fn release(&mut self, sku: &str) {
        if let Some(held) = self.reserved.remove(sku) {
            *self.available.entry(sku.to_owned()).or_insert(0) += held;
        }
    }
}

/// Move `quantity` units of `sku` from available to reserved.
///
/// Fails without partial effect when the ledger holds fewer units than the
/// checkout asked for.
pub fn reserve_inventory_for_checkout(
    ledger: &mut InventoryLedger,
    sku: &str,
    quantity: u32,
) -> bool {
    let Some(available) = ledger.available.get_mut(sku) else {
        return false;
    };
    if *available < quantity {
        return false;
    }
    *available -= quantity;
    *ledger.reserved.entry(sku.to_owned()).or_insert(0) += quantity;
    true
}
