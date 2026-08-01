//! Revisioned configuration persistence modules.

pub mod migration;
pub mod schema;
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
