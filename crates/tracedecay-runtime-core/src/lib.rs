//! Root-free runtime primitives shared by TraceDecay crates.

#![allow(clippy::collapsible_if)]

pub mod branch;
pub mod branch_meta;
pub mod config;
pub mod db;
pub mod errors;
pub mod git;
pub mod lifecycle_lease;
pub mod memory;
pub mod open_store_holders;
pub mod path_scope;
pub mod project_registry;
pub mod redundancy;
pub mod runtime_identity;
pub mod serde_util;
pub mod sqlite_read_snapshot;
pub mod storage;
pub mod sync;
pub mod text;
pub mod timeutil;
pub mod tracedecay;
pub mod types;
pub mod worktree;
