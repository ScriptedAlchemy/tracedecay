//! Durable application settlement for automatic memory runs.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracedecay_agent_hosts::automation::backend::{AgentTaskKind, task_key};
use tracedecay_agent_hosts::automation::run_ledger::{
    AutomationRunLedgerRecord, AutomationRunStatus, ExactRunPublication, ExactRunPublishOutcome,
};
use tracedecay_agent_hosts::automation::runner::{
    AutomationRunSettlementGuard, RetainedAutomationRun, RetainedAutomationSettlementDisposition,
    ReusedSchedulerSkip,
};
use tracedecay_agent_hosts::automation::{AutomationCommittedReceipt, AutomationRunError};
use tracedecay_application::retained_surfaces::{
    AutomationCommittedReceiptV1, AutomationRunProblemV1, AutomationRunRequestV1,
    AutomationRunResultV1, AutomationRunSummaryV1, AutomationRunTerminalV1, AutomationSkipReasonV1,
    AutomationTaskV1, RetainedSurfaceResultV1,
};
use tracedecay_application::{
    ApplicationOutcome, ApplicationProblem, ApplicationProblemEnvelope, CancellationSignal,
    Deadline, ProblemOwningLayer, RequestContext, RequestId, ResolvedScope,
    RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1, RetainedSurfaceOperation,
    retained_surface_application_operation, retained_surface_execution_problem,
    retained_surface_outcome_matches_terminal, retained_surface_problem_matches_terminal,
};
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_domain::{ManifestDigest, UtcMicros};
use tracedecay_store::FactReadControl;

use crate::daemon::retained_owner::receipts::{PreparedRetainedEffect, prepare_retained_effect};
use crate::daemon::service::invocation::{
    DaemonInvocationService, RegisteredRetainedRequestContextError,
};
use crate::errors::Result;

mod authority;
mod contract;
mod input;
mod journal;
mod problem;
mod projection;
mod recovery_index;
mod retirement;
use authority::finalize_terminal_housekeeping as finalize_terminal_housekeeping_owned;
use contract::{contract_error, digest};
use journal::{
    AutomationRecoveryBinding, AutomationReservationClaim, DurableAutomationAdmission,
    DurableSettlementClassification, ReservationResult, abandon_reservation_blocking,
    classify_durable_settlement_blocking, persist_prepared_terminal_blocking,
    persist_recovered_terminal_blocking, persist_terminal_blocking,
    promote_prepared_terminal_blocking, replay_exact_binding_after_error_blocking,
    reserve_or_replay_indexed_blocking, retained_source_bindings,
};
use problem::{
    failed_ledger_problem, indeterminate_external_effect_problem, reset_required_problem,
    runtime_problem, shipped_proposal_reset_required_problem,
};
use projection::{
    project_committed_receipts, project_recovered_committed_receipts, project_run_summary,
    project_skip_reason,
};

/// Total wall-clock budget for a retained-settlement blocking-pool retry
/// loop before it gives up and returns an error instead of retrying
/// forever.
///
/// These loops run on a `spawn_blocking` thread while the caller's
/// `RetainedSettlementWaiter::wait().await` is parked on them and the held
/// `AutomationRunSettlementGuard` keeps the task lock. A persistent,
/// non-self-healing failure (stale append-intent from a different run's
/// crashed publication, a read-only filesystem, ...) must not pin that
/// thread and hang the daemon request forever. 120s is long enough to ride
/// out transient filesystem contention, short enough that a wedged
/// settlement surfaces as an error instead of a hung request. On
/// exhaustion the journal state is left exactly as it was
/// (Reserved/Prepared); that is the durable recovery path and
/// `reconcile_reserved_automation_effects_for_project` picks it up later.
const RETAINED_SETTLEMENT_RETRY_BUDGET: Duration = Duration::from_secs(120);

pub(crate) struct AutomationEffectAuthority {
    context: RequestContext,
    cancellation: CancellationSignal,
    operation: tracedecay_application::ApplicationOperation,
    prepared: PreparedRetainedEffect,
    admission: DurableAutomationAdmission,
    journal_path: PathBuf,
    dashboard_root: PathBuf,
    _reservation_claim: Option<AutomationReservationClaim>,
}

/// An opaque join authority for durability work that has already started.
///
/// Dropping this value only stops observing the task. The blocking owner still
/// holds the application claim and scheduler guard until it proves the exact
/// terminal durable (or proves an uncommitted reservation was abandoned).
#[must_use = "dropping the waiter does not cancel retained automation settlement"]
pub(crate) struct RetainedSettlementWaiter<T> {
    task: tokio::task::JoinHandle<T>,
}

pub(crate) struct ReusedSchedulerSkipStartError {
    error: crate::errors::TraceDecayError,
    _authority: AutomationEffectAuthority,
    _guard: AutomationRunSettlementGuard,
}

impl ReusedSchedulerSkipStartError {
    pub(crate) fn into_error(self) -> crate::errors::TraceDecayError {
        self.error
    }
}

impl std::fmt::Display for ReusedSchedulerSkipStartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl<T: Send + 'static> RetainedSettlementWaiter<Result<T>> {
    pub(crate) async fn wait(self) -> Result<T> {
        self.task.await.map_err(|error| {
            contract_error(format!(
                "retained automation settlement task failed: {error}"
            ))
        })?
    }
}

pub(crate) type AutomationLedgerObserver =
    Box<dyn FnOnce(&AutomationRunLedgerRecord) + Send + 'static>;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetainedSettlementPhase {
    PreparedWriteFailed,
    Prepared,
    Published,
}

#[cfg(test)]
struct SettlementPhaseHook {
    callback: Arc<dyn Fn(RetainedSettlementPhase) + Send + Sync + 'static>,
}

#[cfg(test)]
impl SettlementPhaseHook {
    fn new(callback: impl Fn(RetainedSettlementPhase) + Send + Sync + 'static) -> Self {
        Self {
            callback: Arc::new(callback),
        }
    }

    fn notify(&self, phase: RetainedSettlementPhase) {
        (self.callback)(phase);
    }
}

#[cfg(test)]
#[derive(Clone)]
struct PreparedWriteHook {
    callback: Arc<dyn Fn(&ExactRunPublication) -> Result<()> + Send + Sync + 'static>,
}

#[cfg(test)]
impl PreparedWriteHook {
    fn new(callback: impl Fn(&ExactRunPublication) -> Result<()> + Send + Sync + 'static) -> Self {
        Self {
            callback: Arc::new(callback),
        }
    }

    fn before_write(&self, publication: &ExactRunPublication) -> Result<()> {
        (self.callback)(publication)
    }
}

pub(crate) struct DeferredRunSettlementRequest {
    pub(crate) ledger: AutomationRunLedgerRecord,
    pub(crate) committed: Option<AutomationCommittedReceipt>,
    pub(crate) observer: Option<AutomationLedgerObserver>,
}

pub(crate) struct DeferredProblemSettlementRequest {
    pub(crate) error: AutomationRunError,
    pub(crate) observer: Option<AutomationLedgerObserver>,
}

pub(crate) enum DeferredSettlementRequest {
    Run(Box<DeferredRunSettlementRequest>),
    Problem(Box<DeferredProblemSettlementRequest>),
    Abandon,
}

pub(crate) struct DeferredSettledOutcome {
    pub(crate) terminal: AutomationSettledTerminal,
}

pub(crate) enum DeferredSettlementOutcome {
    Settled(Box<DeferredSettledOutcome>),
    Abandoned,
}

pub(crate) enum RetainedAutomationSettlementOutcome {
    Run {
        terminal: AutomationSettledTerminal,
        record: AutomationRunLedgerRecord,
    },
    Problem {
        problem: AutomationSettledProblem,
        record: Option<AutomationRunLedgerRecord>,
    },
    Reused {
        record: AutomationRunLedgerRecord,
    },
    AbandonedObserved {
        record: AutomationRunLedgerRecord,
    },
}

pub(crate) enum RetainedAutomationSettlementProjection {
    Run {
        record: AutomationRunLedgerRecord,
        committed: Option<AutomationCommittedReceipt>,
    },
    AbandonObserved {
        record: AutomationRunLedgerRecord,
    },
}

impl
    From<(
        AutomationRunLedgerRecord,
        Option<AutomationCommittedReceipt>,
    )> for RetainedAutomationSettlementProjection
{
    fn from(
        (record, committed): (
            AutomationRunLedgerRecord,
            Option<AutomationCommittedReceipt>,
        ),
    ) -> Self {
        Self::Run { record, committed }
    }
}

struct PairSettlementGuards {
    _first: AutomationRunSettlementGuard,
    _second: AutomationRunSettlementGuard,
}

enum RetainedSettlementGuardOwner {
    Single(AutomationRunSettlementGuard),
    Pair(Arc<PairSettlementGuards>),
}

struct RetainedOwnerValue<T> {
    value: T,
    pair_keepalive: Option<Arc<PairSettlementGuards>>,
}

struct RetainedPairOwnerOutcome {
    outcome: DeferredSettlementOutcome,
    _pair_keepalive: Arc<PairSettlementGuards>,
}

#[must_use = "dropping the pair waiter does not cancel either retained settlement"]
pub(crate) struct RetainedSettlementPairWaiter {
    first: RetainedSettlementWaiter<Result<RetainedPairOwnerOutcome>>,
    second: RetainedSettlementWaiter<Result<RetainedPairOwnerOutcome>>,
}

