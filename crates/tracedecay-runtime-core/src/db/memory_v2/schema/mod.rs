//! Owner-scoped final-V2 fact-lineage schema installers.

mod feedback;
mod install;
#[cfg(test)]
mod introspection;

pub(in crate::db) use install::create_schema;
#[cfg(test)]
pub(in crate::db::memory_v2) use introspection::{table_exists, table_has_column};
