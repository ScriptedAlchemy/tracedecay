//! Durable relational authority for metadata-only semantic-vector staging.
//!
//! Vector values and source content never cross this boundary. A stage records
//! only exact identities, canonical digests, ordered chunk effects, progress,
//! and the publication intent consumed by the verified graph publisher.

#[path = "semantic_vector_staging/manifest.rs"]
mod manifest;
#[path = "semantic_vector_staging/published_generation.rs"]
mod published_generation;
#[path = "semantic_vector_staging/retention.rs"]
mod retention;
#[path = "semantic_vector_staging/store.rs"]
mod store;
#[path = "semantic_vector_staging/types.rs"]
mod types;

pub use manifest::*;
pub use published_generation::*;
pub use retention::*;
pub use store::{
    SemanticVectorPublicationAuthority, SemanticVectorStagingStore,
    SemanticVectorStagingStoreError, SemanticVectorStagingStoreResult,
};
pub use types::*;
