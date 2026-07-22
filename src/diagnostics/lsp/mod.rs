//! Dashboard-owned LSP diagnostics support.

pub mod activity;
pub mod adapters;
pub mod broker;
pub mod client;
pub mod semantic;
pub mod settings;

pub use semantic::{Pr12ProductionSemanticAuthorities, pr12_production_semantic_authorities};
