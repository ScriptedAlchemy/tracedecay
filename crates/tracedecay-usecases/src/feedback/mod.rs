//! Root-owned concrete adapters for the transport-neutral feedback core.
//!
//! This module composes existing store, query, and observation boundaries. It
//! deliberately does not register a daemon trigger, transport handler, CI or
//! GitHub source, proximity source, or persistence schema.

pub mod concrete;
mod concrete_evidence;
pub mod cycle_production;
pub mod cycle_runtime;
pub mod diagnostics;
pub mod observations;
pub mod owner;
pub mod production;

pub use cycle_production::{
    FEEDBACK_CYCLE_CONFIGURATION_DRIFT_CLASS, ProductionFeedbackCycleAuthorizationFuture,
    ProductionFeedbackCycleAuthorizationPort, ProductionFeedbackCycleOpenV1,
    ProductionFeedbackCyclePartsV1, ProductionFeedbackCycleProximityPortV1,
    resolve_production_feedback_cycle_parts, resolve_project_feedback_scope_v1,
};
pub use cycle_runtime::{
    CanonicalFeedbackResultV1, FeedbackCycleInvocation, FeedbackCycleLspInput,
    FeedbackCycleRuntime, FeedbackCycleRuntimeError, FeedbackFindingHandlesV1,
    open_feedback_cycle_runtime,
};
pub use production::ProductionFeedbackRuntimeStateV1;
