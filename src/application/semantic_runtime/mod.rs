//! Application seam for semantic runtime lifecycle control.
//!
//! This module deliberately does not mount central configuration or Doctor.
//! It consumes the current configuration snapshot and exposes one integration
//! port that those owners can mount later.

mod owner;
mod ports;
mod production;

pub use owner::SemanticRuntimeOwnerV1;
pub use ports::{
    SemanticActivationCommandV1, SemanticActivationReceiptV1, SemanticActivationRequestV1,
    SemanticConfigurationPinV1, SemanticConfigurationSnapshotSourceV1, SemanticFallbackReasonV1,
    SemanticRollbackCommandV1, SemanticRollbackReceiptV1, SemanticRollbackRequestV1,
    SemanticRuntimeBackendErrorV1, SemanticRuntimeBackendV1, SemanticRuntimeContractErrorV1,
    SemanticRuntimeControlErrorV1, SemanticRuntimeFuture, SemanticRuntimeIntegrationPortV1,
    SemanticRuntimeRouteV1, SemanticRuntimeStateV1, SemanticRuntimeStatusV1,
};
pub use production::{
    DaemonSemanticRuntimeBackendV1, SavedCodeGenerationScheduleHookV1,
    application_status_from_projection, current_query_factory,
    production_saved_generation_schedule_hook, project_semantic_application_status,
    register_project_semantic_runtime, saved_generation_schedule_hook,
    schedule_saved_code_generation, semantic_lane_readiness_for_request,
    unregister_project_semantic_runtime,
};

#[cfg(test)]
mod tests;
