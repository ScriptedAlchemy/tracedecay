#![forbid(unsafe_code)]

#[path = "../src/semantic_code/artifact_store.rs"]
mod artifact_store;
#[path = "../src/semantic_code/fastembed_adapter.rs"]
mod fastembed_adapter;
#[path = "../src/semantic_code/manifest.rs"]
mod manifest;
#[path = "../src/semantic_code/model_catalog.rs"]
pub mod model_catalog;
mod root_adapter {
    pub(super) use tracedecay::config::{DEFAULT_FASTEMBED_MODEL_ID, SemanticResourceCeilings};
}
#[path = "../src/semantic_code/runtime_query.rs"]
mod runtime_query;
#[path = "../src/semantic_code/runtime_service.rs"]
mod runtime_service;
#[path = "../src/semantic_code/session_pool.rs"]
mod session_pool;

pub mod query {
    pub use tracedecay::query::*;
}

// The included sources resolve root-owned paths against this test crate, so
// mirror those paths onto the real lib module and the path-included copy.
pub mod config {
    pub use tracedecay::config::*;
}

pub mod semantic_code {
    pub use crate::model_catalog;
}
