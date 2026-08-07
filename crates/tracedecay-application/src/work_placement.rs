//! Typed placement lowering: preflight, admit, status, and release.
//!
//! Plan 32 (`docs/plans/tracedecay-v2/32-dynamic-workflow-runtime-and-sdk.md`,
//! "Application operations and surfaces") lists "placement preflight/admit/
//! status/release and safe cleanup" among the retained operations, and
//! "Placement, topology, and safe Git effects" requires linked and isolated
//! placements to be "canonical, exclusive, fenced ... and retained/quarantined
//! rather than cleaned when dirty, conflicted, unknown, or uniquely valuable".
//!
//! Two things stay out of this module on purpose:
//!
//! * **Reading the filesystem.** The caller supplies the observation through a
//!   closure, exactly as [`crate::WorkAttemptService::list`] takes its verified
//!   topology. The daemon resolves it from the native Git authority (Plan 36 is
//!   the Git evidence owner); the service decides what the observation *means*.
//!   That keeps the decision testable without a repository and keeps a second
//!   Git reader out of the application layer.
//! * **Deleting anything.** [`WorkPlacementService::release`] publishes a
//!   released or quarantined placement. Removal of bytes is a separate cleanup
//!   preflight, and the plan is explicit that retention expiry "is eligibility
//!   for a fresh cleanup preflight, not delete authority".
//!
//! Exclusivity is the service's own decision rather than the observer's: only
//! storage knows whether another admitted placement holds the same root, so the
//! service overwrites the observation's `active_holder` with what it read.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    RunId, TaskId, UtcMicros, WorkAuthority, WorkPlacementBlockerV1, WorkPlacementContractError,
    WorkPlacementIdentityV1, WorkPlacementObservationV1, WorkPlacementPreflightV1,
    WorkPlacementStateV1, WorkPlacementTargetV1, WorkPlacementV1,
};

use crate::work::work_authority;
use crate::{
    ApplicationProblem, LegalAction, RequestAdmission, RequestContext, RetryDirective,
    SafeDiagnostic,
};

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum WorkPlacementStorageError {
    #[error("the Work placement authority is unavailable")]
    Unavailable,
    #[error("the Work placement row is not present or not authorized")]
    NotFoundOrNotAuthorized,
    #[error("the Work placement authority version changed")]
    AuthorityConflict,
}

/// The durable placement relations, one per run.
pub trait WorkPlacementStoragePort: Send + Sync {
    fn load_placement(
        &self,
        authority: &WorkAuthority,
        identity: &WorkPlacementIdentityV1,
    ) -> Result<Option<WorkPlacementV1>, WorkPlacementStorageError>;

    /// The placement that currently holds `root`, if any. A placement holds its
    /// root while admitted or quarantined; a released one does not.
    fn target_holder(
        &self,
        authority: &WorkAuthority,
        root: &str,
    ) -> Result<Option<WorkPlacementIdentityV1>, WorkPlacementStorageError>;

    /// Publishes `next` under a compare-and-swap on the authority version the
    /// caller read. `expected` is `None` only for the first admission.
    fn publish_placement(
        &self,
        authority: &WorkAuthority,
        expected: Option<u64>,
        next: &WorkPlacementV1,
    ) -> Result<(), WorkPlacementStorageError>;
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(title = "WorkPlacementPreflightRequestV1")]
pub struct WorkPlacementPreflightRequestV1 {
    pub task_id: TaskId,
    pub run_id: RunId,
    pub target: WorkPlacementTargetV1,
    pub occurred_at: UtcMicros,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(title = "AdmitWorkPlacementCommand")]
pub struct AdmitWorkPlacementCommand {
    pub task_id: TaskId,
    pub run_id: RunId,
    pub target: WorkPlacementTargetV1,
    /// When retention makes this placement eligible for a fresh cleanup
    /// preflight. Eligibility is not delete authority.
    #[serde(default)]
    pub retention_eligible_at: Option<UtcMicros>,
    pub occurred_at: UtcMicros,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(title = "WorkPlacementStatusRequestV1")]
pub struct WorkPlacementStatusRequestV1 {
    pub task_id: TaskId,
    pub run_id: RunId,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(title = "ReleaseWorkPlacementCommand")]
pub struct ReleaseWorkPlacementCommand {
    pub task_id: TaskId,
    pub run_id: RunId,
    pub expected_authority_version: u64,
    pub occurred_at: UtcMicros,
}

