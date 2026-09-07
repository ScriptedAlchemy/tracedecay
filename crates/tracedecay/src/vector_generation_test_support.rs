//! Typed integration-test surface for the vector-generation state machine.
//!
//! This keeps tests on the same nominal projector and store types used by the
//! product without exposing database-engine connections or SQL primitives.

pub use tracedecay_semantic::projector::{
    CanonicalChunkVectorEncoderV1, PreparedVectorGenerationV1, ProjectedChunkVectorV1,
    ProjectionRequestBatchV1, SemanticProjectionErrorV1, prepare_vector_generation,
    prepare_vector_generation_async, split_projection_request,
};

pub use tracedecay_usecases::store::vector_generations::{
    GraphVectorGenerationStoreV1, PublishedVectorGenerationV1, VectorGenerationBuildIdV1,
    VectorGenerationIdV1, VectorGenerationPlanV1, VectorGenerationPublicationV1,
    VectorGenerationStateMachineV1, VectorGenerationStoreErrorV1, VectorProjectionCheckpointV1,
};
