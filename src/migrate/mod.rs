pub mod consolidate;
pub mod hermes;
pub mod inventory;
pub mod manifest;
pub mod memory_cutover;
pub mod registry;

/// Store durability classification, extracted to `tracedecay-migrate` because
/// it decides escalation policy without opening a store. Re-exported here so
/// `crate::migrate::durability` stays the caller path.
pub use tracedecay_migrate::durability;