/// One placement reading. Absence is a state, not an empty placement.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(title = "WorkPlacementReadingV1")]
pub enum WorkPlacementReadingV1 {
    /// No placement was ever admitted for this run.
    Absent,
    /// The durable placement relation.
    Placed { placement: WorkPlacementV1 },
}

/// The preflight/admit/status/release authority for run placement.
pub struct WorkPlacementService<S> {
    storage: S,
}

impl<S> WorkPlacementService<S>
where
    S: WorkPlacementStoragePort,
{
    pub const fn new(storage: S) -> Self {
        Self { storage }
    }

    /// Evaluates a placement without changing anything.
    pub fn preflight(
        &self,
        context: &RequestContext,
        request: WorkPlacementPreflightRequestV1,
        observe: impl FnOnce(
            &WorkPlacementTargetV1,
        ) -> Result<WorkPlacementObservationV1, ApplicationProblem>,
    ) -> Result<WorkPlacementPreflightV1, ApplicationProblem> {
        admit(context, request.occurred_at)?;
        let authority = work_authority(context)?;
        let identity = WorkPlacementIdentityV1::new(request.task_id, request.run_id);
        self.evaluate(&authority, identity, request.target, observe)
    }

    /// Admits a placement from a fresh, unblocked preflight.
    ///
    /// The preflight is re-run here rather than trusted from a prior call: an
    /// admission that reused a caller-held preflight would admit against a
    /// target that may have changed since it was read.
    pub fn admit_placement(
        &self,
        context: &RequestContext,
        command: AdmitWorkPlacementCommand,
        observe: impl FnOnce(
            &WorkPlacementTargetV1,
        ) -> Result<WorkPlacementObservationV1, ApplicationProblem>,
    ) -> Result<WorkPlacementV1, ApplicationProblem> {
        admit(context, command.occurred_at)?;
        let authority = work_authority(context)?;
        let identity = WorkPlacementIdentityV1::new(command.task_id, command.run_id);
        let existing = self
            .storage
            .load_placement(&authority, &identity)
            .map_err(storage_problem)?;
        if let Some(existing) = existing {
            // Re-admitting the same target is an idempotent replay; a different
            // target under the same run identity is a conflict, never a move.
            return if existing.state() == WorkPlacementStateV1::Admitted
                && existing.target() == &command.target
            {
                Ok(existing)
            } else {
                Err(conflict_problem(
                    "application.work-placement.identity-conflict",
                    "The Work run already holds a different placement.",
                ))
            };
        }
        let preflight = self.evaluate(&authority, identity, command.target, observe)?;
        if !preflight.is_admissible() {
            return Err(blocked_problem(&preflight.blockers));
        }
        let placement =
            WorkPlacementV1::admit(&preflight, command.retention_eligible_at, command.occurred_at)
                .map_err(contract_problem)?;
        self.storage
            .publish_placement(&authority, None, &placement)
            .map_err(storage_problem)?;
        Ok(placement)
    }

    /// Reads the durable placement relation for one run.
    pub fn status(
        &self,
        context: &RequestContext,
        request: &WorkPlacementStatusRequestV1,
    ) -> Result<WorkPlacementReadingV1, ApplicationProblem> {
        let authority = work_authority(context)?;
        let identity =
            WorkPlacementIdentityV1::new(request.task_id.clone(), request.run_id.clone());
        Ok(
            match self
                .storage
                .load_placement(&authority, &identity)
                .map_err(storage_problem)?
            {
                Some(placement) => WorkPlacementReadingV1::Placed { placement },
                None => WorkPlacementReadingV1::Absent,
            },
        )
    }

    /// Gives the target up, or quarantines it when removal is blocked.
    ///
    /// This never deletes. It publishes what the fresh cleanup preflight found,
    /// so a caller can tell "the bytes are gone" from "the bytes were kept, and
    /// here is exactly why".
    pub fn release(
        &self,
        context: &RequestContext,
        command: ReleaseWorkPlacementCommand,
        observe: impl FnOnce(
            &WorkPlacementTargetV1,
        ) -> Result<WorkPlacementObservationV1, ApplicationProblem>,
    ) -> Result<WorkPlacementV1, ApplicationProblem> {
        admit(context, command.occurred_at)?;
        let authority = work_authority(context)?;
        let identity = WorkPlacementIdentityV1::new(command.task_id, command.run_id);
        let current = self
            .storage
            .load_placement(&authority, &identity)
            .map_err(storage_problem)?
            .ok_or_else(not_found_problem)?;
        if current.authority_version() != command.expected_authority_version {
            return Err(authority_conflict_problem());
        }
        let observation = observe(current.target())?;
        let next = current
            .release(
                observation.removal_blockers(current.target()),
                command.occurred_at,
            )
            .map_err(contract_problem)?;
        self.storage
            .publish_placement(&authority, Some(current.authority_version()), &next)
            .map_err(storage_problem)?;
        Ok(next)
    }

    /// Observes the target and folds in the exclusivity fact only storage knows.
    fn evaluate(
        &self,
        authority: &WorkAuthority,
        identity: WorkPlacementIdentityV1,
        target: WorkPlacementTargetV1,
        observe: impl FnOnce(
            &WorkPlacementTargetV1,
        ) -> Result<WorkPlacementObservationV1, ApplicationProblem>,
    ) -> Result<WorkPlacementPreflightV1, ApplicationProblem> {
        let mut observation = observe(&target)?;
        observation.active_holder = self.has_foreign_holder(authority, &identity, &target)?;
        Ok(WorkPlacementPreflightV1::evaluate(
            identity,
            target,
            observation,
        ))
    }

    /// Whether a *different* run already holds this exclusive target.
    ///
    /// The run's own placement is not its own blocker, which is what lets a
    /// repeat preflight of an already-admitted placement stay admissible.
    fn has_foreign_holder(
        &self,
        authority: &WorkAuthority,
        identity: &WorkPlacementIdentityV1,
        target: &WorkPlacementTargetV1,
    ) -> Result<bool, ApplicationProblem> {
        if !target.kind().is_exclusive() {
            return Ok(false);
        }
        let Some(root) = target.root() else {
            return Ok(false);
        };
        Ok(self
            .storage
            .target_holder(authority, root)
            .map_err(storage_problem)?
            .is_some_and(|holder| &holder != identity))
    }
}

