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

pub(crate) use accepted_profile_authority::SemanticAcceptedProfileAuthorityPortV1;
pub use accepted_profile_authority::{
    RegisteredSemanticAcceptedProfileAuthorityV1, SemanticAcceptedProfileAuthorityErrorV1,
};
pub use bundled_query::bundled_query_authority;
pub use config_backend::ConfigurationLinkedSemanticRuntimeBackendV1;
pub use config_store::ProductionSemanticRetrievalConfigurationStoreV1;
pub use configuration_operation::{
    ProductionSemanticConfigurationOperationV1, SemanticAppliedActivationV1,
    SemanticAppliedRollbackV1, SemanticEvaluatedProfilePublicationV1,
    SemanticEvaluationAuthorityPublicationV1, SemanticEvaluationDiversityCandidateV1,
    SemanticEvaluationFusionCandidateV1, SemanticEvaluationProfileCandidateV1,
    SemanticEvaluationPublicationSnapshotPortV1, SemanticEvaluationPublicationSnapshotV1,
    SemanticEvaluationRerankCandidateV1, SemanticProtectedActivationOperationV1,
    SemanticProtectedRollbackOperationV1,
};
pub use coordinator::{
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
#[cfg(feature = "semantic-fastembed")]
pub use production::current_query_factory;
pub(crate) use production::project_semantic_generation_pointer;
pub use production::{
    PreparedSemanticEvaluationGenerationV1, ProductionSemanticRuntimeV1,
    SavedCodeGenerationScheduleHookV1, SemanticCompatibleCurrentGenerationSnapshotV1,
    SemanticVectorPublicationLeaseV1, production_saved_generation_schedule_hook,
    project_semantic_application_status, project_semantic_production_runtime,
    project_semantic_source_generation, register_project_semantic_runtime,
    unregister_project_semantic_runtime,
};
pub use production::{
    ProductionProjectSemanticSearchBridgeV1, compose_project_application_semantic_search,
};
pub use redundancy::{
    SemanticRedundancyGenerationV1, SemanticRedundancyProfileV1, SemanticRedundancyVectorV1,
    project_semantic_redundancy_generation, register_project_semantic_redundancy_authority,
    unregister_project_semantic_redundancy_authority,
};
pub(crate) use redundancy::{
    register_project_semantic_redundancy_generation,
    unregister_project_semantic_redundancy_generation,
};

#[cfg(test)]
mod tests;
