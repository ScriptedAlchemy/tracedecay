//! Owner-scoped materialized compatibility-bank writers.

mod compatibility_bank;

pub(in crate::db) use compatibility_bank::{
    clear_memory_v2_compatibility_bank_dirty_in_transaction,
    delete_memory_v2_compatibility_bank_in_transaction,
    mark_memory_v2_compatibility_bank_dirty_in_transaction,
    upsert_memory_v2_compatibility_bank_in_transaction,
};
