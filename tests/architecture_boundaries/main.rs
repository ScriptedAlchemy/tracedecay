//! Final workspace dependency-direction guard.
//!
//! Compile-time package tests and direct journeys protect behavior. This target
//! retains the Cargo dependency check that prevents extracted crates from
//! depending back on the root package, plus a ratchet over the module-level
//! coupling between the MCP surface and the daemon inside the root package.

mod compile_isolation;
mod dependency_direction_ratchet;
