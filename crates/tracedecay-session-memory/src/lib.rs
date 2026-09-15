//! Session retrieval, project memory, and provider usage use cases,
//! extracted from `tracedecay-application` so session-facing adapters
//! (`tracedecay-automation-runtime`, `tracedecay-host-admission`,
//! `tracedecay-mcp`, `tracedecay-cli`) compile without the advisory,
//! semantic-runtime, and code-index surfaces that crate carries.
//!
//! This crate must never depend on `tracedecay-application`,
//! `tracedecay-semantic`, `tracedecay-code-index`, `tracedecay-search-eval`,
//! or `tracedecay-lsp` — that boundary is the point of the extraction.

pub mod anchor_resolution;
pub mod context;
pub mod event_lane;
pub mod external_source_store;
pub mod fact_store;
pub mod memory;
pub mod memory_mapping;
pub mod memory_mutation;
pub mod memory_tracking;
pub mod monitor_ring;
pub mod observability_store;
pub mod provider_pricing;
pub mod provider_usage;
pub mod response_handles;
pub mod session;
pub mod transcript;
pub mod user_config;
