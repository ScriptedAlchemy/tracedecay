//! One-shot, transport-neutral PR11 post-edit feedback orchestration.
//!
//! This module composes canonical diagnostics and graph/test evidence through
//! narrow ports. It owns neither a diagnostic store nor a graph, scheduler,
//! delivery adapter, task relation, or durable overlay path.

mod ports;
mod service;

pub use ports::{
    FeedbackCycleDedupePort, FeedbackCycleDedupeState, FeedbackDiagnosticsPort,
    FeedbackDiagnosticsRequest, FeedbackImpactPort, FeedbackImpactPortOutcome,
    FeedbackImpactRequest, FeedbackObservationPort,
};
pub use service::{
    FeedbackBudgetUsage, FeedbackCycleControl, FeedbackCycleExecutionRequest,
    FeedbackCycleExecutionResult, FeedbackCycleService,
};
