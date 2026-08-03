//! Immutable semantic vector-generation storage.
//!
//! The deterministic state machine is retained as a test oracle. Production
//! persistence stores that same state in the already-open project database,
//! using a revisioned compare-and-swap so generation publication and the
//! active pointer become visible together. No separate vector database or
//! approximate index is introduced.
#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{Arc, Mutex, Weak},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, CodeGenerationId, CodeSearchChunkId, ContentDigest,
    ManifestDigest, ProjectionBatchReceiptV1, ProjectionKeyV1, ProjectionKindV1,
    ProjectionOperationV1, ProjectionOutcomeV1, canonical_sha256,
};

pub use tracedecay_domain::VectorGenerationIdV1;

use tracedecay_code_index::projection::{expected_publication_digest, verify_batch_receipt};
use tracedecay_runtime_core::db::{Database, engine::params};
use tracedecay_runtime_core::sqlite_read_snapshot::{
    BOUNDED_PROBE_BUSY_TIMEOUT, open_read_only_probe,
};
use tracedecay_semantic::projector::{
    PreparedVectorGenerationV1, ProjectedChunkVectorV1, SemanticProjectionErrorV1,
};

include!("vector_generations/contracts.rs");
include!("vector_generations/physical_pool.rs");
include!("vector_generations/external_collections.rs");
include!("vector_generations/externalized_vectors.rs");
include!("vector_generations/generation_state.rs");
include!("vector_generations/oracle.rs");
include!("vector_generations/database_contracts.rs");
include!("vector_generations/database_records.rs");
include!("vector_generations/database_gc.rs");
include!("vector_generations/database.rs");
include!("vector_generations/evaluation.rs");
include!("vector_generations/state_validation.rs");
include!("vector_generations/external_storage.rs");
include!("vector_generations/validation.rs");

#[cfg(test)]
mod tests;
