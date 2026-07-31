//! Owner-scoped V2 fact-lineage row writers.
//!
//! Re-exports below preserve every `writers::` path relied on by the rest of
//! `memory_v2`.

mod compatibility_bank;
mod lineage;
mod purge;

pub(in crate::db) use compatibility_bank::{
    clear_memory_v2_compatibility_bank_dirty_in_transaction,
    delete_memory_v2_compatibility_bank_in_transaction,
    mark_memory_v2_compatibility_bank_dirty_in_transaction,
    upsert_memory_v2_compatibility_bank_in_transaction,
};
pub(in crate::db::memory_v2) use lineage::{
    ensure_current, insert_assertion, insert_event, insert_fact_identity, insert_feedback_history,
    insert_legacy_feedback_event_mapping, insert_mapping, legacy_feedback_mapping_can_be_recorded,
    update_current,
};
pub(crate) use purge::MemoryV2LegacyPurgeReceipt;
#[cfg(test)]
pub(in crate::db) use purge::purge_memory_v2_fact;
pub(in crate::db) use purge::purge_memory_v2_fact_in_transaction;
#[cfg(test)]
pub(in crate::db::memory_v2) use purge::purge_payload_rows;
pub(in crate::db::memory_v2) use purge::{
    PurgeIntent, insert_quarantine, purge_memory_v2_fact_inner, quarantine_fact,
};
