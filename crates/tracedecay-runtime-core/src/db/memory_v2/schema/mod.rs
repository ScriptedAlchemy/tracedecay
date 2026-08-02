//! Owner-scoped V2 fact-lineage schema installers and upgrades.
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
pub(in crate::db::memory_v2) use introspection::{
    proposal_schema_is_v22, table_exists, table_has_column,
};
pub(in crate::db) use upgrades::{
    install_v22_fresh_schema, install_v23_fresh_schema, upgrade_v20_schema, upgrade_v21_schema,
    upgrade_v22_schema, upgrade_v23_schema,
};
