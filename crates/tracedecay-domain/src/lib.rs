//! Pure, versioned domain contracts for TraceDecay V2.
//!
//! This crate contains values and validation only. It performs no I/O,
//! persistence, query execution, policy evaluation, host integration, or async work.

pub mod canonical_text;
pub mod code_intelligence;
pub mod configuration;
pub mod diagnostics;
pub mod external_source;
pub mod feedback;
pub mod framed_log;
pub mod git;
pub mod integration;
pub mod memory;
pub mod multi_root;
pub mod observability;
pub mod observation;
pub mod repository;
pub mod research;
pub mod retrieval;
pub mod session;
pub mod session_derived;
pub mod work;
pub mod work_read;
pub mod work_runtime;
pub mod workflow;

pub use code_intelligence::*;
pub use configuration::*;
pub use diagnostics::*;
pub use external_source::*;
pub use feedback::*;
pub use framed_log::*;
pub use git::*;
pub use integration::*;
pub use memory::*;
pub use multi_root::*;
pub use observability::*;
pub use observation::*;
pub use repository::*;
pub use research::*;
pub use retrieval::*;
pub use session::*;
pub use session_derived::*;
pub use work::*;
pub use work_read::*;
pub use work_runtime::*;
pub use workflow::*;
