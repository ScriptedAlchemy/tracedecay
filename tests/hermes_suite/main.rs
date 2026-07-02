//! Hermes agent integration suite.
//!
//! One test binary for the generated Hermes plugin (LCM bridge) and the
//! dashboard plugin-page deployment checks: Windows CI is dominated by
//! per-binary compile+link cost, so both former binaries
//! (`hermes_lcm_bridge_test`, `hermes_dashboard_test`) live here as modules.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../common/mod.rs"]
mod common;

mod dashboard;
mod lcm_bridge;
