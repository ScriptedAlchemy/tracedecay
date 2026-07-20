//! Pure, versioned domain contracts for TraceDecay V2.
//!
//! This crate contains values and validation only. It performs no I/O,
//! persistence, query execution, policy evaluation, host integration, or async work.

pub mod code_intelligence;
pub mod diagnostics;
pub mod evaluation;
pub mod git;
pub mod integration;
pub mod memory;
pub mod observation;
pub mod repository;
pub mod research;
pub mod retrieval;
pub mod session;

pub use code_intelligence::*;
pub use diagnostics::*;
pub use evaluation::*;
pub use git::*;
pub use integration::*;
pub use memory::*;
pub use observation::*;
pub use repository::*;
pub use research::*;
pub use retrieval::*;
pub use session::*;
