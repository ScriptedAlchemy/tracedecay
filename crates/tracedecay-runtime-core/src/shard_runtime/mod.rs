//! Shard-runtime kernel: the per-shard `SQLite` runtime and its registry.
//!
//! This tree is the substrate under `db::Database`: a database facade is a
//! client lease on a published [`shard::ShardRuntime`], the registry opens,
//! attaches, retires, and closes those runtimes, and `telemetry` projects
//! their inventory. It sits below every store and cannot move above them.
//!
//! It is not the store runtime. Project-store and session lifecycle — which
//! shards a daemon opens, in what order, under which profile authority, and
//! how they converge, retire, and shut down — is owned by
//! `tracedecay-store-runtime`, which drives this registry through its public
//! ports (`registry::StoreRuntimeResolver`, `registry::ShardRuntimePublisher`).

pub mod registry;
pub mod shard;
pub mod telemetry;
mod verified_graph;

pub use verified_graph::{VerifiedGraphRuntimePortV1, VerifiedGraphRuntimeWeakProxyV1};

pub(crate) use crate::tracedecay::saturating_utc_now as utc_now;