impl RetainedSettlementPairWaiter {
    pub(crate) async fn wait(
        self,
    ) -> (
        Result<DeferredSettlementOutcome>,
        Result<DeferredSettlementOutcome>,
    ) {
        let (first, second) = tokio::join!(self.first.wait(), self.second.wait());
        (pair_join_result(first), pair_join_result(second))
    }
}

fn pair_join_result(owned: Result<RetainedPairOwnerOutcome>) -> Result<DeferredSettlementOutcome> {
    let owned = owned?;
    Ok(owned.outcome)
}

struct RetainedBoundSettlement {
    authority: AutomationEffectAuthority,
    guard: RetainedSettlementGuardOwner,
    terminal: AutomationSettledTerminal,
    ledger: AutomationRunLedgerRecord,
    publication: Option<ExactRunPublication>,
    observer: Option<AutomationLedgerObserver>,
    #[cfg(test)]
    phase_hook: Option<SettlementPhaseHook>,
    #[cfg(test)]
    prepared_write_hook: Option<PreparedWriteHook>,
}

struct RetainedDirectSettlement {
    authority: AutomationEffectAuthority,
    guard: RetainedSettlementGuardOwner,
    terminal: AutomationSettledTerminal,
}

struct RetainedAbandonment {
    authority: AutomationEffectAuthority,
    guard: RetainedSettlementGuardOwner,
    reservation_abandoned: bool,
}

pub(crate) type AutomationSettledProblem = AutomationRunProblemV1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(crate) enum AutomationSettledTerminal {
    Outcome {
        scope: ResolvedScope,
        outcome: ApplicationOutcome<RetainedSurfaceResultV1>,
    },
    Problem(AutomationSettledProblem),
}

impl AutomationSettledTerminal {
    pub(crate) fn into_outcome(
        self,
    ) -> std::result::Result<ApplicationOutcome<RetainedSurfaceResultV1>, AutomationSettledProblem>
    {
        match self {
            Self::Outcome { outcome, .. } => Ok(outcome),
            Self::Problem(problem) => Err(problem),
        }
    }

    fn matches_admission(&self, admission: &DurableAutomationAdmission) -> bool {
        match self {
            Self::Outcome {
                scope: terminal_scope,
                outcome,
            } => {
                terminal_scope == &admission.scope
                    && retained_surface_outcome_matches_terminal(
                        RetainedSurfaceOperation::FactStoreCurate,
                        &admission.request_id,
                        &admission.scope,
                        outcome,
                    )
                    && matches!(
                        outcome,
                        ApplicationOutcome::Effect(effect)
                            if matches!(
                                effect.payload.as_ref(),
                                Some(RetainedSurfaceResultV1::FactStoreCurate(result))
                                    if result.matches_admission(&admission.request)
                            )
                    )
            }
            Self::Problem(problem) => {
                problem.scope == admission.scope
                    && problem.matches_terminal(&admission.request_id)
                    && problem.matches_admission(&admission.request, &admission.request_id)
            }
        }
    }

    pub(crate) fn run_result(&self) -> Option<&AutomationRunResultV1> {
        let Self::Outcome { outcome, .. } = self else {
            return None;
        };
        let ApplicationOutcome::Effect(effect) = outcome else {
            return None;
        };
        let Some(RetainedSurfaceResultV1::FactStoreCurate(result)) = effect.payload.as_ref() else {
            return None;
        };
        Some(result)
    }

    pub(crate) fn problem(&self) -> Option<&AutomationSettledProblem> {
        match self {
            Self::Outcome { .. } => None,
            Self::Problem(problem) => Some(problem),
        }
    }

    pub(crate) fn is_completed(&self) -> bool {
        let Self::Outcome { outcome, .. } = self else {
            return false;
        };
        let ApplicationOutcome::Effect(effect) = outcome else {
            return false;
        };
        let Some(RetainedSurfaceResultV1::FactStoreCurate(result)) = effect.payload.as_ref() else {
            return false;
        };
        matches!(result.terminal, AutomationRunTerminalV1::Completed { .. })
    }

    fn is_retirement_terminal(&self) -> bool {
        let Self::Outcome { outcome, .. } = self else {
            return false;
        };
        let ApplicationOutcome::Effect(effect) = outcome else {
            return false;
        };
        let Some(RetainedSurfaceResultV1::FactStoreCurate(result)) = effect.payload.as_ref() else {
            return false;
        };
        matches!(
            &result.terminal,
            AutomationRunTerminalV1::Skipped { reason, .. }
                if Some(*reason)
                    == AutomationSkipReasonV1::from_ledger_reason(
                        "shipped_fact_proposal_history_retired"
                    )
        ) && result.committed_receipts.is_empty()
    }
}

fn ledger_record_matches_result(
    record: &AutomationRunLedgerRecord,
    result: &AutomationRunResultV1,
) -> bool {
    let task = agent_task_kind(result.task);
    if record.run_id != result.run_id.as_str() || record.task != task {
        return false;
    }
    match &result.terminal {
        AutomationRunTerminalV1::Completed { .. } => {
            record.status == AutomationRunStatus::Succeeded && record.error.is_none()
        }
        AutomationRunTerminalV1::Skipped { reason, .. } => {
            record.status == AutomationRunStatus::Skipped
                && record.error == record.fallback_status
                && record
                    .error
                    .as_deref()
                    .and_then(AutomationSkipReasonV1::from_ledger_reason)
                    == Some(*reason)
        }
    }
}

fn agent_task_kind(task: AutomationTaskV1) -> AgentTaskKind {
    match task {
        AutomationTaskV1::MemoryCurator => AgentTaskKind::MemoryCurator,
        AutomationTaskV1::SessionReflector => AgentTaskKind::SessionReflector,
        AutomationTaskV1::SkillWriter => AgentTaskKind::SkillWriter,
        AutomationTaskV1::CombinedReview => AgentTaskKind::CombinedReview,
        AutomationTaskV1::UserJob => AgentTaskKind::UserJob,
    }
}

pub(crate) enum AutomationEffectAdmission {
    Execute(AutomationEffectAuthority),
    Replay(AutomationSettledTerminal),
    /// A valid durable record already owns this run identity under a different
    /// stable admission. Callers must not execute or settle a second effect.
    Conflict,
    /// The registered application request was cancelled or timed out before
    /// the durable automation admission. This is deliberately not written to
    /// the automation journal: it is a pre-admission application problem.
    PreAdmissionProblem(ApplicationProblemEnvelope),
}

pub(crate) use input::{
    memory_curator_run_request, session_reflector_run_request, skill_writer_run_request,
    user_job_run_request,
};
pub(crate) use recovery_index::reconcile_reserved_automation_effects_for_project;

pub(crate) fn pinned_automation_configuration_digest(
    revision: &ConfigurationRevisionId,
    behavior: &ManifestDigest,
    provenance: &ManifestDigest,
) -> Result<ManifestDigest> {
    digest(&(
        "tracedecay.automation.configuration.v1",
        revision,
        behavior,
        provenance,
    ))
}

impl AutomationEffectAuthority {
    pub(crate) fn start_retained_automation_settlement<T, P, R>(
        self,
        retained: RetainedAutomationRun<T>,
        observer: Option<AutomationLedgerObserver>,
        projector: P,
    ) -> RetainedSettlementWaiter<Result<RetainedAutomationSettlementOutcome>>
    where
        T: Send + 'static,
        P: FnOnce(T) -> R + Send + 'static,
        R: Into<RetainedAutomationSettlementProjection> + Send + 'static,
    {
        self.start_retained_automation_settlement_inner(
            retained,
            observer,
            projector,
            #[cfg(test)]
            None,
            #[cfg(test)]
            None,
        )
    }

