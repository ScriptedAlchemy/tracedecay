//! Pure, versioned domain contracts for TraceDecay V2.
//!
//! This crate contains values and validation only. It performs no I/O,
//! persistence, query execution, policy evaluation, host integration, or async work.

pub mod research;

pub use research::*;
