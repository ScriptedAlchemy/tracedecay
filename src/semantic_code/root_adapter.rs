//! Explicit root-owned boundary for semantic runtime integration.
//!
//! The semantic implementation can use the extracted domain, query, and code
//! index crates directly. Configuration ownership, application status
//! projection, and the accepted search-evaluation executor remain root-owned,
//! so they cross this single adapter instead of leaking root paths throughout
//! the implementation. The source boundary test keeps that disposition
//! measurable until those contracts gain independent owners.

use std::path::PathBuf;

pub(super) use crate::application::semantic_runtime::{
    SemanticFallbackReasonV1, SemanticRuntimeStateV1,
};
pub(super) use crate::config::retrieval::RerankCompatibilityPinsV1;
pub(super) use crate::config::{DEFAULT_FASTEMBED_MODEL_ID, SemanticResourceCeilings};
pub(super) use crate::search_eval::semantic_native::AdmittedNativeRerankExecutorV1;

pub(super) fn default_lifecycle_root() -> Option<PathBuf> {
    crate::config::user_data_dir().map(|root| root.join("semantic-models"))
}
