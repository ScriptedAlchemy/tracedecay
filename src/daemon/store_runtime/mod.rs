//! Daemon store-runtime adapters and the canonical shard registry.
//!
//! Production call sites still land behind these modules during S8 cutover, so
//! the registry/compat surfaces remain intentionally constructed from tests and
//! the lifecycle publisher until every live open is routed here.

#![allow(dead_code)] // S8 lands before all daemon call sites route through this registry.

#[allow(dead_code)]
pub(crate) mod driver;
#[allow(dead_code)]
pub(crate) mod global_compat;
#[allow(dead_code)]
pub(crate) mod graph_compat;
#[allow(dead_code)]
pub(crate) mod registry;
#[allow(dead_code)]
pub(crate) mod resolver;
#[allow(dead_code)]
pub(crate) mod rusqlite_parity;
#[allow(dead_code)]
pub(crate) mod shard;
#[allow(dead_code)]
pub(crate) mod telemetry;
