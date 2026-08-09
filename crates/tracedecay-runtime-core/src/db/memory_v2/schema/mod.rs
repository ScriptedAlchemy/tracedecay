//! Owner-scoped V2 fact-lineage schema installers.

mod automatic_facts;
mod baseline;
mod final_authority;
#[cfg(test)]
mod introspection;

pub(in crate::db) use baseline::create_schema;
pub(in crate::db) const FINAL_SCHEMA_BATCHES: &[&str] = &[
    baseline::BASELINE_SCHEMA,
    final_authority::FINAL_MEMORY_SUPPORT_SCHEMA,
    automatic_facts::AUTOMATIC_FACT_RECEIPT_INTEGRITY_SCHEMA,
    automatic_facts::CURRENT_PROJECTION_INDEXES_SCHEMA,
];
#[cfg(test)]
pub(in crate::db::memory_v2) use introspection::{table_exists, table_has_column};
