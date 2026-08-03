//! Storage migration logic with root-facing compatibility adapters.

#![allow(clippy::collapsible_if)]

pub use tracedecay_runtime_core::{
    branch, branch_meta, config, db, errors, lifecycle_lease, memory, open_store_holders,
    sqlite_read_snapshot, storage, tracedecay, worktree,
};

pub mod consolidate;
pub mod hermes;
pub mod inventory;
pub mod manifest;
pub mod registry;
pub mod registry_adapter;
