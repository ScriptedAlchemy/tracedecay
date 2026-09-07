//! Durable automation-effect journal, projection, retirement, and terminals.
//!
//! Daemon composition that needs the root `TraceDecay` aggregate (admission,
//! retained-owner settlement, and crash-recovery orchestration) stays in the
//! composition-root crate.

pub mod contract;
pub mod input;
pub mod journal;
pub mod problem;
pub mod projection;
pub mod retirement;
pub mod terminal;

pub use contract::{contract_error, digest};
pub use input::{
    memory_curator_run_request, session_reflector_run_request, skill_writer_run_request,
    user_job_run_request,
};
pub use terminal::{AutomationSettledProblem, AutomationSettledTerminal};
