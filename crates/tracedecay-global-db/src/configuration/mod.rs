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
pub mod migration;
pub mod registry;
pub mod resolver;
pub mod schema;
pub mod semantic;
pub mod store;

pub use migration::{
    CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME, CanonicalGenesisConfigurationV1,
    ConfigurationMigrationError, ConfigurationMigrationOutcomeV1,
    ConfigurationMigrationQuarantineEntryV1, ConfigurationMigrationQuarantineReasonV1,
    ConfigurationMigrationReceiptV1, ConfigurationMigrationStore, LegacyConfigurationEntryV1,
    LegacyConfigurationSourceKindV1, ReadonlyLegacyConfigurationInputV1,
    migrate_legacy_configuration, migrate_legacy_configuration_inputs,
    migrate_legacy_configuration_inputs_with_genesis,
};
pub use schema::ensure_configuration_schema;
pub use schema::{
    ConfigurationSchemaError, TOPOLOGY_POLICY_SCHEMA_VERSION,
    WORK_TOPOLOGY_POLICY_MIGRATION_RECEIPT_NAME,
};
pub use store::{
    ConfigurationStorageError, GlobalDbConfigurationControlStore,
    OwnedGlobalDbConfigurationControlStore,
};
