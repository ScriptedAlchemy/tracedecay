//! Owner-scoped V2 fact-lineage row writers.
//!
//! Re-exports below preserve every `writers::` path relied on by the rest of
//! `memory_v2`.

mod compatibility_bank;
mod lineage;
mod purge;

pub(in crate::db::memory_v2) use lineage::insert_event;

pub(in crate::db) use compatibility_bank::{
    clear_memory_v2_compatibility_bank_dirty_in_transaction,
    delete_memory_v2_compatibility_bank_in_transaction,
    mark_memory_v2_compatibility_bank_dirty_in_transaction,
    upsert_memory_v2_compatibility_bank_in_transaction,
};
pub(crate) use purge::MemoryV2LegacyPurgeReceipt;
#[cfg(test)]
pub(in crate::db) use purge::purge_memory_v2_fact;
pub(in crate::db) use purge::purge_memory_v2_fact_in_transaction;
