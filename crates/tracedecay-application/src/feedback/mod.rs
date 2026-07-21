//! One-shot, transport-neutral PR11 post-edit feedback orchestration.
//!
//! This module composes canonical diagnostics and graph/test evidence through
//! narrow ports. It owns neither a diagnostic store nor a graph, scheduler,
//! delivery adapter, task relation, or durable overlay path.

mod adapters;
mod ports;
mod service;

pub use adapters::{GenerationBoundFeedbackDiagnosticsAdapter, GraphImpactFeedbackAdapter};
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
