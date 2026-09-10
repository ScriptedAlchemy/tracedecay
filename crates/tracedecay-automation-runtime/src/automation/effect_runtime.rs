//! Durable automation-effect journal, recovery, projection, and housekeeping.

mod authority;
pub mod contract;
pub mod input;
pub mod journal;
pub mod problem;
pub mod projection;
#[cfg(any(test, feature = "test-helpers"))]
pub mod recovery_index;
#[cfg(not(any(test, feature = "test-helpers")))]
mod recovery_index;
pub mod retirement;
pub mod settlement;
pub mod terminal;

pub use authority::finalize_terminal_housekeeping;
pub use contract::{contract_error, digest};
pub use input::{
    memory_curator_run_request, session_reflector_run_request, skill_writer_run_request,
    user_job_run_request,
};
pub use recovery_index::{
    AutomationEffectRecoveryPreparation, AutomationEffectRecoveryReport,
    PreparedAutomationEffectRecovery, add_pending_blocking, effect_authority_digest,
    prepare_reserved_automation_effect_recovery, reconcile_prepared_automation_effects_for_project,
    recovered_partial_terminal, remove_pending_blocking,
};
pub use settlement::{
    AdmittedAutomationEffectRequest, AutomationEffectAdmission, AutomationEffectAuthority,
    AutomationLedgerObserver, DeferredProblemSettlementRequest, DeferredRunSettlementRequest,
    DeferredSettledOutcome, DeferredSettlementOutcome, DeferredSettlementPairSubmission,
    DeferredSettlementRequest, RetainedAutomationSettlementOutcome,
    RetainedAutomationSettlementProjection, RetainedSettlementPairWaiter, RetainedSettlementWaiter,
    ReusedSchedulerSkipStartError, observe_admission_decision,
    pinned_automation_configuration_digest,
};
pub use terminal::{AutomationSettledProblem, AutomationSettledTerminal};
