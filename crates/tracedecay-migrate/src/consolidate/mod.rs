//! Retained SQLite merge primitives for branch memory cutover.
//!
//! The V1→V2 profile-shard consolidation pipeline (`migrate consolidate`,
//! `plan`, `apply`, `verify`, `rollback`, …) was removed with the rest of the
//! cross-version migration surface: V2 stores are created at their final shape,
//! so there are no pre-V2 shards left to merge. What survives here is the
//! memory-merge SQL that [`crate::memory_cutover`] still drives when a tracked
//! branch store is folded back into its project database — a live daemon path,
//! not a migration.

pub mod sqlite;