    fn start_retained_automation_settlement_inner<T, P, R>(
        self,
        retained: RetainedAutomationRun<T>,
        observer: Option<AutomationLedgerObserver>,
        projector: P,
        #[cfg(test)] phase_hook: Option<SettlementPhaseHook>,
        #[cfg(test)] prepared_write_hook: Option<PreparedWriteHook>,
    ) -> RetainedSettlementWaiter<Result<RetainedAutomationSettlementOutcome>>
    where
        T: Send + 'static,
        P: FnOnce(T) -> R + Send + 'static,
        R: Into<RetainedAutomationSettlementProjection> + Send + 'static,
    {
        RetainedSettlementWaiter {
            task: tokio::task::spawn_blocking(move || {
                match retained.into_settlement_disposition() {
                    RetainedAutomationSettlementDisposition::Current {
                        result,
                        settlement_guard,
                    } => match result {
                        Ok(run) => {
                            let projected =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    projector(run)
                                }));
                            let projected = match projected {
                                Ok(projected) => projected.into(),
                                Err(_) => {
                                    return self.finish_projection_failure(
                                        RetainedSettlementGuardOwner::Single(settlement_guard),
                                        contract_error(
                                            "retained automation settlement projector panicked",
                                        ),
                                    );
                                }
                            };
                            match projected {
                                RetainedAutomationSettlementProjection::Run {
                                    record,
                                    committed,
                                } => self
                                    .settle_retained_run_blocking(
                                        record,
                                        committed,
                                        RetainedSettlementGuardOwner::Single(settlement_guard),
                                        observer,
                                        #[cfg(test)]
                                        phase_hook,
                                        #[cfg(test)]
                                        prepared_write_hook,
                                    )
                                    .map(|owned| {
                                        let (terminal, record) = owned.value;
                                        RetainedAutomationSettlementOutcome::Run {
                                            terminal,
                                            record,
                                        }
                                    }),
                                RetainedAutomationSettlementProjection::AbandonObserved {
                                    record,
                                } => {
                                    self.abandon_retained_blocking(
                                        RetainedSettlementGuardOwner::Single(settlement_guard),
                                    )?;
                                    observe_automation_ledger(observer, &record);
                                    Ok(RetainedAutomationSettlementOutcome::AbandonedObserved {
                                        record,
                                    })
                                }
                            }
                        }
                        Err(error) => self
                            .settle_retained_problem_blocking(
                                error,
                                RetainedSettlementGuardOwner::Single(settlement_guard),
                                observer,
                                #[cfg(test)]
                                phase_hook,
                            )
                            .map(|owned| {
                                let (problem, record) = owned.value;
                                RetainedAutomationSettlementOutcome::Problem { problem, record }
                            }),
                    },
                    RetainedAutomationSettlementDisposition::ReusedSchedulerSkip {
                        reused,
                        settlement_guard,
                    } => {
                        if let Err(error) = self.validate_reused_scheduler_skip(&reused) {
                            return self.finish_projection_failure(
                                RetainedSettlementGuardOwner::Single(settlement_guard),
                                error,
                            );
                        }
                        self.abandon_retained_blocking(RetainedSettlementGuardOwner::Single(
                            settlement_guard,
                        ))?;
                        let record = reused.prior_record;
                        observe_automation_ledger(observer, &record);
                        Ok(RetainedAutomationSettlementOutcome::Reused { record })
                    }
                }
            }),
        }
    }

    #[cfg(test)]
    fn start_retained_automation_settlement_with_phase_hooks<T, P, R>(
        self,
        retained: RetainedAutomationRun<T>,
        observer: Option<AutomationLedgerObserver>,
        projector: P,
        phase_hook: SettlementPhaseHook,
        prepared_write_hook: Option<PreparedWriteHook>,
    ) -> RetainedSettlementWaiter<Result<RetainedAutomationSettlementOutcome>>
    where
        T: Send + 'static,
        P: FnOnce(T) -> R + Send + 'static,
        R: Into<RetainedAutomationSettlementProjection> + Send + 'static,
    {
        self.start_retained_automation_settlement_inner(
            retained,
            observer,
            projector,
            Some(phase_hook),
            prepared_write_hook,
        )
    }

    pub(crate) fn start_deferred_settlement_pair(
        first: (
            Self,
            DeferredSettlementRequest,
            AutomationRunSettlementGuard,
        ),
        second: (
            Self,
            DeferredSettlementRequest,
            AutomationRunSettlementGuard,
        ),
    ) -> RetainedSettlementPairWaiter {
        Self::start_deferred_settlement_pair_inner(
            first,
            second,
            #[cfg(test)]
            None,
            #[cfg(test)]
            None,
        )
    }

    fn start_deferred_settlement_pair_inner(
        first: (
            Self,
            DeferredSettlementRequest,
            AutomationRunSettlementGuard,
        ),
        second: (
            Self,
            DeferredSettlementRequest,
            AutomationRunSettlementGuard,
        ),
        #[cfg(test)] first_phase_hook: Option<SettlementPhaseHook>,
        #[cfg(test)] second_phase_hook: Option<SettlementPhaseHook>,
    ) -> RetainedSettlementPairWaiter {
        let (first_authority, first_request, first_guard) = first;
        let (second_authority, second_request, second_guard) = second;
        let guards = Arc::new(PairSettlementGuards {
            _first: first_guard,
            _second: second_guard,
        });
        let first_guards = Arc::clone(&guards);
        let first = RetainedSettlementWaiter {
            task: tokio::task::spawn_blocking(move || {
                first_authority.settle_pair_leg_blocking(
                    first_request,
                    first_guards,
                    #[cfg(test)]
                    first_phase_hook,
                )
            }),
        };
        let second_guards = Arc::clone(&guards);
        let second = RetainedSettlementWaiter {
            task: tokio::task::spawn_blocking(move || {
                second_authority.settle_pair_leg_blocking(
                    second_request,
                    second_guards,
                    #[cfg(test)]
                    second_phase_hook,
                )
            }),
        };
        drop(guards);
        RetainedSettlementPairWaiter { first, second }
    }

    #[cfg(test)]
    fn start_deferred_settlement_pair_with_phase_hooks(
        first: (
            Self,
            DeferredSettlementRequest,
            AutomationRunSettlementGuard,
        ),
        second: (
            Self,
            DeferredSettlementRequest,
            AutomationRunSettlementGuard,
        ),
        first_phase_hook: Option<SettlementPhaseHook>,
        second_phase_hook: Option<SettlementPhaseHook>,
    ) -> RetainedSettlementPairWaiter {
        Self::start_deferred_settlement_pair_inner(
            first,
            second,
            first_phase_hook,
            second_phase_hook,
        )
    }

    pub(crate) fn start_deferred_run_settlement_observed(
        self,
        ledger: AutomationRunLedgerRecord,
        committed: Option<AutomationCommittedReceipt>,
        guard: AutomationRunSettlementGuard,
        observer: Option<AutomationLedgerObserver>,
    ) -> RetainedSettlementWaiter<Result<(AutomationSettledTerminal, AutomationRunLedgerRecord)>>
    {
        RetainedSettlementWaiter {
            task: tokio::task::spawn_blocking(move || {
                self.settle_retained_run_blocking(
                    ledger,
                    committed,
                    RetainedSettlementGuardOwner::Single(guard),
                    observer,
                    #[cfg(test)]
                    None,
                    #[cfg(test)]
                    None,
                )
                .map(|owned| owned.value)
            }),
        }
    }

    pub(crate) fn start_deferred_problem_settlement_observed(
        self,
        error: AutomationRunError,
        guard: AutomationRunSettlementGuard,
        observer: Option<AutomationLedgerObserver>,
    ) -> RetainedSettlementWaiter<
        Result<(AutomationSettledProblem, Option<AutomationRunLedgerRecord>)>,
    > {
        RetainedSettlementWaiter {
            task: tokio::task::spawn_blocking(move || {
                self.settle_retained_problem_blocking(
                    error,
                    RetainedSettlementGuardOwner::Single(guard),
                    observer,
                    #[cfg(test)]
                    None,
                )
                .map(|owned| owned.value)
            }),
        }
    }

    pub(crate) fn start_reused_scheduler_skip_abandonment_observed(
        self,
        reused: ReusedSchedulerSkip,
        guard: AutomationRunSettlementGuard,
        observer: Option<AutomationLedgerObserver>,
    ) -> std::result::Result<
        RetainedSettlementWaiter<Result<AutomationRunLedgerRecord>>,
        Box<ReusedSchedulerSkipStartError>,
    > {
        if let Err(error) = self.validate_reused_scheduler_skip(&reused) {
            return Err(Box::new(ReusedSchedulerSkipStartError {
                error,
                _authority: self,
                _guard: guard,
            }));
        }
        Ok(RetainedSettlementWaiter {
            task: tokio::task::spawn_blocking(move || {
                self.abandon_retained_blocking(RetainedSettlementGuardOwner::Single(guard))?;
                let prior_record = reused.prior_record;
                observe_automation_ledger(observer, &prior_record);
                Ok(prior_record)
            }),
        })
    }

    fn settle_pair_leg_blocking(
        self,
        request: DeferredSettlementRequest,
        guards: Arc<PairSettlementGuards>,
        #[cfg(test)] phase_hook: Option<SettlementPhaseHook>,
    ) -> Result<RetainedPairOwnerOutcome> {
        let guard = RetainedSettlementGuardOwner::Pair(Arc::clone(&guards));
        match request {
            DeferredSettlementRequest::Run(request) => {
                let DeferredRunSettlementRequest {
                    ledger,
                    committed,
                    observer,
                } = *request;
                self.settle_retained_run_blocking(
                    ledger,
                    committed,
                    guard,
                    observer,
                    #[cfg(test)]
                    phase_hook,
                    #[cfg(test)]
                    None,
                )
                .map(|owned| {
                    let (terminal, _) = owned.value;
                    RetainedPairOwnerOutcome {
                        outcome: DeferredSettlementOutcome::Settled(Box::new(
                            DeferredSettledOutcome { terminal },
                        )),
                        _pair_keepalive: owned.pair_keepalive.unwrap_or(guards),
                    }
                })
            }
            DeferredSettlementRequest::Problem(request) => {
                let DeferredProblemSettlementRequest { error, observer } = *request;
                self.settle_retained_problem_blocking(
                    error,
                    guard,
                    observer,
                    #[cfg(test)]
                    phase_hook,
                )
                .map(|owned| RetainedPairOwnerOutcome {
                    outcome: DeferredSettlementOutcome::Settled(Box::new(DeferredSettledOutcome {
                        terminal: AutomationSettledTerminal::Problem(owned.value.0),
                    })),
                    _pair_keepalive: owned.pair_keepalive.unwrap_or(guards),
                })
            }
            DeferredSettlementRequest::Abandon => {
                self.abandon_retained_blocking(guard)
                    .map(|owned| RetainedPairOwnerOutcome {
                        outcome: DeferredSettlementOutcome::Abandoned,
                        _pair_keepalive: owned.pair_keepalive.unwrap_or(guards),
                    })
            }
        }
    }

    fn settle_retained_run_blocking(
        self,
        ledger: AutomationRunLedgerRecord,
        committed: Option<AutomationCommittedReceipt>,
        guard: RetainedSettlementGuardOwner,
        observer: Option<AutomationLedgerObserver>,
        #[cfg(test)] phase_hook: Option<SettlementPhaseHook>,
        #[cfg(test)] prepared_write_hook: Option<PreparedWriteHook>,
    ) -> Result<RetainedOwnerValue<(AutomationSettledTerminal, AutomationRunLedgerRecord)>> {
        let terminal = match self.terminal_for_run(&ledger, committed.as_ref()) {
            Ok(terminal) => terminal,
            Err(error) => return self.finish_projection_failure(guard, error),
        };
        settle_bound_owner(RetainedBoundSettlement {
            authority: self,
            guard,
            terminal,
            ledger,
            publication: None,
            observer,
            #[cfg(test)]
            phase_hook,
            #[cfg(test)]
            prepared_write_hook,
        })
    }

    fn settle_retained_problem_blocking(
        self,
        error: AutomationRunError,
        guard: RetainedSettlementGuardOwner,
        observer: Option<AutomationLedgerObserver>,
        #[cfg(test)] phase_hook: Option<SettlementPhaseHook>,
    ) -> Result<RetainedOwnerValue<(AutomationSettledProblem, Option<AutomationRunLedgerRecord>)>>
    {
        let projection = match error {
            AutomationRunError::PartialEffect {
                run_id,
                committed_receipt,
                ledger_record,
                detail,
            } => self
                .settle_partial(&run_id, &committed_receipt, detail)
                .map(|problem| (problem, ledger_record)),
            AutomationRunError::RecordedFailure {
                error,
                ledger_record,
            } => runtime_problem(&self.context, &self.cancellation, &error)
                .and_then(|problem| self.problem_envelope(problem, Vec::new()))
                .map(|problem| (problem, Some(ledger_record))),
            AutomationRunError::Runtime(error) => {
                runtime_problem(&self.context, &self.cancellation, &error)
                    .and_then(|problem| self.problem_envelope(problem, Vec::new()))
                    .map(|problem| (problem, None))
            }
        };
        let (problem, ledger) = match projection {
            Ok(projected) => projected,
            Err(error) => return self.finish_projection_failure(guard, error),
        };
        let terminal = AutomationSettledTerminal::Problem(problem);
        if let Some(ledger) = ledger {
            if let Err(error) = self.validate_failed_ledger(&ledger) {
                return self.finish_projection_failure(guard, error);
            }
            let owned = settle_bound_owner(RetainedBoundSettlement {
                authority: self,
                guard,
                terminal,
                ledger,
                publication: None,
                observer,
                #[cfg(test)]
                phase_hook,
                #[cfg(test)]
                prepared_write_hook: None,
            })?;
            let (terminal, ledger) = owned.value;
            match terminal {
                AutomationSettledTerminal::Problem(problem) => Ok(RetainedOwnerValue {
                    value: (problem, Some(ledger)),
                    pair_keepalive: owned.pair_keepalive,
                }),
                AutomationSettledTerminal::Outcome { .. } => Err(contract_error(
                    "automation problem settlement replayed a successful terminal",
                )),
            }
        } else {
            let owned = settle_direct_owner(RetainedDirectSettlement {
                authority: self,
                guard,
                terminal,
            })?;
            match owned.value {
                AutomationSettledTerminal::Problem(problem) => Ok(RetainedOwnerValue {
                    value: (problem, None),
                    pair_keepalive: owned.pair_keepalive,
                }),
                AutomationSettledTerminal::Outcome { .. } => Err(contract_error(
                    "automation problem settlement replayed a successful terminal",
                )),
            }
        }
    }

    fn abandon_retained_blocking(
        self,
        guard: RetainedSettlementGuardOwner,
    ) -> Result<RetainedOwnerValue<()>> {
        abandon_retained_owner(RetainedAbandonment {
            authority: self,
            guard,
            reservation_abandoned: false,
        })
    }

    fn finish_projection_failure<T>(
        self,
        guard: RetainedSettlementGuardOwner,
        error: crate::errors::TraceDecayError,
    ) -> Result<T> {
        let terminal =
            AutomationSettledTerminal::Problem(self.admission.recovery_problem().clone());
        let _ = settle_direct_owner(RetainedDirectSettlement {
            authority: self,
            guard,
            terminal,
        })?;
        Err(error)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn prepare(
        invocation: &DaemonInvocationService,
        memory: &crate::tracedecay::TraceDecay,
        project_root: &Path,
        dashboard_root: &Path,
        request_id: RequestId,
        deadline: Deadline,
        cancellation: &CancellationSignal,
        observed_at: UtcMicros,
        configuration_digest: ManifestDigest,
        request: AutomationRunRequestV1,
    ) -> Result<AutomationEffectAdmission> {
        if !request.validate() {
            return Err(contract_error("automation run identity is empty"));
        }
        let journal_key = digest(&("tracedecay.automation-run.terminal-key.v1", &request.run_id))?;
        let journal_path = dashboard_root.join("automation_effects").join(format!(
            "{}.json",
            journal_key.as_str().trim_start_matches("sha256:")
        ));
        let task = request.task_kind();
        let (retained_binding, retained_reset_digest) =
            if task == AutomationTaskV1::SessionReflector {
                let binding_path = journal_path.clone();
                tokio::task::spawn_blocking(move || retained_source_bindings(&binding_path))
                    .await
                    .map_err(|error| {
                        contract_error(format!(
                            "automation retirement binding reader failed: {error}"
                        ))
                    })??
            } else {
                (None, None)
            };
        let classification = if retained_binding.is_some() {
            // A durable retirement binding owns the exact admitted source.
            // Current shipped bytes may be a later replacement and must not
            // override replay/recovery of that retained authority.
            retirement::RetirementClassification::Absent
        } else {
            retirement::classify_for_task(task, dashboard_root).await?
        };
        let (live_retirement, shipped_reset) = match classification {
            retirement::RetirementClassification::Absent => (None, None),
            retirement::RetirementClassification::ResetRequired {
                source_digest,
                detail,
            } => (None, Some((source_digest, detail))),
            retirement::RetirementClassification::Terminal(plan) => (Some(plan), None),
        };
        if let (Some(plan), Some(binding)) = (&live_retirement, &retained_binding) {
            retirement::verify_plan_matches_binding(plan, binding)?;
        }
        let retirement_binding =
            retained_binding.or_else(|| live_retirement.as_ref().map(|plan| plan.binding.clone()));
        let reset_source_digest = match (retained_reset_digest, shipped_reset.as_ref()) {
            (Some(stored), Some((live, _))) if stored != *live => {
                return Err(contract_error(
                    "unresolved shipped proposal bytes changed after durable admission",
                ));
            }
            (Some(stored), _) => Some(stored),
            (None, Some((live, _))) => Some(live.clone()),
            (None, None) => None,
        };
        let retained_operation = RetainedSurfaceOperation::FactStoreCurate;
        let operation =
            retained_surface_application_operation(retained_operation).map_err(contract_error)?;
        let context = match invocation
            .registered_retained_request_context(
                project_root,
                request_id.clone(),
                deadline,
                cancellation.context(),
                observed_at,
                &operation,
            )
            .await
        {
            Ok(context) => context,
            Err(RegisteredRetainedRequestContextError::Application(problem)) => {
                let envelope = ApplicationProblemEnvelope::new(
                    operation.result_contract().clone(),
                    request_id,
                    problem,
                )
                .map_err(contract_error)?;
                return Ok(AutomationEffectAdmission::PreAdmissionProblem(envelope));
            }
            Err(RegisteredRetainedRequestContextError::Runtime(error)) => return Err(error),
        };
        let input_digest = digest(&(
            "tracedecay.automation-run.input.v1",
            operation.use_case_id(),
            context.actor(),
            context.scope(),
            &configuration_digest,
            &request,
            &retirement_binding,
            &reset_source_digest,
        ))?;
        let execution_cancellation = cancellation.clone();
        let execution = RetainedSurfaceExecutionContextV1 {
            request_context: &context,
            cancellation_signal: &execution_cancellation,
            operation: &operation,
            observed_at,
        };
        let prepared = prepare_retained_effect(
            &execution,
            retained_operation,
            &configuration_digest,
            &(&request, &retirement_binding, &reset_source_digest),
            request.run_id.as_str(),
        )
        .map_err(|_| contract_error("canonical memory automation effect preparation failed"))?;
        let placeholder = digest(&"tracedecay.automation-run.uncommitted.v1")?;
        let RetainedSurfaceExecutionErrorV1::PartialEffect {
            mut committed_receipt,
            ..
        } = prepared.partial_error_with_digest(
            &placeholder,
            "application.automation-run.recovery-template",
            "Durable automation recovery receipt template.",
        )
        else {
            return Err(contract_error(
                "canonical memory automation recovery template is unavailable",
            ));
        };
        committed_receipt.committed_state = None;
        let recovery_problem = if matches!(
            task,
            AutomationTaskV1::MemoryCurator | AutomationTaskV1::SessionReflector
        ) {
            reset_required_problem(&operation, &context, &request)?
        } else {
            indeterminate_external_effect_problem(&operation, &context, &request)?
        };
        let recovery = if matches!(
            task,
            AutomationTaskV1::MemoryCurator | AutomationTaskV1::SessionReflector
        ) {
            AutomationRecoveryBinding::Memory {
                owner: memory.project_memory_owner()?,
                recovery_problem,
                retirement: retirement_binding,
                reset_source_digest,
            }
        } else {
            if retirement_binding.is_some() || reset_source_digest.is_some() {
                return Err(contract_error(
                    "external automation admission carried memory retirement state",
                ));
            }
            AutomationRecoveryBinding::External { recovery_problem }
        };
        let effect_authority_digest = recovery_index::effect_authority_digest(
            1,
            &operation,
            &request,
            &input_digest,
            &configuration_digest,
            &context.grant().grant_id,
            context.grant().revision,
            &context.grant().digest,
            &context.grant().disclosure,
            &committed_receipt,
            context.actor(),
            context.scope(),
            context.request_id(),
            &recovery,
        )?;
        let admission = DurableAutomationAdmission {
            schema_version: 1,
            request,
            input_digest,
            configuration_digest,
            effect_authority_digest,
            grant_id: context.grant().grant_id.clone(),
            grant_revision: context.grant().revision,
            grant_digest: context.grant().digest.clone(),
            disclosure: context.grant().disclosure,
            effect_receipt_template: committed_receipt,
            actor: context.actor().clone(),
            scope: context.scope().clone(),
            request_id: context.request_id().clone(),
            process_run_id: crate::runtime_identity::process_run_id().to_owned(),
            recovery,
        };
        let reserve_path = journal_path.clone();
        let requested = admission.clone();
        let index_root = dashboard_root.to_path_buf();
        let index_path = journal_path.clone();
        let indexed = admission.clone();
        let reservation = tokio::task::spawn_blocking(move || {
            reserve_or_replay_indexed_blocking(
                &reserve_path,
                requested,
                || recovery_index::add_pending_blocking(&index_root, &index_path, &indexed),
                || recovery_index::remove_pending_blocking(&index_root, &index_path),
            )
        })
        .await
        .map_err(|error| {
            contract_error(format!("automation reservation writer failed: {error}"))
        })??;
        match reservation {
            ReservationResult::Replay {
                terminal,
                publication,
                retirement,
            } => {
                validate_retirement_binding(&admission, retirement.as_ref())?;
                if let Some(publication) = publication.as_ref() {
                    let published = tracedecay_agent_hosts::automation::run_ledger::publish_staged_run_record_exact(
                        dashboard_root,
                        admission.request.run_id.as_str(),
                        publication,
                    )
                    .await?;
                    if published
                        == tracedecay_agent_hosts::automation::run_ledger::ExactRunPublishOutcome::MissingPayload
                    {
                        return Err(contract_error(
                            "durable automation replay has neither its exact ledger row nor bound spool",
                        ));
                    }
                    let cleanup_path = journal_path.clone();
                    let cleanup_admission = admission.clone();
                    let cleanup_terminal = terminal.clone();
                    let cleanup_publication = publication.clone();
                    tracedecay_agent_hosts::automation::run_ledger::discard_stale_staged_run_record_exact_after_terminal(
                        dashboard_root,
                        admission.request.run_id.as_str(),
                        publication,
                        move || {
                            Ok(classify_durable_settlement_blocking(
                                &cleanup_path,
                                &cleanup_admission,
                                &cleanup_terminal,
                                Some(&cleanup_publication),
                            )?
                            .is_terminal())
                        },
                    )
                    .await?;
                }
                finalize_terminal_housekeeping(
                    dashboard_root,
                    &journal_path,
                    &admission,
                    &terminal,
                    live_retirement,
                )
                .await?;
                Ok(AutomationEffectAdmission::Replay(terminal))
            }
            ReservationResult::Execute { claim, retirement } => {
                validate_retirement_binding(&admission, retirement.as_ref())?;
                let authority = Self {
                    context,
                    cancellation: cancellation.clone(),
                    operation,
                    prepared,
                    admission,
                    journal_path,
                    dashboard_root: dashboard_root.to_path_buf(),
                    _reservation_claim: Some(claim),
                };
                if let Some((_digest, _detail)) = shipped_reset {
                    let problem = shipped_proposal_reset_required_problem(
                        &authority.operation,
                        &authority.context,
                        &authority.admission.request,
                    )?;
                    let terminal = authority
                        .persist_terminal(AutomationSettledTerminal::Problem(problem))
                        .await?;
                    finalize_terminal_housekeeping(
                        dashboard_root,
                        &authority.journal_path,
                        &authority.admission,
                        &terminal,
                        None,
                    )
                    .await?;
                    Ok(AutomationEffectAdmission::Replay(terminal))
                } else if retirement.is_some() {
                    let terminal = authority.settle_retirement().await?;
                    finalize_terminal_housekeeping(
                        dashboard_root,
                        &authority.journal_path,
                        &authority.admission,
                        &terminal,
                        live_retirement,
                    )
                    .await?;
                    Ok(AutomationEffectAdmission::Replay(terminal))
                } else {
                    Ok(AutomationEffectAdmission::Execute(authority))
                }
            }
            ReservationResult::Recover { retirement } => {
                discard_direct_recovery_unbound_spools(dashboard_root, &journal_path, &admission)
                    .await?;
                let authority = Self {
                    context,
                    cancellation: cancellation.clone(),
                    operation,
                    prepared,
                    admission,
                    journal_path,
                    dashboard_root: dashboard_root.to_path_buf(),
                    _reservation_claim: None,
                };
                if authority.admission.is_external() {
                    let terminal = authority
                        .persist_recovered_terminal(AutomationSettledTerminal::Problem(
                            authority.admission.recovery_problem().clone(),
                        ))
                        .await?;
                    finalize_terminal_housekeeping(
                        dashboard_root,
                        &authority.journal_path,
                        &authority.admission,
                        &terminal,
                        None,
                    )
                    .await?;
                    return Ok(AutomationEffectAdmission::Replay(terminal));
                }
                let recovery_cancellation = cancellation.clone();
                let read_control =
                    FactReadControl::new(Arc::new(move || recovery_cancellation.is_cancelled()));
                let recovered = memory
                    .project_memory_application()
                    .await?
                    .project_memory_automation_run_receipts(
                        authority.admission.request.run_id.clone(),
                        &read_control,
                    )
                    .await
                    .map_err(|error| {
                        contract_error(format!(
                            "canonical memory automation receipt recovery failed: {error}"
                        ))
                    })?;
                let committed_receipts =
                    project_recovered_committed_receipts(&authority.admission.request, &recovered)?;
                if retirement.is_some() && !committed_receipts.is_empty() {
                    return Err(contract_error(
                        "proposal retirement recovery found unrelated canonical memory commits",
                    ));
                }
                let terminal = if retirement.is_some() {
                    authority.settle_recovered_retirement().await?
                } else if !committed_receipts.is_empty() {
                    authority
                        .persist_recovered_terminal(recovery_index::recovered_partial_terminal(
                            &authority.admission,
                            committed_receipts,
                            &authority.operation,
                        )?)
                        .await?
                } else if authority.admission.reset_source_digest().is_some() {
                    let problem = shipped_proposal_reset_required_problem(
                        &authority.operation,
                        &authority.context,
                        &authority.admission.request,
                    )?;
                    authority
                        .persist_recovered_terminal(AutomationSettledTerminal::Problem(problem))
                        .await?
                } else {
                    let terminal = AutomationSettledTerminal::Problem(
                        authority.admission.recovery_problem().clone(),
                    );
                    authority.persist_recovered_terminal(terminal).await?
                };
                validate_retirement_binding(&authority.admission, retirement.as_ref())?;
                finalize_terminal_housekeeping(
                    dashboard_root,
                    &authority.journal_path,
                    &authority.admission,
                    &terminal,
                    live_retirement,
                )
                .await?;
                Ok(AutomationEffectAdmission::Replay(terminal))
            }
            ReservationResult::RecoverPrepared {
                terminal,
                publication,
                retirement,
            } => {
                if retirement.is_some() || terminal.is_retirement_terminal() {
                    return Err(contract_error(
                        "proposal retirement cannot carry a prepared run publication",
                    ));
                }
                let authority = Self {
                    context,
                    cancellation: cancellation.clone(),
                    operation,
                    prepared,
                    admission,
                    journal_path,
                    dashboard_root: dashboard_root.to_path_buf(),
                    _reservation_claim: None,
                };
                let terminal = authority
                    .promote_prepared_terminal(terminal, publication)
                    .await?;
                Ok(AutomationEffectAdmission::Replay(terminal))
            }
            ReservationResult::Conflict { terminal } => {
                reservation_conflict_admission(dashboard_root, &journal_path, terminal).await
            }
        }
    }

    fn terminal_for_run(
        &self,
        ledger: &AutomationRunLedgerRecord,
        committed: Option<&AutomationCommittedReceipt>,
    ) -> Result<AutomationSettledTerminal> {
        if ledger.run_id != self.admission.request.run_id.as_str() {
            return Err(contract_error(
                "automation ledger identity changed before settlement",
            ));
        }
        if ledger.task != agent_task_kind(self.admission.request.task_kind()) {
            return Err(contract_error(
                "automation ledger task changed before settlement",
            ));
        }
        let committed_receipts = committed
            .map(|receipt| project_committed_receipts(&self.admission.request, receipt))
            .transpose()?
            .unwrap_or_default();
        if ledger.status == AutomationRunStatus::Failed {
            if ledger.error.as_deref().is_none_or(str::is_empty) {
                return Err(contract_error(
                    "failed automation ledger has no exact error terminal",
                ));
            }
            if !committed_receipts.is_empty() {
                return Err(contract_error(
                    "a failed automation run carried canonical commits without a partial terminal",
                ));
            }
            let problem = self.problem_envelope(
                failed_ledger_problem(&self.context, &self.cancellation, ledger)?,
                Vec::new(),
            )?;
            return Ok(AutomationSettledTerminal::Problem(problem));
        }
        let terminal = match ledger.status {
            AutomationRunStatus::Succeeded => AutomationRunTerminalV1::Completed {
                summary: project_run_summary(
                    self.admission.request.task_kind(),
                    &committed_receipts,
                )?,
            },
            AutomationRunStatus::Skipped => {
                if ledger.error != ledger.fallback_status {
                    return Err(contract_error(
                        "skipped automation ledger reason disagrees with its fallback status",
                    ));
                }
                AutomationRunTerminalV1::Skipped {
                    reason: project_skip_reason(ledger.error.as_deref().ok_or_else(|| {
                        contract_error("skipped automation terminal has no exact reason")
                    })?)?,
                    summary: AutomationRunSummaryV1 {
                        reviewed_count: 0,
                        accepted_count: 0,
                        rejected_count: 0,
                        skipped_count: 1,
                    },
                }
            }
            _ => {
                return Err(contract_error(
                    "successful automation settlement requires a terminal success or skip status",
                ));
            }
        };
        if matches!(terminal, AutomationRunTerminalV1::Skipped { .. })
            && !committed_receipts.is_empty()
        {
            return Err(contract_error(
                "a skipped automation run carried committed receipts",
            ));
        }
        let result = AutomationRunResultV1 {
            run_id: self.admission.request.run_id.clone(),
            task: self.admission.request.task_kind(),
            request_digest: self
                .admission
                .request
                .input_digest()
                .map_err(contract_error)?,
            terminal,
            committed_receipts,
        };
        if !ledger_record_matches_result(ledger, &result) {
            return Err(contract_error(
                "automation run ledger does not match its application terminal",
            ));
        }
        self.success_terminal(result)
    }

    async fn settle_retirement(&self) -> Result<AutomationSettledTerminal> {
        self.persist_success_result(self.retirement_result()?).await
    }

    async fn settle_recovered_retirement(&self) -> Result<AutomationSettledTerminal> {
        let terminal = self.success_terminal(self.retirement_result()?)?;
        self.persist_recovered_terminal(terminal).await
    }

    fn retirement_result(&self) -> Result<AutomationRunResultV1> {
        let reason =
            AutomationSkipReasonV1::from_ledger_reason("shipped_fact_proposal_history_retired")
                .ok_or_else(|| {
                    contract_error("shipped proposal retirement reason is not registered")
                })?;
        Ok(AutomationRunResultV1 {
            run_id: self.admission.request.run_id.clone(),
            task: self.admission.request.task_kind(),
            request_digest: self
                .admission
                .request
                .input_digest()
                .map_err(contract_error)?,
            terminal: AutomationRunTerminalV1::Skipped {
                reason,
                summary: AutomationRunSummaryV1 {
                    reviewed_count: 0,
                    accepted_count: 0,
                    rejected_count: 0,
                    skipped_count: 1,
                },
            },
            committed_receipts: Vec::new(),
        })
    }

    async fn persist_success_result(
        &self,
        result: AutomationRunResultV1,
    ) -> Result<AutomationSettledTerminal> {
        let terminal = self.success_terminal(result)?;
        self.persist_terminal(terminal).await
    }

    fn success_terminal(&self, result: AutomationRunResultV1) -> Result<AutomationSettledTerminal> {
        if !result.matches_terminal() {
            return Err(contract_error(
                "memory automation success terminal is inconsistent",
            ));
        }
        let committed_state = self
            .prepared
            .material_committed_state_digest(&result)
            .map_err(|_| contract_error("memory automation committed state is invalid"))?;
        let execution = RetainedSurfaceExecutionContextV1 {
            request_context: &self.context,
            cancellation_signal: &self.cancellation,
            operation: &self.operation,
            observed_at: tracedecay_application::now_micros(),
        };
        let committed_outer_result = result.clone();
        let outcome = self.prepared.complete_with_digest(
            &execution,
            &committed_state,
            tracedecay_application::ReconciliationState::Reconciled,
            RetainedSurfaceResultV1::FactStoreCurate(result),
            None,
        );
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(RetainedSurfaceExecutionErrorV1::PartialEffect {
                reason_code,
                committed_receipt,
                detail,
            }) => {
                return self
                    .outer_result_partial_problem(
                        reason_code,
                        committed_receipt,
                        detail,
                        committed_outer_result,
                    )
                    .map(AutomationSettledTerminal::Problem);
            }
            Err(_) => {
                return Err(contract_error(
                    "canonical memory automation effect completion failed before a typed post-commit terminal",
                ));
            }
        };
        if !retained_surface_outcome_matches_terminal(
            RetainedSurfaceOperation::FactStoreCurate,
            self.context.request_id(),
            self.context.scope(),
            &outcome,
        ) {
            return Err(contract_error(
                "memory automation outcome does not match its registered admission",
            ));
        }
        Ok(AutomationSettledTerminal::Outcome {
            scope: self.context.scope().clone(),
            outcome,
        })
    }

    fn outer_result_partial_problem(
        &self,
        reason_code: String,
        committed_receipt: tracedecay_application::EffectReceipt,
        detail: String,
        committed_outer_result: AutomationRunResultV1,
    ) -> Result<AutomationSettledProblem> {
        let problem =
            retained_surface_execution_problem(RetainedSurfaceExecutionErrorV1::PartialEffect {
                reason_code,
                committed_receipt,
                detail,
            });
        problem.validate().map_err(contract_error)?;
        let envelope = ApplicationProblemEnvelope::new(
            self.operation.result_contract().clone(),
            self.context.request_id().clone(),
            problem,
        )
        .map(|problem| problem.with_owning_layer(ProblemOwningLayer::Application))
        .map_err(contract_error)?;
        AutomationRunProblemV1::new_outer_effect_partial(
            &self.admission.request,
            self.context.scope().clone(),
            envelope,
            committed_outer_result,
            self.context.request_id(),
        )
        .map_err(contract_error)
    }

    fn validate_failed_ledger(&self, ledger: &AutomationRunLedgerRecord) -> Result<()> {
        if ledger.run_id != self.admission.request.run_id.as_str()
            || ledger.task != agent_task_kind(self.admission.request.task_kind())
            || ledger.status != AutomationRunStatus::Failed
            || ledger.error.as_deref().is_none_or(str::is_empty)
        {
            return Err(contract_error(
                "deferred failed automation ledger does not match its admitted run, task, or error terminal",
            ));
        }
        Ok(())
    }

    fn validate_reused_scheduler_skip(&self, reused: &ReusedSchedulerSkip) -> Result<()> {
        let admitted_task = agent_task_kind(self.admission.request.task_kind());
        let expected_task_key = self
            .admission
            .request
            .task
            .expected_external_task_key()
            .unwrap_or_else(|| task_key(admitted_task).to_owned());
        let prior_task_key = reused
            .prior_record
            .task_key
            .as_deref()
            .unwrap_or_else(|| task_key(reused.prior_record.task));
        if reused.requested_run_id != self.admission.request.run_id.as_str()
            || reused.prior_record.run_id == reused.requested_run_id
            || !matches!(
                admitted_task,
                AgentTaskKind::MemoryCurator
                    | AgentTaskKind::SessionReflector
                    | AgentTaskKind::SkillWriter
            )
            || reused.prior_record.task != admitted_task
            || reused.task_key != expected_task_key
            || prior_task_key != reused.task_key
            || reused.prior_record.trigger
                != tracedecay_agent_hosts::automation::run_ledger::AutomationTrigger::Scheduler
            || reused.prior_record.status != AutomationRunStatus::Skipped
            || reused.prior_record.error != reused.prior_record.fallback_status
            || reused.prior_record.error.as_deref() != Some(reused.reason.as_str())
            || reused.reason.is_empty()
        {
            return Err(contract_error(
                "reused scheduler skip does not match its current admission and exact prior terminal",
            ));
        }
        Ok(())
    }

    pub(crate) async fn abandon_uncommitted(self) -> Result<()> {
        let path = self.journal_path;
        let admission = self.admission;
        let dashboard_root = self.dashboard_root;
        let index_path = path.clone();
        tokio::task::spawn_blocking(move || {
            abandon_reservation_blocking(&path, &admission)?;
            recovery_index::remove_pending_blocking(&dashboard_root, &index_path)
        })
        .await
        .map_err(|error| {
            contract_error(format!("automation reservation rollback failed: {error}"))
        })?
    }

    fn settle_partial(
        &self,
        run_id: &str,
        committed: &AutomationCommittedReceipt,
        detail: &'static str,
    ) -> Result<AutomationSettledProblem> {
        if run_id != self.admission.request.run_id.as_str() {
            return Err(contract_error(
                "automation run identity changed before settlement",
            ));
        }
        let committed_receipts = project_committed_receipts(&self.admission.request, committed)?;
        if committed_receipts.is_empty() {
            return Err(contract_error(
                "zero committed memory effects cannot produce a partial-effect terminal",
            ));
        }
        let committed_state = digest(&(
            "tracedecay.automation-run.partial-state.v1",
            run_id,
            &committed_receipts,
        ))?;
        let problem = retained_surface_execution_problem(self.prepared.partial_error_with_digest(
            &committed_state,
            "application.automation-run.partial-effect",
            detail,
        ));
        if !retained_surface_problem_matches_terminal(
            RetainedSurfaceOperation::FactStoreCurate,
            self.context.request_id(),
            Some(self.context.scope()),
            &problem,
        ) {
            return Err(contract_error(
                "memory automation partial terminal does not match its admission",
            ));
        }
        self.problem_envelope(problem, committed_receipts)
    }

    fn problem_envelope(
        &self,
        problem: ApplicationProblem,
        committed_receipts: Vec<AutomationCommittedReceiptV1>,
    ) -> Result<AutomationSettledProblem> {
        problem.validate().map_err(contract_error)?;
        let problem = ApplicationProblemEnvelope::new(
            self.operation.result_contract().clone(),
            self.context.request_id().clone(),
            problem,
        )
        .map(|problem| problem.with_owning_layer(ProblemOwningLayer::Application))
        .map_err(contract_error)?;
        AutomationRunProblemV1::new(
            &self.admission.request,
            self.context.scope().clone(),
            problem,
            committed_receipts,
            self.context.request_id(),
        )
        .map_err(contract_error)
    }

    async fn persist_terminal(
        &self,
        terminal: AutomationSettledTerminal,
    ) -> Result<AutomationSettledTerminal> {
        let path = self.journal_path.clone();
        let admission = self.admission.clone();
        tokio::task::spawn_blocking(move || persist_terminal_blocking(&path, &admission, terminal))
            .await
            .map_err(|error| {
                contract_error(format!("automation terminal writer failed: {error}"))
            })?
    }

    async fn promote_prepared_terminal(
        &self,
        terminal: AutomationSettledTerminal,
        publication: tracedecay_agent_hosts::automation::run_ledger::ExactRunPublication,
    ) -> Result<AutomationSettledTerminal> {
        let published =
            tracedecay_agent_hosts::automation::run_ledger::publish_staged_run_record_exact(
                &self.dashboard_root,
                self.admission.request.run_id.as_str(),
                &publication,
            )
            .await?;
        if published
            == tracedecay_agent_hosts::automation::run_ledger::ExactRunPublishOutcome::MissingPayload
        {
            return Err(contract_error(
                "prepared automation terminal has neither its spool nor exact ledger row",
            ));
        }
        let path = self.journal_path.clone();
        let admission = self.admission.clone();
        let journal_publication = publication.clone();
        let terminal = tokio::task::spawn_blocking(move || {
            promote_prepared_terminal_blocking(&path, &admission, terminal, &journal_publication)
        })
        .await
        .map_err(|error| {
            contract_error(format!(
                "automation prepared-terminal promotion failed: {error}"
            ))
        })??;
        tracedecay_agent_hosts::automation::run_ledger::discard_staged_run_record_exact(
            &self.dashboard_root,
            self.admission.request.run_id.as_str(),
            &publication,
        )
        .await?;
        finalize_terminal_housekeeping(
            &self.dashboard_root,
            &self.journal_path,
            &self.admission,
            &terminal,
            None,
        )
        .await?;
        Ok(terminal)
    }

    async fn persist_recovered_terminal(
        &self,
        terminal: AutomationSettledTerminal,
    ) -> Result<AutomationSettledTerminal> {
        let path = self.journal_path.clone();
        let admission = self.admission.clone();
        let cancellation = self.cancellation.clone();
        tokio::task::spawn_blocking(move || {
            persist_recovered_terminal_blocking(&path, &admission, terminal, Some(&cancellation))?
                .ok_or_else(|| contract_error("automation recovery settlement was cancelled"))
        })
        .await
        .map_err(|error| {
            contract_error(format!(
                "automation recovery terminal writer failed: {error}"
            ))
        })?
    }
}

