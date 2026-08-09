//! Typed run-control operations over the durable Work run-control aggregate.
//!
//! Plan 32 (`docs/plans/tracedecay-v2/32-dynamic-workflow-runtime-and-sdk.md`,
//! "Application operations and surfaces") lists
//! "pause/resume/cancel/retry/reconcile" among the operations the advanced
//! workflow delivery retains, and "One runtime, run control, and effect budget"
//! states that "pause and cancellation fence new reservations and reconcile
//! active effects before publishing a stable state".
//!
//! Cancel already exists as an attempt-level authority
//! ([`crate::WorkAttemptService::request_cancellation`]); this module adds the
//! run-level half — pause, resume, and the read that lets a caller see the
//! published control state without guessing it from attempt rows.
//!
//! The service owns three decisions the surfaces must not re-make:
//!
//! * **A run is known only through its attempts.** There is no separate "run"
//!   row to create, so pausing a run nobody ever leased an attempt for is
//!   `not_found_or_not_authorized`, not an empty success. The admitted deadline
//!   the aggregate is measured against is read from the attempt's own pinned
//!   execution snapshot, never supplied by the caller — a caller-supplied
//!   deadline would be a way to buy budget.
//! * **Reconciliation before publication.** A pause records the exact live
//!   attempt frontier it fenced. Attempts already running are not killed by a
//!   pause (that is cancellation's job and it has its own receipt); the pause
//!   fences *new* reservations and states what was in flight.
//! * **Version-checked control.** Every transition may carry the authority
//!   version the caller believed it was acting on. A stale version conflicts
//!   instead of overwriting a concurrent transition.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    AttemptId, RunId, TaskId, UtcMicros, WorkAuthority, WorkBlockedIntervalCauseV1,
    WorkBlockedIntervalClosureV1, WorkBlockedIntervalIdentityV1, WorkBlockedIntervalReceiptV1,
    WorkRunControlAuthorityV1, WorkRunControlContractError, WorkRunControlReasonV1,
    WorkRunControlV1, WorkflowStepId,
};

use crate::work::work_authority;
use crate::{
    ApplicationProblem, LegalAction, RequestAdmission, RequestContext, RetryDirective,
    SafeDiagnostic,
};

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum WorkRunControlStorageError {
    #[error("the Work run control authority is unavailable")]
    Unavailable,
    #[error("the Work run control row is not present or not authorized")]
    NotFoundOrNotAuthorized,
    #[error("the Work run control authority version changed")]
    AuthorityConflict,
}

/// What the durable attempt rows say about one run.
///
/// This is the only evidence the run-control aggregate is derived from, and
/// every field is read from a persisted attempt: nothing here is estimated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkRunAdmissionV1 {
    /// The admitted absolute deadline, taken from the pinned execution
    /// snapshot of the run's earliest attempt.
    pub deadline: UtcMicros,
    /// The attempts of this run that have not reached a terminal state, in
    /// stable attempt-id order.
    pub live_attempts: Vec<AttemptId>,
    /// Every attempt this run ever durably held.
    pub total_attempts: u32,
}

/// One live attempt the run-control authority may fence.
///
/// `step_id` comes from the canonical workflow journal fan-out binding. It is
/// intentionally required: a provider operation name is not interchangeable
/// with a workflow step, and an interval without that binding cannot become
/// product observability evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkRunLiveAttemptV1 {
    pub attempt_id: AttemptId,
    /// `None` is an ordinary Work attempt outside a workflow journal. It
    /// remains controllable, but cannot fabricate a workflow-step interval.
    pub step_id: Option<WorkflowStepId>,
}

/// Exact durable evidence a run-control transition was prepared from.
///
/// Storage must acquire this snapshot from one read transaction and compare it
/// again inside the write transaction that publishes the transition. That
/// closes the gap where an attempt could become terminal (or a new attempt
/// could be admitted) after pause selected its frontier but before the control
/// row and blocked intervals committed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkRunControlFrontierV1 {
    pub admission: WorkRunAdmissionV1,
    pub control: Option<WorkRunControlV1>,
    pub open_blocked_intervals: Vec<WorkBlockedIntervalReceiptV1>,
}

/// One control transition together with the interval receipts committed in
/// its same storage transaction. The returned receipts are the only facts a
/// transport may offer to observability; a command input is never evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkRunControlTransitionReceiptV1 {
    pub control: WorkRunControlV1,
    pub blocked_intervals: Vec<WorkBlockedIntervalReceiptV1>,
}

