//! Hermes agent integration suite.
//!
//! One test binary for the generated Hermes plugin and LCM bridge.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../common/mod.rs"]
mod common;

mod lcm_bridge;