fn settle_bound_owner(
    state: RetainedBoundSettlement,
) -> Result<RetainedOwnerValue<(AutomationSettledTerminal, AutomationRunLedgerRecord)>> {
    settle_bound_owner_with_budget(state, RETAINED_SETTLEMENT_RETRY_BUDGET)
}

fn settle_bound_owner_with_budget(
    mut state: RetainedBoundSettlement,
    budget: Duration,
) -> Result<RetainedOwnerValue<(AutomationSettledTerminal, AutomationRunLedgerRecord)>> {
    let started = std::time::Instant::now();
    let mut delay = Duration::from_millis(25);
    loop {
        let error = match settle_bound_once(&mut state) {
            Ok(()) => return Ok(complete_bound_settlement(state)),
            Err(error) => {
                match classify_bound_settlement(&state) {
                    Ok(classification)
                        if classification.is_terminal() && state.publication.is_some() =>
                    {
                        tracing::warn!(
                            run_id = %state.ledger.run_id,
                            error = %error,
                            "automation settlement reached its exact terminal with deferred housekeeping"
                        );
                        cleanup_bound_terminal(&state);
                        return Ok(complete_bound_settlement(state));
                    }
                    Ok(_) => tracing::warn!(
                        run_id = %state.ledger.run_id,
                        error = %error,
                        "automation finalization remains pending under its blocking owner"
                    ),
                    Err(classification_error) => tracing::warn!(
                        run_id = %state.ledger.run_id,
                        error = %error,
                        classification_error = %classification_error,
                        "automation finalization remains uncertain under its blocking owner"
                    ),
                }
                error
            }
        };
        if state.authority.cancellation.is_cancelled() {
            return Err(contract_error(format!(
                "retained automation settlement for run '{}' was cancelled while its blocking owner retried; state remains recoverable: {error}",
                state.ledger.run_id
            )));
        }
        if started.elapsed() >= budget {
            return Err(contract_error(format!(
                "retained automation settlement for run '{}' exceeded its retry budget; state remains recoverable: {error}",
                state.ledger.run_id
            )));
        }
        std::thread::sleep(delay);
        delay = delay.saturating_mul(2).min(Duration::from_secs(5));
    }
}

