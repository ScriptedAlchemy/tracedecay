pub mod agent_targets;
mod artifact_feedback;
mod artifact_generated_evals;
mod artifact_optimizer;
mod artifact_payloads;
mod artifact_policy;
mod artifact_refs;
pub mod artifacts;
pub mod automatic_facts;
pub mod backend;
pub mod config;
pub mod hermes_skill_bridge;
pub mod host_receipts;
mod job_webhook;
pub mod jobs;
pub mod lifecycle;
mod managed_skill_model;
mod managed_skill_validation;
pub mod managed_skills;
pub mod memory_curator;
pub mod outcomes;
pub mod run_ledger;
pub mod runner;
pub mod scheduler;
pub mod session_reflector;
pub mod skill_frontmatter;
pub mod skill_materialization;
pub mod skill_targets;
pub mod skill_usage;
pub mod skill_writer;
pub mod text;

pub use lifecycle::{
    AutomationCommittedReceipt, AutomationRunControl, AutomationRunError, AutomationRunResult,
    NonEmptyAutomaticFactReceipts,
};

/// Build a [`TraceDecayError::Config`] from any message-like value.
///
/// Canonical home for the `config_error` helper duplicated across the
/// automation module tree; other automation submodules should call this
/// instead of re-declaring their own copy.
pub fn config_error(message: impl Into<String>) -> crate::errors::TraceDecayError {
    crate::errors::TraceDecayError::Config {
        message: message.into(),
    }
}
