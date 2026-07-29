//! Root-owned concrete semantic authorities over the crate-owned analyzer runtime.

pub use tracedecay_lsp::analyzer::{activity, adapters, broker, client, settings};

pub mod semantic;

pub use semantic::{
    Pr12ProductionSemanticAuthorities, ProductionSemanticAuthorities,
    pr12_production_semantic_authorities, production_semantic_authorities,
};