/// The durable run-control rows and the attempt evidence they are derived
/// from.
pub trait WorkRunControlStoragePort: Send + Sync {
    /// Reads all mutable evidence needed by a pause or resume from one storage
    /// snapshot. `None` means the run has no durable attempt.
    fn run_control_frontier(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<Option<WorkRunControlFrontierV1>, WorkRunControlStorageError>;

    /// The admitted deadline and live attempt frontier for one run, or `None`
    /// when the run holds no durable attempt at all.
    fn run_admission(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<Option<WorkRunAdmissionV1>, WorkRunControlStorageError>;

    /// Resolves the canonical workflow journal binding for every durable
    /// attempt of one run. Journal replay happens only while a pause is about
    /// to create interval evidence, never on ordinary reads or reservations.
    fn workflow_bound_live_attempts(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<Vec<WorkRunLiveAttemptV1>, WorkRunControlStorageError>;

    /// The published control row for one run, or `None` when the run has never
    /// been controlled.
    fn load_run_control(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<Option<WorkRunControlV1>, WorkRunControlStorageError>;

    /// Publishes `next` under a compare-and-swap on the authority version the
    /// caller read. `expected` is `None` only for the first publication.
    fn publish_run_control(
        &self,
        authority: &WorkAuthority,
        expected: Option<WorkRunControlAuthorityV1>,
        next: &WorkRunControlV1,
        blocked_intervals: &[WorkBlockedIntervalReceiptV1],
    ) -> Result<(), WorkRunControlStorageError>;

    /// Publishes only while the complete mutable frontier is still identical
    /// to `expected`. Implementations must perform the comparison and control
    /// CAS in the same write transaction.
    fn publish_run_control_at_frontier(
        &self,
        authority: &WorkAuthority,
        expected: &WorkRunControlFrontierV1,
        next: &WorkRunControlV1,
        blocked_intervals: &[WorkBlockedIntervalReceiptV1],
    ) -> Result<(), WorkRunControlStorageError>;

    /// The still-open receipts for one exact run. Resume closes precisely these
    /// rows under the same control compare-and-swap; it does not reconstruct
    /// an interval from a current clock or current live-attempt query.
    fn open_blocked_intervals(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<Vec<WorkBlockedIntervalReceiptV1>, WorkRunControlStorageError>;

    /// The next bounded, cyclic recovery page of settled receipts.
    ///
    /// A page advances an independent durable scan cursor before retained
    /// recovery tries the producer. It is not itself delivery acknowledgement:
    /// only the exact receipt whose owner fact was durably claimed leaves
    /// later cycles; every unmarked receipt remains eligible after wraparound.
    fn next_settled_blocked_intervals_for_observation(
        &self,
        authority: &WorkAuthority,
        limit: u32,
    ) -> Result<Vec<WorkBlockedIntervalReceiptV1>, WorkRunControlStorageError>;

    /// Marks one exact receipt only after the retained producer path has
    /// durably claimed the matching owner fact. A synchronous enqueue is not
    /// sufficient evidence for this transition.
    fn mark_settled_blocked_interval_durable(
        &self,
        authority: &WorkAuthority,
        receipt: &WorkBlockedIntervalReceiptV1,
    ) -> Result<(), WorkRunControlStorageError>;
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(title = "PauseWorkRunCommand")]
pub struct PauseWorkRunCommand {
    pub task_id: TaskId,
    pub run_id: RunId,
    pub reason: WorkRunControlReasonV1,
    /// The authority version the caller read. Absent means "no control row was
    /// published yet"; a mismatch is a conflict, never an overwrite.
    #[serde(default)]
    pub expected_authority_version: Option<u64>,
    pub occurred_at: UtcMicros,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(title = "ResumeWorkRunCommand")]
pub struct ResumeWorkRunCommand {
    pub task_id: TaskId,
    pub run_id: RunId,
    pub reason: WorkRunControlReasonV1,
    pub expected_authority_version: u64,
    pub occurred_at: UtcMicros,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(title = "WorkRunControlRequestV1")]
pub struct WorkRunControlRequestV1 {
    pub task_id: TaskId,
    pub run_id: RunId,
}

/// One run's control reading.
///
/// `Uncontrolled` is a distinct answer from `Controlled`: it says the run is
/// admitted and running under its admitted deadline with no control transition
/// ever published, which is not the same as a control row that happens to say
/// `Running`.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(title = "WorkRunControlReadingV1")]
pub enum WorkRunControlReadingV1 {
    /// The run holds durable attempts but no control transition has ever been
    /// published for it.
    Uncontrolled {
        /// The admitted deadline the run is running against.
        deadline: UtcMicros,
        /// Attempts that have not reached a terminal state.
        live_attempts: Vec<AttemptId>,
        total_attempts: u32,
    },
    /// The published control aggregate, plus the live frontier as of this
    /// read (which may differ from the frontier the transition fenced).
    Controlled {
        control: WorkRunControlV1,
        live_attempts: Vec<AttemptId>,
        total_attempts: u32,
    },
}

impl WorkRunControlReadingV1 {
    /// Whether a new attempt reservation may be admitted right now.
    pub fn admits_reservation(&self) -> bool {
        match self {
            Self::Uncontrolled { .. } => true,
            Self::Controlled { control, .. } => control.admits_reservation(),
        }
    }
}

/// The pause/resume authority for admitted Work runs.
pub struct WorkRunControlService<S> {
    storage: S,
}

impl<S> WorkRunControlService<S>
where
    S: WorkRunControlStoragePort,
{
    pub const fn new(storage: S) -> Self {
        Self { storage }
    }

    /// Fences new reservations for one run.
    pub fn pause(
        &self,
        context: &RequestContext,
        command: PauseWorkRunCommand,
    ) -> Result<WorkRunControlV1, ApplicationProblem> {
        self.pause_with_receipt(context, command)
            .map(|receipt| receipt.control)
    }

    /// Fences new reservations and returns the exact interval receipts that
    /// committed with the run-control compare-and-swap.
    pub fn pause_with_receipt(
        &self,
        context: &RequestContext,
        command: PauseWorkRunCommand,
    ) -> Result<WorkRunControlTransitionReceiptV1, ApplicationProblem> {
        admit(context, command.occurred_at)?;
        let authority = work_authority(context)?;
        let frontier = self
            .storage
            .run_control_frontier(&authority, &command.task_id, &command.run_id)
            .map_err(storage_problem)?
            .ok_or_else(not_found_problem)?;
        check_expected(
            frontier.control.as_ref(),
            expected_authority(command.expected_authority_version)?,
        )?;

        // The compare-and-swap expectation is what storage currently holds,
        // which `check_expected` has just proved is what the caller read.
        let workflow_attempts = self
            .storage
            .workflow_bound_live_attempts(&authority, &command.task_id, &command.run_id)
            .map_err(storage_problem)?;
        let workflow_steps =
            workflow_steps_for_live_attempts(&frontier.admission.live_attempts, workflow_attempts)?;
        let current = match frontier.control.clone() {
            Some(control) => control,
            None => WorkRunControlV1::admitted(
                command.task_id.clone(),
                command.run_id.clone(),
                frontier.admission.deadline,
                command.occurred_at,
            )
            .map_err(contract_problem)?,
        };
        // A run that was never controlled publishes the paused aggregate
        // directly; writing an intermediate `Running` row first would claim a
        // transition that never happened.
        let next = current
            .pause(
                command.reason,
                command.occurred_at,
                frontier.admission.live_attempts.clone(),
            )
            .map_err(contract_problem)?;
        let cause = WorkBlockedIntervalCauseV1::new(command.reason, next.authority());
        let blocked_intervals = workflow_steps
            .into_iter()
            .filter_map(|attempt| {
                let step_id = attempt.step_id?;
                Some(WorkBlockedIntervalReceiptV1::opened(
                    WorkBlockedIntervalIdentityV1::new(
                        command.task_id.clone(),
                        command.run_id.clone(),
                        attempt.attempt_id,
                        step_id,
                    ),
                    cause,
                    command.occurred_at,
                ))
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(contract_problem)?;
        self.storage
            .publish_run_control_at_frontier(&authority, &frontier, &next, &blocked_intervals)
            .map_err(storage_problem)?;
        Ok(WorkRunControlTransitionReceiptV1 {
            control: next,
            blocked_intervals,
        })
    }

    /// Readmits reservations for one paused run.
    pub fn resume(
        &self,
        context: &RequestContext,
        command: ResumeWorkRunCommand,
    ) -> Result<WorkRunControlV1, ApplicationProblem> {
        self.resume_with_receipt(context, command)
            .map(|receipt| receipt.control)
    }

    /// Readmits reservations and returns the settled receipts that committed
    /// with the authority transition.
    pub fn resume_with_receipt(
        &self,
        context: &RequestContext,
        command: ResumeWorkRunCommand,
    ) -> Result<WorkRunControlTransitionReceiptV1, ApplicationProblem> {
        admit(context, command.occurred_at)?;
        let authority = work_authority(context)?;
        let frontier = self
            .storage
            .run_control_frontier(&authority, &command.task_id, &command.run_id)
            .map_err(storage_problem)?
            .ok_or_else(not_found_problem)?;
        let current = frontier.control.clone().ok_or_else(|| {
            // A run that was never paused has nothing to resume, and
            // answering "resumed" would be a false receipt.
            conflict_problem(
                "application.work-run-control.not-paused",
                "The Work run has no published control state to resume.",
            )
        })?;
        let expected = WorkRunControlAuthorityV1::new(command.expected_authority_version)
            .map_err(contract_problem)?;
        if current.authority() != expected {
            return Err(authority_conflict_problem());
        }
        let next = current
            .resume(command.reason, command.occurred_at)
            .map_err(contract_problem)?;
        let blocked_intervals = frontier
            .open_blocked_intervals
            .iter()
            .cloned()
            .map(|receipt| {
                receipt.close(
                    command.occurred_at,
                    WorkBlockedIntervalClosureV1::Resumed {
                        reason: command.reason,
                        authority: next.authority(),
                    },
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(contract_problem)?;
        self.storage
            .publish_run_control_at_frontier(&authority, &frontier, &next, &blocked_intervals)
            .map_err(storage_problem)?;
        Ok(WorkRunControlTransitionReceiptV1 {
            control: next,
            blocked_intervals,
        })
    }

    /// Reads the published control state for one run.
    pub fn read(
        &self,
        context: &RequestContext,
        request: &WorkRunControlRequestV1,
    ) -> Result<WorkRunControlReadingV1, ApplicationProblem> {
        let authority = work_authority(context)?;
        let admission = self.require_admission(&authority, &request.task_id, &request.run_id)?;
        let control = self
            .storage
            .load_run_control(&authority, &request.task_id, &request.run_id)
            .map_err(storage_problem)?;
        Ok(match control {
            Some(control) => WorkRunControlReadingV1::Controlled {
                control,
                live_attempts: admission.live_attempts,
                total_attempts: admission.total_attempts,
            },
            None => WorkRunControlReadingV1::Uncontrolled {
                deadline: admission.deadline,
                live_attempts: admission.live_attempts,
                total_attempts: admission.total_attempts,
            },
        })
    }

    /// Refuses a new attempt reservation while the run is paused.
    ///
    /// This is the fence Plan 32 requires: "pause and cancellation fence new
    /// reservations". A run with no control row has never been paused, so it
    /// admits — the absence of a control row is not a denial.
    pub fn admit_reservation(
        &self,
        context: &RequestContext,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<(), ApplicationProblem> {
        let authority = work_authority(context)?;
        let control = self
            .storage
            .load_run_control(&authority, task_id, run_id)
            .map_err(storage_problem)?;
        match control {
            Some(control) if !control.admits_reservation() => Err(conflict_problem(
                "application.work-run-control.paused",
                "The Work run is paused, so no new attempt reservation is admitted.",
            )),
            Some(_) | None => Ok(()),
        }
    }

    /// Reads the next bounded, cyclic recovery page of settled interval
    /// receipts. It skips only receipts whose exact owner fact was durably
    /// claimed by the retained producer; ordinary bounded queue offers leave
    /// the source receipt eligible for recovery.
    pub fn next_settled_blocked_intervals_for_observation(
        &self,
        context: &RequestContext,
        limit: u32,
    ) -> Result<Vec<WorkBlockedIntervalReceiptV1>, ApplicationProblem> {
        if limit == 0 || limit > 128 {
            return Err(invalid_pending_interval_limit_problem());
        }
        let authority = work_authority(context)?;
        self.storage
            .next_settled_blocked_intervals_for_observation(&authority, limit)
            .map_err(storage_problem)
    }

    /// Commits the durable-delivery marker after the retained producer claimed
    /// this exact receipt. Public request paths intentionally never call it.
    pub fn mark_settled_blocked_interval_durable(
        &self,
        context: &RequestContext,
        receipt: &WorkBlockedIntervalReceiptV1,
    ) -> Result<(), ApplicationProblem> {
        if !receipt.is_settled() {
            return Err(invalid_open_interval_durable_problem());
        }
        let authority = work_authority(context)?;
        self.storage
            .mark_settled_blocked_interval_durable(&authority, receipt)
            .map_err(storage_problem)
    }

    fn require_admission(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<WorkRunAdmissionV1, ApplicationProblem> {
        self.storage
            .run_admission(authority, task_id, run_id)
            .map_err(storage_problem)?
            .ok_or_else(not_found_problem)
    }
}

fn expected_authority(
    value: Option<u64>,
) -> Result<Option<WorkRunControlAuthorityV1>, ApplicationProblem> {
    value
        .map(|value| WorkRunControlAuthorityV1::new(value).map_err(contract_problem))
        .transpose()
}

/// Refuses a transition whose caller read a different authority version than
/// the one durably published.
fn check_expected(
    existing: Option<&WorkRunControlV1>,
    expected: Option<WorkRunControlAuthorityV1>,
) -> Result<(), ApplicationProblem> {
    match (existing.map(WorkRunControlV1::authority), expected) {
        (Some(current), Some(expected)) if current == expected => Ok(()),
        (None, None) => Ok(()),
        _ => Err(authority_conflict_problem()),
    }
}

fn admit(context: &RequestContext, observed_at: UtcMicros) -> Result<(), ApplicationProblem> {
    match context.admission_at(observed_at) {
        RequestAdmission::Admitted => Ok(()),
        RequestAdmission::Cancelled => Err(ApplicationProblem::cancelled_before_admission()),
        RequestAdmission::TimedOut => Err(ApplicationProblem::timed_out_before_admission()),
    }
}

fn storage_problem(error: WorkRunControlStorageError) -> ApplicationProblem {
    match error {
        WorkRunControlStorageError::NotFoundOrNotAuthorized => not_found_problem(),
        WorkRunControlStorageError::AuthorityConflict => authority_conflict_problem(),
        WorkRunControlStorageError::Unavailable => {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "application.work-run-control.storage-unavailable".to_owned(),
                message: "The Work run control authority is unavailable.".to_owned(),
            })
        }
    }
}

fn contract_problem(error: WorkRunControlContractError) -> ApplicationProblem {
    match error {
        WorkRunControlContractError::AlreadyPaused => conflict_problem(
            "application.work-run-control.already-paused",
            "The Work run is already paused.",
        ),
        WorkRunControlContractError::NotPaused => conflict_problem(
            "application.work-run-control.not-paused",
            "The Work run is not paused.",
        ),
        WorkRunControlContractError::NonMonotonicTransition => conflict_problem(
            "application.work-run-control.non-monotonic",
            "The Work run control transition is older than the published state.",
        ),
        WorkRunControlContractError::InvalidAuthorityVersion
        | WorkRunControlContractError::AuthorityVersionOverflow
        | WorkRunControlContractError::InvalidDeadlineCheckpoint
        | WorkRunControlContractError::TooManyFencedAttempts
        | WorkRunControlContractError::DuplicateFencedAttempt
        | WorkRunControlContractError::InvalidBlockedIntervalRevision
        | WorkRunControlContractError::InvalidBlockedIntervalClosure => {
            ApplicationProblem::InvalidRequest {
                diagnostic: SafeDiagnostic {
                    code: "application.work-run-control.invalid-transition".to_owned(),
                    message: "The Work run control command or stored state is invalid.".to_owned(),
                },
                retry: RetryDirective::Never,
                legal_actions: vec![LegalAction::CorrectRequest],
            }
        }
    }
}

fn invalid_pending_interval_limit_problem() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "application.work-run-control.invalid-pending-interval-limit".to_owned(),
            message: "The Work blocked-interval recovery page limit must be between 1 and 128."
                .to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::CorrectRequest],
    }
}

fn workflow_steps_for_live_attempts(
    live_attempts: &[AttemptId],
    workflow_attempts: Vec<WorkRunLiveAttemptV1>,
) -> Result<Vec<WorkRunLiveAttemptV1>, ApplicationProblem> {
    let mut by_attempt = std::collections::BTreeMap::new();
    for attempt in workflow_attempts {
        if by_attempt
            .insert(attempt.attempt_id.clone(), attempt)
            .is_some()
        {
            return Err(storage_problem(WorkRunControlStorageError::Unavailable));
        }
    }
    live_attempts
        .iter()
        .map(|attempt_id| {
            by_attempt
                .remove(attempt_id)
                .ok_or_else(|| storage_problem(WorkRunControlStorageError::AuthorityConflict))
        })
        .collect()
}

fn invalid_open_interval_durable_problem() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "application.work-run-control.open-interval-durable".to_owned(),
            message: "Only a settled Work blocked interval can be marked durably delivered."
                .to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::CorrectRequest],
    }
}

fn authority_conflict_problem() -> ApplicationProblem {
    conflict_problem(
        "application.work-run-control.authority-conflict",
        "The Work run control authority version changed after this command was prepared.",
    )
}

fn not_found_problem() -> ApplicationProblem {
    ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
}

fn conflict_problem(code: &str, message: &str) -> ApplicationProblem {
    ApplicationProblem::Conflict {
        diagnostic: SafeDiagnostic {
            code: code.to_owned(),
            message: message.to_owned(),
        },
        retry: RetryDirective::AfterRevalidate,
        legal_actions: vec![LegalAction::Refresh],
    }
}
