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
    AttemptId, RunId, TaskId, UtcMicros, WorkAuthority, WorkRunControlAuthorityV1,
    WorkRunControlContractError, WorkRunControlReasonV1, WorkRunControlV1,
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

/// The durable run-control rows and the attempt evidence they are derived
/// from.
pub trait WorkRunControlStoragePort: Send + Sync {
    /// The admitted deadline and live attempt frontier for one run, or `None`
    /// when the run holds no durable attempt at all.
    fn run_admission(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<Option<WorkRunAdmissionV1>, WorkRunControlStorageError>;

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
        admit(context, command.occurred_at)?;
        let authority = work_authority(context)?;
        let admission = self.require_admission(&authority, &command.task_id, &command.run_id)?;
        let existing = self
            .storage
            .load_run_control(&authority, &command.task_id, &command.run_id)
            .map_err(storage_problem)?;
        check_expected(
            existing.as_ref(),
            expected_authority(command.expected_authority_version)?,
        )?;

        // The compare-and-swap expectation is what storage currently holds,
        // which `check_expected` has just proved is what the caller read.
        let published = existing.as_ref().map(WorkRunControlV1::authority);
        let current = match existing {
            Some(control) => control,
            None => WorkRunControlV1::admitted(
                command.task_id.clone(),
                command.run_id.clone(),
                admission.deadline,
                command.occurred_at,
            )
            .map_err(contract_problem)?,
        };
        // A run that was never controlled publishes the paused aggregate
        // directly; writing an intermediate `Running` row first would claim a
        // transition that never happened.
        let next = current
            .pause(command.reason, command.occurred_at, admission.live_attempts)
            .map_err(contract_problem)?;
        self.storage
            .publish_run_control(&authority, published, &next)
            .map_err(storage_problem)?;
        Ok(next)
    }

    /// Readmits reservations for one paused run.
    pub fn resume(
        &self,
        context: &RequestContext,
        command: ResumeWorkRunCommand,
    ) -> Result<WorkRunControlV1, ApplicationProblem> {
        admit(context, command.occurred_at)?;
        let authority = work_authority(context)?;
        self.require_admission(&authority, &command.task_id, &command.run_id)?;
        let current = self
            .storage
            .load_run_control(&authority, &command.task_id, &command.run_id)
            .map_err(storage_problem)?
            .ok_or_else(|| {
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
        self.storage
            .publish_run_control(&authority, Some(expected), &next)
            .map_err(storage_problem)?;
        Ok(next)
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
        | WorkRunControlContractError::DuplicateFencedAttempt => {
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
