//! Storage-neutral, runtime/store-free code-intelligence contracts for PR9
//! (Plan 25: Code Intelligence Indexing).
//!
//! These values are immutable logical records: no storage rows, no parser
//! acquisition, no runtime, no transport. Implementations live in
//! `src/code_index/` (root modules) and move to `crates/tracedecay-code-index`
//! unchanged only if the Plan 19 extraction gate approves a crate.
//!
//! Ownership: Plan 25 owns these code-specific contracts. Plan 15 owns the
//! shared retrieval kernel (`crate::retrieval`); Plan 35 owns
//! `GenerationDiagnosticV1` (`crate::diagnostics`, pr9/12 packet); Plan 36
//! owns native read-only Git semantics. This module stores only typed
//! references to those contracts.

pub mod identity;
pub mod index;
pub mod language;
pub mod search;

pub use identity::*;
pub use index::*;
pub use language::*;
pub use search::*;
