//! Owner-scoped V2 fact-lineage schema installers.
//!
//! Re-exports below preserve every `schema::` path relied on by the rest of
//! `memory_v2`.

mod baseline;
mod final_authority;
mod introspection;
mod proposals;

pub(in crate::db) use baseline::create_schema;
#[cfg(test)]
pub(in crate::db::memory_v2) use introspection::{table_exists, table_has_column};
