//! Store-free LSP diagnostics support.

pub mod analyzer;

pub use analyzer::{LspError, Result, activity, adapters, broker, client, settings};