fn admit(context: &RequestContext, observed_at: UtcMicros) -> Result<(), ApplicationProblem> {
    match context.admission_at(observed_at) {
        RequestAdmission::Admitted => Ok(()),
        RequestAdmission::Cancelled => Err(ApplicationProblem::cancelled_before_admission()),
        RequestAdmission::TimedOut => Err(ApplicationProblem::timed_out_before_admission()),
    }
}

fn storage_problem(error: WorkPlacementStorageError) -> ApplicationProblem {
    match error {
        WorkPlacementStorageError::NotFoundOrNotAuthorized => not_found_problem(),
        WorkPlacementStorageError::AuthorityConflict => authority_conflict_problem(),
        WorkPlacementStorageError::Unavailable => {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "application.work-placement.storage-unavailable".to_owned(),
                message: "The Work placement authority is unavailable.".to_owned(),
            })
        }
    }
}

fn contract_problem(error: WorkPlacementContractError) -> ApplicationProblem {
    match error {
        WorkPlacementContractError::AlreadyReleased => conflict_problem(
            "application.work-placement.already-released",
            "The Work placement was already released.",
        ),
        WorkPlacementContractError::NonMonotonicTransition => conflict_problem(
            "application.work-placement.non-monotonic",
            "The Work placement transition is older than the published state.",
        ),
        _ => ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic {
                code: "application.work-placement.invalid-placement".to_owned(),
                message: "The Work placement command or stored state is invalid.".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::CorrectRequest],
        },
    }
}

/// Refuses an admission and names the exact blockers, in stable order.
///
/// The blocker names are a closed vocabulary and carry no path, so this message
/// tells a caller what to fix without disclosing anything about the target it
/// was not already authorized to see.
fn blocked_problem(
    blockers: &std::collections::BTreeSet<WorkPlacementBlockerV1>,
) -> ApplicationProblem {
    let named = blockers
        .iter()
        .map(|blocker| {
            serde_json::to_value(blocker)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "unknown".to_owned())
        })
        .collect::<Vec<_>>()
        .join(", ");
    ApplicationProblem::Conflict {
        diagnostic: SafeDiagnostic {
            code: "application.work-placement.blocked".to_owned(),
            message: format!("The Work placement is blocked by: {named}."),
        },
        retry: RetryDirective::AfterRevalidate,
        legal_actions: vec![LegalAction::Refresh],
    }
}

fn authority_conflict_problem() -> ApplicationProblem {
    conflict_problem(
        "application.work-placement.authority-conflict",
        "The Work placement authority version changed after this command was prepared.",
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
