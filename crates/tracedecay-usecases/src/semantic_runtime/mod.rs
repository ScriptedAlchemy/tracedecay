//! Application seam for semantic runtime lifecycle control.
//!
//! This module deliberately does not mount central configuration or Doctor.
//! It consumes the current configuration snapshot and exposes one integration
//! port that those owners can mount later.

mod accepted_profile_authority;
mod bundled_query;
mod config_backend;
mod config_store;
mod configuration_operation;
mod coordinator;
mod fair_scheduler;
mod owner;
mod ports;
mod production;
mod redundancy;

pub(crate) use accepted_profile_authority::{
    RegisteredSemanticAcceptedProfileAuthorityV1, SemanticAcceptedProfileAuthorityErrorV1,
    SemanticAcceptedProfileAuthorityPortV1,
};
pub(crate) use bundled_query::bundled_query_authority;
pub use config_backend::ConfigurationLinkedSemanticRuntimeBackendV1;
pub use config_store::ProductionSemanticRetrievalConfigurationStoreV1;
pub(crate) use configuration_operation::{
    ProductionSemanticConfigurationOperationV1, SemanticEvaluationAuthorityPublicationV1,
    SemanticEvaluationPublicationSnapshotPortV1, SemanticEvaluationPublicationSnapshotV1,
    SemanticProtectedActivationOperationV1, SemanticProtectedRollbackOperationV1,
};
pub use configuration_operation::{
    SemanticEvaluationDiversityCandidateV1, SemanticEvaluationFusionCandidateV1,
    SemanticEvaluationProfileCandidateV1, SemanticEvaluationRerankCandidateV1,
};
pub(crate) use coordinator::{
    ProductionSemanticActivationCoordinatorV1, SemanticActivationCoordinationErrorV1,
};
pub use fair_scheduler::{
    DaemonGlobalSemanticProjectionSchedulerV1, SemanticProjectionBatchV1,
    SemanticProjectionCancellationOutcomeV1, SemanticProjectionDispatchV1,
    SemanticProjectionEnqueueOutcomeV1, SemanticProjectionLeaseV1,
    SemanticProjectionPublicationLeaseV1, SemanticProjectionScheduleErrorV1,
    SemanticProjectionSchedulerConfigErrorV1, SemanticProjectionSchedulerLimitsV1,
    SemanticProjectionSchedulerStatsV1, SemanticProjectionSchedulingPortV1,
};
pub use owner::SemanticRuntimeOwnerV1;
pub use ports::{
    CommittedRetrievalProfileStateV1, RetrievalProfileActivationObserverErrorV1,
    RetrievalProfileActivationObserverV1, SemanticActivationCommandV1, SemanticActivationReceiptV1,
    SemanticActivationRequestV1, SemanticConfigurationBackendErrorV1, SemanticConfigurationPinV1,
    SemanticConfigurationSnapshotSourceV1, SemanticConfigurationTransitionV1,
    SemanticCurrentLinkedActivationV1, SemanticExecutableGenerationV1, SemanticFallbackReasonV1,
    SemanticLinkedTransitionV1, SemanticRetrievalConfigurationPortV1, SemanticRollbackCommandV1,
    SemanticRollbackReceiptV1, SemanticRollbackRequestV1, SemanticRuntimeBackendErrorV1,
    SemanticRuntimeBackendV1, SemanticRuntimeContractErrorV1, SemanticRuntimeControlErrorV1,
    SemanticRuntimeFuture, SemanticRuntimeGenerationInspectorV1, SemanticRuntimeIntegrationPortV1,
    SemanticRuntimeRouteV1, SemanticRuntimeStateV1, SemanticRuntimeStatusV1,
};
pub use production::{
    ProductionProjectSemanticSearchBridgeV1, compose_project_application_semantic_search,
};
// The only in-crate consumers are the feature-gated scheduler tests, so this
// test re-export carries the same feature bound.
#[cfg(all(test, feature = "semantic-fastembed"))]
pub(crate) use production::current_query_factory;
pub(crate) use production::{
    PreparedSemanticEvaluationGenerationV1, ProductionSemanticRuntimeV1,
    SavedCodeGenerationScheduleHookV1, production_saved_generation_schedule_hook,
    project_semantic_application_status, project_semantic_generation_pointer,
    project_semantic_production_runtime, project_semantic_source_generation,
    register_project_semantic_runtime, unregister_project_semantic_runtime,
};
// The only in-crate consumers are the redundancy handler tests, so this
// re-export is test-gated.
pub(crate) use redundancy::{
    SemanticRedundancyGenerationV1, project_semantic_redundancy_generation,
    register_project_semantic_redundancy_authority,
    register_project_semantic_redundancy_generation,
    unregister_project_semantic_redundancy_authority,
    unregister_project_semantic_redundancy_generation,
};
#[cfg(test)]
pub(crate) use redundancy::{SemanticRedundancyProfileV1, SemanticRedundancyVectorV1};

#[cfg(test)]
mod tests;
