//! One-shot, transport-neutral post-edit feedback orchestration.
//!
//! This module composes canonical diagnostics and graph/test evidence through
//! narrow ports. It owns neither a diagnostic store nor a graph, scheduler,
//! delivery adapter, task relation, or durable overlay path.

mod adapters;
mod catalog;
mod github_ci_proximity;
mod ports;
mod read;
mod service;

pub use catalog::{
    feedback_read_operations, feedback_surface_catalog_contribution,
    feedback_surface_handler_descriptors, feedback_surface_operation,
};

pub use adapters::{GenerationBoundFeedbackDiagnosticsAdapter, GraphImpactFeedbackAdapter};
pub use github_ci_proximity::{
    ADVISORY_CYCLE_CAPABILITY_ID_V1, ADVISORY_CYCLE_USE_CASE_ID_V1,
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
    CiFailureLocalizationPort, CiFailureLocalizationPortOutcomeV1, CiFailureLocalizationRequestV1,
    GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1, GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
    GitHubReviewReadPort, GitHubReviewReadPortOutcomeV1, GitHubReviewReadRequestV1,
    GitHubReviewReadResponseV1, PROXIMITY_CAPABILITY_ID_V1, PROXIMITY_USE_CASE_ID_V1,
    ProximityCandidatesPortOutcomeV1, ProximityDedupeOutcomeV1, ProximityEvaluationRequestV1,
};
pub use ports::{
    FeedbackCompletedPublicationReadPort, FeedbackCompletedPublicationV1, FeedbackCycleDedupePort,
    FeedbackCycleDedupePublicationState, FeedbackCycleDedupeState, FeedbackDiagnosticsPort,
    FeedbackDiagnosticsRequest, FeedbackImpactPort, FeedbackImpactPortOutcome,
    FeedbackImpactRequest, FeedbackObservationPort, FeedbackPortFuture, FeedbackRouteAdmission,
    FeedbackRouteAuthorizationPort, FeedbackRuntimeStatePort, FeedbackRuntimeStateV1,
};
pub use read::{
    FEEDBACK_DIAGNOSTICS_CAPABILITY_ID_V1, FEEDBACK_DIAGNOSTICS_USE_CASE_ID_V1,
    FEEDBACK_EXPAND_CAPABILITY_ID_V1, FEEDBACK_EXPAND_USE_CASE_ID_V1,
    FEEDBACK_GET_CAPABILITY_ID_V1, FEEDBACK_GET_USE_CASE_ID_V1, FEEDBACK_LIST_CAPABILITY_ID_V1,
    FEEDBACK_LIST_USE_CASE_ID_V1, FeedbackDiagnosticsReadRequestV1,
    FeedbackDiagnosticsReadResultV1, FeedbackExpandRequestV1, FeedbackExpandResultV1,
    FeedbackFindingReadV1, FeedbackGetRequestV1, FeedbackGetResultV1, FeedbackHandleRequestV1,
    FeedbackListRequestV1, FeedbackListResultV1, FeedbackReadOperationsV1, FeedbackReadPort,
    FeedbackReadPortContext, FeedbackReadPortFuture, FeedbackReadService,
};
pub use service::{
    FeedbackBudgetUsage, FeedbackCycleAdvisoryV1, FeedbackCycleControl,
    FeedbackCycleExecutionRequest, FeedbackCycleExecutionResult, FeedbackCycleService,
};
