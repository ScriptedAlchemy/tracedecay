//! Owner-scoped V2 fact-lineage schema installers.
//!
//! Re-exports below preserve every `schema::` path relied on by the rest of
//! `memory_v2`.

mod baseline;
mod compatibility;
mod introspection;
mod proposals;
mod upgrades;

pub(in crate::db) use baseline::create_schema;
#[cfg(test)]
pub(in crate::db::memory_v2) use introspection::{table_exists, table_has_column};
pub(in crate::db) use upgrades::{install_v22_fresh_schema, install_v23_fresh_schema};
