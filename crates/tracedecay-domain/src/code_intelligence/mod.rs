//! Storage-neutral, runtime/store-free code-intelligence contracts.
//!
//! These values are immutable logical records: no storage rows, no parser
//! acquisition, no runtime, no transport. Implementations live in
//! `src/code_index/` (root modules) and `crates/tracedecay-code-index`.
//!
//! This module stores only typed references to the shared retrieval kernel
//! (`crate::retrieval`), `GenerationDiagnosticV1` (`crate::diagnostics`),
//! and native read-only Git semantics.

pub mod graph;
pub mod identity;
pub mod index;
pub mod language;
pub mod search;
pub mod token_grammar;
mod vector_contract;

pub use graph::*;
pub use identity::*;
pub use index::*;
pub use language::*;
pub use search::*;
pub use token_grammar::*;
pub use vector_contract::*;
