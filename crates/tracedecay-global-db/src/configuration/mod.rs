//! Revisioned configuration persistence modules.
//!
//! `contracts`, `registry`, `resolver`, and `semantic` were moved down here
//! from root (`src/application/configuration/{types,ports}.rs`,
//! `src/config/{registry,resolver}.rs`, and the semantic block of
//! `src/config.rs`). The configuration control store in this crate is the only
//! durable implementation of the contract and the only caller of the registry
//! and resolver, while root `src/application/` is staying at the top of the
//! stack and already depends on `global_db` — so keeping them above this crate
//! was a cycle. Root re-exports them through its `tracedecay_global_db::*`
//! shim.

pub mod contracts;
mod genesis;
pub mod registry;
pub mod resolver;
pub mod schema;
pub mod semantic;
pub mod store;

pub use genesis::CanonicalGenesisConfigurationV1;
pub use schema::ensure_configuration_schema;
pub use schema::{ConfigurationSchemaError, TOPOLOGY_POLICY_SCHEMA_VERSION};
pub use store::{
    ConfigurationStorageError, GlobalDbConfigurationControlStore,
    OwnedGlobalDbConfigurationControlStore,
};