fn settle_bound_once(state: &mut RetainedBoundSettlement) -> Result<()> {
    if state.publication.is_none() {
        #[cfg(test)]
        let prepared_write_hook = state.prepared_write_hook.clone();
        let bound = tracedecay_agent_hosts::automation::run_ledger::bind_staged_run_record_exact(
            &state.authority.dashboard_root,
            &state.ledger,
            |publication| {
                #[cfg(test)]
                if let Some(hook) = prepared_write_hook.as_ref() {
                    hook.before_write(publication)?;
                }
                let first = persist_prepared_terminal_blocking(
                    &state.authority.journal_path,
                    &state.authority.admission,
                    &state.terminal,
                    publication.clone(),
                );
                match first {
                    Ok(()) => Ok(()),
                    Err(first_error) => replay_exact_binding_after_error_blocking(
                        &state.authority.journal_path,
                        &state.authority.admission,
                        &state.terminal,
                        publication,
                    )?
                    .map(|_| ())
                    .ok_or(first_error),
                }
            },
        );
        match bound {
            Ok((publication, ())) => {
                state.publication = Some(publication);
                #[cfg(test)]
                if let Some(phase_hook) = state.phase_hook.as_ref() {
                    phase_hook.notify(RetainedSettlementPhase::Prepared);
                }
            }
            Err(error) => {
                // A staged payload identifies only captured bytes. Until the
                // bind callback returns successfully, the journal has not
                // proven that exact publication as Prepared. Leave it
                // unbound so this owner re-enters the canonical bind path,
                // which reuses the digest-owned spool without publishing it.
                state.publication = None;
                #[cfg(test)]
                if let Some(phase_hook) = state.phase_hook.as_ref() {
                    phase_hook.notify(RetainedSettlementPhase::PreparedWriteFailed);
                }
                return Err(error.into());
            }
        }
    }
    let publication = state
        .publication
        .as_ref()
        .ok_or_else(|| contract_error("prepared settlement lost its exact publication"))?;
    let published =
        tracedecay_agent_hosts::automation::run_ledger::publish_staged_run_record_exact_blocking(
            &state.authority.dashboard_root,
            state.authority.admission.request.run_id.as_str(),
            publication,
        )?;
    if published == ExactRunPublishOutcome::MissingPayload {
        return Err(contract_error(
            "prepared automation terminal has neither its spool nor exact ledger row",
        ));
    }
    #[cfg(test)]
    {
        if let Some(phase_hook) = state.phase_hook.as_ref() {
            phase_hook.notify(RetainedSettlementPhase::Published);
        }
    }
    state.terminal = promote_prepared_terminal_blocking(
        &state.authority.journal_path,
        &state.authority.admission,
        state.terminal.clone(),
        publication,
    )?;
    cleanup_bound_terminal(state);
    Ok(())
}

