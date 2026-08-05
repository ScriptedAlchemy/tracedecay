//! Final-shape revisioned configuration persistence.

pub mod contracts;
pub mod registry;
pub mod resolver;
pub mod schema;
pub mod semantic;
pub mod store;

pub use schema::{
    CONFIGURATION_FORMAT_REVISION, ConfigurationResetConfirmation,
    ConfigurationResetConfirmationError, ConfigurationSchemaError, TOPOLOGY_POLICY_SCHEMA_VERSION,
    configuration_reset_confirmation, ensure_configuration_schema, reset_configuration_schema,
};
pub use store::{
    ConfigurationStorageError, GlobalDbConfigurationControlStore,
    OwnedGlobalDbConfigurationControlStore,
};
