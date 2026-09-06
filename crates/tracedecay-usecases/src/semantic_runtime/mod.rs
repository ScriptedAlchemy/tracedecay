//! Application seam for semantic runtime lifecycle control.
//!
//! This module deliberately does not mount central configuration or Doctor.
//! It consumes the current configuration snapshot and exposes one integration
//! port that those owners can mount later.

mod acceptance_calibration;
mod accepted_profile_authority;
mod config_backend;
mod config_inventory;
mod config_store;
mod configuration_operation;
mod configuration_runtime;
mod coordinator;
mod fair_scheduler;
mod graph_provider;
mod owner;
mod ports;
mod production;
mod publish_failure_memo;
mod redundancy;
mod retention;

pub use acceptance_calibration::{
    MAX_COSINE_DISTANCE_MICROS, SemanticAcceptanceCalibrationV1,
    UNCALIBRATED_MAXIMUM_DISTANCE_MICROS, measure_acceptance_calibration,
    measure_acceptance_calibration_from_values,
};
pub(crate) use accepted_profile_authority::SemanticAcceptedProfileAuthorityPortV1;
pub use accepted_profile_authority::{
    RegisteredSemanticAcceptedProfileAuthorityV1, SemanticAcceptedProfileAuthorityErrorV1,
};
pub use config_backend::ConfigurationLinkedSemanticRuntimeBackendV1;
pub use config_inventory::{
    MAX_SEMANTIC_CONFIGURATION_INVENTORY_SCOPES_PER_PAGE, SemanticConfigurationInventoryCursorV1,
    SemanticConfigurationInventoryPageRequestV1, SemanticConfigurationInventoryPageV1,
    SemanticConfigurationInventoryReceiptV1, SemanticConfiguredVectorRootCursorV1,
    SemanticConfiguredVectorRootPageRequestV1, SemanticConfiguredVectorRootPageV1,
    SemanticConfiguredVectorRootReceiptV1,
};
pub use config_store::ProductionSemanticRetrievalConfigurationStoreV1;
pub use configuration_operation::{
    ProductionSemanticConfigurationOperationV1, SemanticAppliedActivationV1,
    SemanticAppliedRollbackV1, SemanticEvaluatedProfilePublicationV1,
    SemanticEvaluatedProfileQualificationV1, SemanticEvaluationAuthorityPublicationV1,
    SemanticEvaluationDiversityCandidateV1, SemanticEvaluationFusionCandidateV1,
    SemanticEvaluationProfileCandidateV1, SemanticEvaluationPublicationSnapshotPortV1,
    SemanticEvaluationPublicationSnapshotV1, SemanticEvaluationRerankCandidateV1,
    SemanticEvaluationSnapshotPortV1, SemanticProtectedActivationOperationV1,
    SemanticProtectedRollbackOperationV1,
};
pub use configuration_runtime::{
    InstalledSemanticActivationCoordination, ProjectSemanticActivationExt,
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
    SemanticProjectionWorktreeRetirementReceiptV1,
};
pub use graph_provider::{
    RetainedSemanticVectorGraphV1, SemanticGraphExecutionAuthorityV1, SemanticVectorGraphErrorV1,
    SemanticVectorGraphProviderV1, SemanticVectorGraphScopeV1,
    SemanticVectorRetentionAuthorizationV1, VerifiedSemanticVectorGraphRuntimeV1,
};
pub use owner::SemanticRuntimeOwnerV1;
pub use ports::{
    CommittedRetrievalProfileStateV1, RetrievalProfileActivationObserverErrorV1,
    RetrievalProfileActivationObserverV1, SemanticActivationCommandV1, SemanticActivationReceiptV1,
    SemanticActivationRequestV1, SemanticConfigurationBackendErrorV1, SemanticConfigurationPinV1,
    SemanticConfigurationSnapshotSourceV1, SemanticConfigurationTransitionV1,
    SemanticCurrentLinkedActivationV1, SemanticExecutableGenerationLeaseV1,
    SemanticExecutableGenerationV1, SemanticLinkedTransitionV1,
    SemanticRetrievalConfigurationPortV1, SemanticRollbackCommandV1, SemanticRollbackReceiptV1,
    SemanticRollbackRequestV1, SemanticRuntimeBackendErrorV1, SemanticRuntimeBackendV1,
    SemanticRuntimeContractErrorV1, SemanticRuntimeControlErrorV1, SemanticRuntimeFuture,
    SemanticRuntimeGenerationInspectorV1, SemanticRuntimeIntegrationPortV1, SemanticRuntimeRouteV1,
    SemanticRuntimeStateV1, SemanticRuntimeStatusV1,
};
pub use production::{
    ApplicationSemanticSearchParametersV1, AuthorizedProjectSemanticSearchParametersV1,
    ProductionProjectSemanticSearchBridgeV1, SemanticSourceCoherenceOutcomeV1,
    SemanticSourceCoherenceV1, SemanticSourceMismatchV1,
    compose_project_application_semantic_search, semantic_source_coherence,
    semantic_source_content_coherent,
};
pub use production::{
    PreparedProductionSemanticCacheCommitV1, PreparedSemanticEvaluationGenerationV1,
    ProductionSemanticRuntimeV1, SavedCodeGenerationScheduleHookV1,
    SavedGenerationScheduleHookParametersV1, SemanticCompatibleCurrentGenerationSnapshotV1,
    SemanticEvaluationCurrentGenerationSnapshotV1, SemanticEvaluationLifecycleVerificationV1,
    SemanticEvaluationPublicationLeaseV1, SemanticVectorPublicationLeaseV1,
    SemanticVerifiedEvaluationTargetSnapshotV1, lifecycle_to_runtime_state,
    prefer_lifecycle_over_generic_unavailable, production_saved_generation_schedule_hook,
    project_or_shared_lifecycle_status, project_semantic_application_status,
    project_semantic_production_runtime, project_semantic_source_generation,
    register_project_semantic_runtime, resolve_project_semantic_runtime_status,
    resolve_semantic_application_status, unbind_project_semantic_cache_if_current,
    unregister_project_semantic_runtime, vectors_serve_source,
};
pub use publish_failure_memo::{
    DEFAULT_PUBLISH_FAILURE_BACKOFF_BASE, DEFAULT_PUBLISH_FAILURE_BACKOFF_CEILING,
    SemanticPublishAdmissionV1, SemanticPublishFailureKeyV1, SemanticPublishFailureMemoV1,
    SuppressedSemanticPublishV1, corpus_size_class, publish_failure_witness,
    semantic_publish_failure_memo,
};
pub use redundancy::{
    PreparedSemanticRedundancyAuthorityV1, SemanticRedundancyGenerationV1,
    SemanticRedundancyProfileV1, SemanticRedundancyVectorV1, commit_project_absent_semantic_roots,
    commit_project_initial_semantic_roots, commit_project_semantic_redundancy_authority,
    commit_project_semantic_redundancy_authority_under_gate,
    prepare_project_semantic_redundancy_authority, project_committed_semantic_pins,
    project_semantic_activation_gate, project_semantic_activation_receipt,
    project_semantic_redundancy_generation, project_semantic_redundancy_revision,
    project_semantic_retained_code_generation, project_semantic_retained_vector_generations,
    retain_project_semantic_code_sources,
};
pub(crate) use redundancy::{
    register_project_semantic_redundancy_generation,
    unregister_project_semantic_redundancy_generation, with_project_semantic_activation_receipt,
};
pub use retention::SemanticRetainedVectorGenerationsV1;

#[cfg(test)]
mod process_local_release_tests;
#[cfg(test)]
mod tests;
