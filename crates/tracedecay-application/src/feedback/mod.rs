//! One-shot, transport-neutral PR11 post-edit feedback orchestration.
//!
//! This module composes canonical diagnostics and graph/test evidence through
//! narrow ports. It owns neither a diagnostic store nor a graph, scheduler,
//! delivery adapter, task relation, or durable overlay path.

mod adapters;
mod catalog;
mod github_ci_proximity;
mod ports;
mod service;

pub use catalog::{feedback_surface_catalog_contribution, feedback_surface_handler_descriptors};

pub use adapters::{GenerationBoundFeedbackDiagnosticsAdapter, GraphImpactFeedbackAdapter};
pub use github_ci_proximity::{
    GitHubReadCredentialScopeV1, GitHubReviewReadPort, GitHubReviewReadRequestV1,
};
pub use ports::{
    FeedbackCompletedPublicationV1, FeedbackCycleDedupePort, FeedbackCycleDedupePublicationState,
    FeedbackCycleDedupeState, FeedbackDiagnosticsPort, FeedbackDiagnosticsRequest,
    FeedbackImpactPort, FeedbackImpactPortOutcome, FeedbackImpactRequest, FeedbackObservationPort,
    FeedbackPortFuture, FeedbackRuntimeStatePort, FeedbackRuntimeStateV1,
};
pub use service::{
    FeedbackBudgetUsage, FeedbackCycleControl, FeedbackCycleExecutionRequest,
    FeedbackCycleExecutionResult, FeedbackCycleService,
};
