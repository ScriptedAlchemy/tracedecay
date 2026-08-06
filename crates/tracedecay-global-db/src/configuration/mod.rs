//! Canonical revisioned configuration authority.
//!
//! The registry, resolver, schema, and control store form one durable boundary.
//! Fresh project stores begin at an exact genesis snapshot; incompatible stores
//! require explicit reset rather than conversion or repair.

pub mod contracts;
pub mod genesis;
pub mod registry;
pub mod resolver;
pub mod schema;
pub mod semantic;
pub mod store;

pub use genesis::{ConfigurationGenesisError, resolve_project_genesis};
pub use schema::{ConfigurationSchemaError, TOPOLOGY_POLICY_SCHEMA_VERSION};
pub use schema::{ensure_configuration_schema, validate_configuration_schema};
pub use store::{GlobalDbConfigurationControlStore, OwnedGlobalDbConfigurationControlStore};
