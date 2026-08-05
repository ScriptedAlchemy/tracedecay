//! Owner-scoped V2 fact-lineage schema installers.
//!
//! Re-exports below preserve every `schema::` path relied on by the rest of
//! `memory_v2`.

mod baseline;
mod compatibility;
mod final_shape;
mod introspection;
mod proposals;

pub(in crate::db) use baseline::create_schema;
pub(in crate::db) use final_shape::install_final_shape;
#[cfg(test)]
pub(in crate::db::memory_v2) use introspection::{table_exists, table_has_column};