fn classify_bound_settlement(
    state: &RetainedBoundSettlement,
) -> Result<DurableSettlementClassification> {
    classify_durable_settlement_blocking(
        &state.authority.journal_path,
        &state.authority.admission,
        &state.terminal,
        state.publication.as_ref(),
    )
}

fn cleanup_bound_terminal(state: &RetainedBoundSettlement) {
    if let Some(publication) = state.publication.as_ref() {
        if let Err(error) =
            tracedecay_agent_hosts::automation::run_ledger::discard_staged_run_record_exact_blocking(
                &state.authority.dashboard_root,
                state.authority.admission.request.run_id.as_str(),
                publication,
            )
        {
            tracing::warn!(
                run_id = %state.ledger.run_id,
                error = %error,
                "exact automation terminal is committed; spool cleanup remains recoverable"
            );
            return;
        }
    }
    if let Err(error) = recovery_index::remove_pending_blocking(
        &state.authority.dashboard_root,
        &state.authority.journal_path,
    ) {
        tracing::warn!(
            run_id = %state.ledger.run_id,
            error = %error,
            "exact automation terminal is committed; pending-index cleanup remains recoverable"
        );
    }
}

fn complete_bound_settlement(
    state: RetainedBoundSettlement,
) -> RetainedOwnerValue<(AutomationSettledTerminal, AutomationRunLedgerRecord)> {
    let RetainedBoundSettlement {
        authority,
        guard,
        terminal,
        ledger,
        publication: _,
        observer,
        #[cfg(test)]
            phase_hook: _,
        #[cfg(test)]
            prepared_write_hook: _,
    } = state;
    drop(authority);
    let pair_keepalive = match guard {
        RetainedSettlementGuardOwner::Single(guard) => {
            drop(guard);
            None
        }
        RetainedSettlementGuardOwner::Pair(guards) => Some(guards),
    };
    observe_automation_ledger(observer, &ledger);
    RetainedOwnerValue {
        value: (terminal, ledger),
        pair_keepalive,
    }
}

