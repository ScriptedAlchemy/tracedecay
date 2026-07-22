//! Crash-safe consolidation from the last released schema into the frozen V2 schema.
//!
//! This module is deliberately not wired into runtime startup. It operates only
//! through copy/staging ports and cannot discover a profile or infer identity
//! from a path. Exactly one release plan becomes constructible after an external
//! authority accepts the release freeze proof.

// The daemon adapter is intentionally not wired yet. Keep the closed,
// crate-private cutover facade compiled and tested without pretending it is a
// live migration registration.
#![allow(dead_code)]

mod engine;
mod model;
mod ports;

#[allow(unused_imports)]
pub(crate) use engine::ConsolidatedMigrationEngine;
#[allow(unused_imports)]
pub(crate) use model::*;
#[allow(unused_imports)]
pub(crate) use ports::{ConsolidatedMigrationPort, FamilyTransform};

#[cfg(test)]
mod tests;
