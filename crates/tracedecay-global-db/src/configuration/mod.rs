//! Final-shape revisioned configuration persistence.

pub mod contracts;
pub mod registry;
pub mod resolver;
pub mod schema;
pub mod semantic;
pub mod store;

#[cfg(test)]
mod schema_admission_tests;

pub use schema::{
    CONFIGURATION_FORMAT_REVISION, ConfigurationSchemaError, FreshConfigurationStoreEvidence,
    TOPOLOGY_POLICY_SCHEMA_VERSION, admit_configuration_schema, ensure_configuration_schema,
    fresh_configuration_store_evidence,
};
pub use store::{
    ConfigurationStorageError, GlobalDbConfigurationControlStore,
    OwnedGlobalDbConfigurationControlStore,
};
