//! Owner-scoped V2 fact-lineage schema installers.

mod automatic_facts;
mod baseline;
mod final_authority;

pub(in crate::db) use baseline::create_schema;
pub(in crate::db) const FINAL_SCHEMA_BATCHES: &[&str] = &[
    baseline::BASELINE_SCHEMA,
    final_authority::FINAL_MEMORY_SUPPORT_SCHEMA,
    automatic_facts::AUTOMATIC_FACT_RECEIPT_INTEGRITY_SCHEMA,
    automatic_facts::CURRENT_PROJECTION_INDEXES_SCHEMA,
];
