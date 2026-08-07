//! Owner-scoped V2 fact-lineage schema installers.

mod baseline;
mod final_authority;
#[cfg(test)]
mod introspection;
mod proposals;

pub(in crate::db) use baseline::create_schema;
#[cfg(test)]
pub(in crate::db::memory_v2) use introspection::{table_exists, table_has_column};
