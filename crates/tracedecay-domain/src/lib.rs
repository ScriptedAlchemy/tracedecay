//! Pure, versioned domain contracts for TraceDecay V2.
//!
//! This crate contains values and validation only. It performs no I/O,
//! persistence, query execution, policy evaluation, host integration, or async work.

pub mod integration;
pub mod memory;
pub mod observation;
pub mod repository;
pub mod research;

pub use integration::*;
pub use memory::*;
pub use observation::*;
pub use repository::*;
pub use research::*;