fn observe_automation_ledger(
    observer: Option<AutomationLedgerObserver>,
    ledger: &AutomationRunLedgerRecord,
) {
    if let Some(observer) = observer
        && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer(ledger))).is_err()
    {
        tracing::error!(
            run_id = %ledger.run_id,
            "automation ledger observer panicked after exact settlement"
        );
    }
}

fn settle_direct_owner(
    state: RetainedDirectSettlement,
) -> Result<RetainedOwnerValue<AutomationSettledTerminal>> {
    settle_direct_owner_with_budget(state, RETAINED_SETTLEMENT_RETRY_BUDGET)
}

fn settle_direct_owner_with_budget(
    mut state: RetainedDirectSettlement,
    budget: Duration,
) -> Result<RetainedOwnerValue<AutomationSettledTerminal>> {
    let started = std::time::Instant::now();
    let mut delay = Duration::from_millis(25);
    loop {
        let error = match settle_direct_once(&mut state) {
            Ok(()) => return Ok(complete_direct_settlement(state)),
            Err(error) => {
                match classify_durable_settlement_blocking(
                    &state.authority.journal_path,
                    &state.authority.admission,
                    &state.terminal,
                    None,
                ) {
                    Ok(classification) if classification.is_terminal() => {
                        tracing::warn!(error = %error, "direct automation terminal committed with deferred housekeeping");
                        cleanup_direct_terminal(&state);
                        return Ok(complete_direct_settlement(state));
                    }
                    Ok(_) => {
                        tracing::warn!(error = %error, "direct automation finalization remains pending under its blocking owner")
                    }
                    Err(classification_error) => tracing::warn!(
                        error = %error,
                        classification_error = %classification_error,
                        "direct automation finalization remains uncertain under its blocking owner"
                    ),
                }
                error
            }
        };
        if state.authority.cancellation.is_cancelled() {
            return Err(contract_error(format!(
                "direct automation settlement for run '{}' was cancelled while its blocking owner retried; state remains recoverable: {error}",
                state.authority.admission.request.run_id
            )));
        }
        if started.elapsed() >= budget {
            return Err(contract_error(format!(
                "direct automation settlement for run '{}' exceeded its retry budget; state remains recoverable: {error}",
                state.authority.admission.request.run_id
            )));
        }
        std::thread::sleep(delay);
        delay = delay.saturating_mul(2).min(Duration::from_secs(5));
    }
}

