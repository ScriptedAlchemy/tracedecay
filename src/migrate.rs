//! Compatibility shim for the migration subsystem.
//!
//! The whole subsystem now lives in `tracedecay-migrate`. This module re-exports
//! it so every existing `crate::migrate::*` and `tracedecay::migrate::*` caller
//! path keeps resolving after the one-shot crate split.

pub use tracedecay_migrate::{
    consolidate, durability, final_v2, hermes, inventory, memory_cutover, profile_backup, registry,
};
