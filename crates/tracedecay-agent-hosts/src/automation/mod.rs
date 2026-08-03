pub mod agent_targets;
pub(crate) mod apply_policy;
mod artifact_feedback;
mod artifact_generated_evals;
mod artifact_optimizer;
mod artifact_payloads;
mod artifact_refs;
pub mod artifacts;
pub mod backend;
pub mod config;
pub mod fact_proposals;
pub mod hermes_skill_bridge;
pub mod host_receipts;
mod job_webhook;
pub mod jobs;
pub mod lifecycle;
pub mod managed_skills;
pub mod memory_curator;
pub mod memory_digest;
pub mod outcomes;
pub mod run_ledger;
pub mod runner;
pub mod scheduler;
pub mod session_reflector;
pub mod skill_materialization;
pub mod skill_targets;
pub mod skill_usage;
pub mod skill_writer;
pub mod staged_notice;

pub(crate) use tracedecay_automation::{
    artifact_policy, managed_skill_model, managed_skill_validation,
};
pub use tracedecay_automation::{skill_frontmatter, text};

/// Build a [`TraceDecayError::Config`] from any message-like value.
///
/// Canonical home for the `config_error` helper duplicated across the
/// automation module tree; other automation submodules should call this
/// instead of re-declaring their own copy.
pub(crate) fn config_error(message: impl Into<String>) -> crate::errors::TraceDecayError {
    crate::errors::TraceDecayError::Config {
        message: message.into(),
    }
}
