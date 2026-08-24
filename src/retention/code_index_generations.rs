//! Liveness-based retention for immutable code-index generations.
//!
//! Moved to `tracedecay-usecases::retention::code_index_generations`. This is a
//! thin shim so every `crate::retention::code_index_generations::…` path in the
//! root crate keeps resolving after the crate split.

pub use tracedecay_usecases::retention::code_index_generations::*;