fn settle_direct_once(state: &mut RetainedDirectSettlement) -> Result<()> {
    state.terminal = persist_terminal_blocking(
        &state.authority.journal_path,
        &state.authority.admission,
        state.terminal.clone(),
    )?;
    cleanup_direct_terminal(state);
    Ok(())
}

fn cleanup_direct_terminal(state: &RetainedDirectSettlement) {
    if let Err(error) = recovery_index::remove_pending_blocking(
        &state.authority.dashboard_root,
        &state.authority.journal_path,
    ) {
        tracing::warn!(
            run_id = %state.authority.admission.request.run_id,
            error = %error,
            "direct automation terminal is committed; pending-index cleanup remains recoverable"
        );
    }
}

fn complete_direct_settlement(
    state: RetainedDirectSettlement,
) -> RetainedOwnerValue<AutomationSettledTerminal> {
    let RetainedDirectSettlement {
        authority,
        guard,
        terminal,
    } = state;
    drop(authority);
    let pair_keepalive = match guard {
        RetainedSettlementGuardOwner::Single(guard) => {
            drop(guard);
            None
        }
        RetainedSettlementGuardOwner::Pair(guards) => Some(guards),
    };
    RetainedOwnerValue {
        value: terminal,
        pair_keepalive,
    }
}

fn abandon_retained_owner(state: RetainedAbandonment) -> Result<RetainedOwnerValue<()>> {
    abandon_retained_owner_with_budget(state, RETAINED_SETTLEMENT_RETRY_BUDGET)
}

fn abandon_retained_owner_with_budget(
    mut state: RetainedAbandonment,
    budget: Duration,
) -> Result<RetainedOwnerValue<()>> {
    let started = std::time::Instant::now();
    let mut delay = Duration::from_millis(25);
    loop {
        match abandon_retained_once(&mut state) {
            Ok(()) => {
                let RetainedAbandonment {
                    authority,
                    guard,
                    reservation_abandoned: _,
                } = state;
                drop(authority);
                let pair_keepalive = match guard {
                    RetainedSettlementGuardOwner::Single(guard) => {
                        drop(guard);
                        None
                    }
                    RetainedSettlementGuardOwner::Pair(guards) => Some(guards),
                };
                return Ok(RetainedOwnerValue {
                    value: (),
                    pair_keepalive,
                });
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "automation abandonment remains pending under its blocking owner"
                );
                if state.authority.cancellation.is_cancelled() {
                    return Err(contract_error(format!(
                        "automation abandonment for run '{}' was cancelled while its blocking owner retried; state remains recoverable: {error}",
                        state.authority.admission.request.run_id
                    )));
                }
                if started.elapsed() >= budget {
                    return Err(contract_error(format!(
                        "automation abandonment for run '{}' exceeded its retry budget; state remains recoverable: {error}",
                        state.authority.admission.request.run_id
                    )));
                }
            }
        }
        std::thread::sleep(delay);
        delay = delay.saturating_mul(2).min(Duration::from_secs(5));
    }
}

fn abandon_retained_once(state: &mut RetainedAbandonment) -> Result<()> {
    if !state.reservation_abandoned {
        if let Err(error) =
            abandon_reservation_blocking(&state.authority.journal_path, &state.authority.admission)
        {
            match std::fs::symlink_metadata(&state.authority.journal_path) {
                Err(metadata_error) if metadata_error.kind() == std::io::ErrorKind::NotFound => {
                    tracedecay_application::sync_parent_directory(
                        &state.authority.journal_path,
                        tracedecay_application::DirectorySyncPolicy::Strict,
                    )
                    .map_err(|sync_error| {
                        contract_error(format!(
                            "{error}; automation abandonment absence resync failed: {sync_error}"
                        ))
                    })?;
                    tracing::warn!(
                        run_id = %state.authority.admission.request.run_id,
                        error = %error,
                        "automation reservation removal was recovered by durable absence resnapshot"
                    );
                }
                Ok(_) | Err(_) => return Err(error),
            }
        }
        state.reservation_abandoned = true;
    }
    recovery_index::remove_pending_blocking(
        &state.authority.dashboard_root,
        &state.authority.journal_path,
    )
}

async fn discard_direct_recovery_unbound_spools(
    dashboard_root: &Path,
    journal_path: &Path,
    admission: &DurableAutomationAdmission,
) -> Result<()> {
    let cleanup_path = journal_path.to_path_buf();
    let cleanup_admission = admission.clone();
    let outcome =
        tracedecay_agent_hosts::automation::run_ledger::discard_unbound_staged_run_records_if(
            dashboard_root,
            admission.request.run_id.as_str(),
            move || {
                journal::unbound_reserved_cleanup_is_safe_blocking(
                    &cleanup_path,
                    &cleanup_admission,
                )
            },
        )
        .await?;
    match outcome {
        tracedecay_agent_hosts::automation::run_ledger::ExactRunUnboundDiscardOutcome::Discarded => {
            Ok(())
        }
        tracedecay_agent_hosts::automation::run_ledger::ExactRunUnboundDiscardOutcome::Retained => {
            Err(contract_error(
                "direct automation recovery changed state before unbound spool cleanup",
            ))
        }
    }
}

async fn reservation_conflict_admission(
    _dashboard_root: &Path,
    _journal_path: &Path,
    _terminal: bool,
) -> Result<AutomationEffectAdmission> {
    // A conflicting caller does not own the existing terminal's retirement or
    // staged-publication cleanup proof. Project recovery may retire an exact
    // stale index entry, but this admission must not erase that authority.
    Ok(AutomationEffectAdmission::Conflict)
}

fn validate_retirement_binding(
    admission: &DurableAutomationAdmission,
    classified: Option<&retirement::RetirementBinding>,
) -> Result<()> {
    if admission.retirement() == classified {
        Ok(())
    } else {
        Err(contract_error(
            "automation retirement classification changed after durable admission",
        ))
    }
}

async fn finalize_terminal_housekeeping(
    dashboard_root: &Path,
    journal_path: &Path,
    admission: &DurableAutomationAdmission,
    terminal: &AutomationSettledTerminal,
    live_retirement: Option<retirement::RetirementPlan>,
) -> Result<()> {
    let retirement_binding = match (
        admission.retirement().cloned(),
        terminal.is_retirement_terminal(),
    ) {
        (Some(binding), true) => Some(binding),
        (Some(_), false) if terminal.problem().is_some() => None,
        (Some(_), false) => {
            return Err(contract_error(
                "retirement-bound automation terminal is neither its exact zero-effect retirement nor a typed problem",
            ));
        }
        (None, true) => {
            return Err(contract_error(
                "automation retirement terminal has no admitted source binding",
            ));
        }
        (None, false) if live_retirement.is_some() => {
            return Err(contract_error(
                "live retirement plan has no durable admission binding",
            ));
        }
        (None, false) => None,
    };
    finalize_terminal_housekeeping_owned(
        dashboard_root,
        journal_path,
        admission.clone(),
        retirement_binding,
        live_retirement,
    )
    .await
}
