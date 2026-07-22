//! Root-owned concrete adapters for the transport-neutral PR11 feedback core.
//!
//! This module composes existing store, query, and observation boundaries. It
//! deliberately does not register a daemon trigger, transport handler, CI or
//! GitHub source, proximity source, or persistence schema.

pub mod concrete;
mod concrete_evidence;
pub mod cycle_runtime;
pub mod diagnostics;
pub mod observations;
pub mod owner;
pub mod runtime;

pub use cycle_runtime::{
    Pr12FeedbackCycleInvocation, Pr12FeedbackCycleLspInput, Pr12FeedbackCycleLspRegistration,
    Pr12FeedbackCycleRuntime, Pr12FeedbackCycleRuntimeError, open_pr12_feedback_cycle_runtime,
};
