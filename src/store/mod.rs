//! Root-crate persistence adapters for store-facing contracts.
//!
//! Adapters in this module borrow already-open authoritative stores. They do
//! not discover paths, open connections, or own transaction state.

pub mod git_correlation;
pub mod global_db;
/// The fact store moved into `tracedecay_runtime_core::store::memory` with the
/// database engine it is built on; the adapters in this module stayed because
/// they borrow `global_db`/`sessions` types that sit above the kernel.
pub use tracedecay_runtime_core::store::memory;
pub mod observation;
pub mod session;
mod session_ingest_authority;
pub(crate) mod vector_generations;
pub mod workflow;

pub use git_correlation::GlobalDbGitCorrelationStore;
pub use global_db::GlobalDbTranscriptStore;
pub use memory::DatabaseFactStore;
pub use observation::GlobalDbObservationStore;
pub use session::{
    GlobalDbSessionTemporalStore, SessionRefreshRecoveryV1, SessionRefreshRestartStateV1,
};
pub(crate) use session_ingest_authority::GlobalDbSessionIngestAuthority;
pub use tracedecay_sessions::runtime::store_port::TranscriptIngestStore;
pub use workflow::GlobalDbWorkflowStore;

/// Typed integration-test surface for the vector-generation state machine.
///
/// This keeps tests on the same nominal projector and store types used by the
/// product without exposing database-engine connections or SQL primitives.
#[doc(hidden)]
pub mod vector_generation_test_support {
    pub use crate::semantic_code::projector::{
        CanonicalChunkVectorEncoderV1, PreparedVectorGenerationV1, ProjectedChunkVectorV1,
        ProjectionRequestBatchV1, SemanticProjectionErrorV1, prepare_vector_generation,
        prepare_vector_generation_async, split_projection_request,
    };

    pub use super::vector_generations::{
        DatabaseVectorGenerationStoreV1, FakeVectorGenerationStoreV1, PublishedVectorGenerationV1,
        VectorGenerationBuildIdV1, VectorGenerationIdV1, VectorGenerationPlanV1,
        VectorGenerationPublicationV1, VectorGenerationStoreErrorV1, VectorProjectionCheckpointV1,
    };

    /// Inject one failure immediately before the oracle's publication swap.
    pub fn fail_before_publication_swap_once(store: &mut FakeVectorGenerationStoreV1) {
        store.fail_before_publication_swap_once();
    }
}
