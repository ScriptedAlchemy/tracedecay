//! Scope-bound Work application authority with optimistic concurrency.

use std::collections::{BTreeSet, VecDeque};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    ManifestDigest, ProposalId, RuntimeEvidenceRef, TaskId, UtcMicros, WorkAuthority,
    WorkCommandId, WorkContractError, WorkEvent, WorkEventKind, WorkProjection, WorkVersion,
    canonical_sha256,
};

use crate::{
    ApplicationProblem, LegalAction, RequestAdmission, RequestContext, RetryDirective,
    SafeDiagnostic,
};

const WORK_INPUT_DIGEST_DOMAIN: &str = "tracedecay.application.work-command.v1";

/// Storage refusal returned without revealing whether a differently scoped
/// history exists.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkStorageError {
    #[error("work was not found or is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("work version changed")]
    VersionConflict,
    #[error("work command identity was reused with different input")]
    IdempotencyConflict,
    #[error("work storage is unavailable")]
    Unavailable,
}

/// One compare-and-append request. `None` is valid only for creation.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAppendRequest {
    pub expected_version: Option<WorkVersion>,
    pub event: WorkEvent,
}

/// Storage returns the authoritative projection it validated and published,
/// for both a new append and an idempotent replay. The projection is the one
/// storage committed, so no caller re-folds history to learn the result.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "outcome", content = "projection")]
pub enum WorkAppendOutcome {
    Appended(WorkProjection),
    Replayed(WorkProjection),
}

impl WorkAppendOutcome {
    pub fn into_projection(self) -> WorkProjection {
        match self {
            Self::Appended(projection) | Self::Replayed(projection) => projection,
        }
    }
}

/// Exact-authority storage boundary. Implementations must compare both the
/// expected version and `(command_id, input_digest)` atomically.
pub trait WorkStoragePort: Send + Sync {
    fn load(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
    ) -> Result<Vec<WorkEvent>, WorkStorageError>;

    fn append(&self, request: &WorkAppendRequest) -> Result<WorkAppendOutcome, WorkStorageError>;
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CreateWorkCommand {
    pub task_id: TaskId,
    pub title: String,
    #[serde(default)]
    pub dependencies: BTreeSet<TaskId>,
    pub command_id: WorkCommandId,
    pub occurred_at: UtcMicros,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReplanDependenciesCommand {
    pub task_id: TaskId,
    #[serde(default)]
    pub dependencies: BTreeSet<TaskId>,
    pub expected_version: WorkVersion,
    pub command_id: WorkCommandId,
    pub occurred_at: UtcMicros,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewProposalCommand {
    pub task_id: TaskId,
    pub proposal_id: ProposalId,
    pub proposal_digest: ManifestDigest,
    pub expected_version: WorkVersion,
    pub command_id: WorkCommandId,
    pub occurred_at: UtcMicros,
}

/// A proposal review records a non-accepting disposition. Acceptance remains a
/// separate command so callers cannot accidentally collapse review into
/// approval.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewProposalDispositionV1 {
    Rejected,
    Superseded,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewProposalRequestV1 {
    pub review: ReviewProposalCommand,
    pub disposition: ReviewProposalDispositionV1,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AcceptProposalCommand {
    pub review: ReviewProposalCommand,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AdmitExecutionCommand {
    pub task_id: TaskId,
    pub expected_version: WorkVersion,
    pub command_id: WorkCommandId,
    pub occurred_at: UtcMicros,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AttachRuntimeEvidenceCommand {
    pub task_id: TaskId,
    pub evidence: RuntimeEvidenceRef,
    pub expected_version: WorkVersion,
    pub command_id: WorkCommandId,
    pub occurred_at: UtcMicros,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AcceptTaskCommand {
    pub task_id: TaskId,
    pub expected_version: WorkVersion,
    pub command_id: WorkCommandId,
    pub occurred_at: UtcMicros,
}

/// Read-only proposal generation is pinned to the current Work version.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerateProposalRequest {
    pub task_id: TaskId,
    pub proposal_id: ProposalId,
    pub proposal_digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GeneratedWorkProposal {
    pub task_id: TaskId,
    pub proposal_id: ProposalId,
    pub proposal_digest: ManifestDigest,
    pub based_on_version: WorkVersion,
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum WorkReadiness {
    Ready,
    Blocked {
        active_dependencies: BTreeSet<TaskId>,
    },
    Accepted,
}

pub struct WorkService<P> {
    storage: P,
}

impl<P> WorkService<P>
where
    P: WorkStoragePort,
{
    pub const fn new(storage: P) -> Self {
        Self { storage }
    }

    pub fn load(
        &self,
        context: &RequestContext,
        task_id: &TaskId,
    ) -> Result<WorkProjection, ApplicationProblem> {
        let authority = work_authority(context)?;
        rebuild(self.load_history(&authority, task_id)?)
    }

    pub fn create(
        &self,
        context: &RequestContext,
        command: CreateWorkCommand,
    ) -> Result<WorkProjection, ApplicationProblem> {
        admit(context, command.occurred_at)?;
        let authority = work_authority(context)?;
        let input_digest = work_input_digest(&(
            WORK_INPUT_DIGEST_DOMAIN,
            "create",
            &command.task_id,
            &command.title,
            &command.dependencies,
            command.occurred_at,
        ))?;
        let event = WorkEvent::new(
            command.task_id,
            WorkVersion::initial(),
            authority,
            command.occurred_at,
            command.command_id,
            input_digest,
            WorkEventKind::Created {
                title: command.title,
                dependencies: command.dependencies,
            },
        )
        .map_err(domain_problem)?;
        self.append(WorkAppendRequest {
            expected_version: None,
            event,
        })
    }

    pub fn replan_dependencies(
        &self,
        context: &RequestContext,
        command: ReplanDependenciesCommand,
    ) -> Result<WorkProjection, ApplicationProblem> {
        admit(context, command.occurred_at)?;
        let authority = work_authority(context)?;
        let input_digest = work_input_digest(&(
            WORK_INPUT_DIGEST_DOMAIN,
            "replan_dependencies",
            &command.task_id,
            &command.dependencies,
            command.expected_version,
            command.occurred_at,
        ))?;
        self.append_mutation(
            authority,
            command.task_id,
            command.expected_version,
            command.command_id,
            input_digest,
            command.occurred_at,
            WorkEventKind::DependenciesReplanned {
                dependencies: command.dependencies,
            },
        )
    }

    pub fn generate_proposal(
        &self,
        context: &RequestContext,
        request: GenerateProposalRequest,
    ) -> Result<GeneratedWorkProposal, ApplicationProblem> {
        let projection = self.load(context, &request.task_id)?;
        Ok(GeneratedWorkProposal {
            task_id: request.task_id,
            proposal_id: request.proposal_id,
            proposal_digest: request.proposal_digest,
            based_on_version: projection.version(),
        })
    }

    pub fn accept_proposal(
        &self,
        context: &RequestContext,
        command: AcceptProposalCommand,
    ) -> Result<WorkProjection, ApplicationProblem> {
        self.apply_proposal_disposition(context, command.review, ProposalDisposition::Accepted)
    }

    pub fn review_proposal(
        &self,
        context: &RequestContext,
        request: ReviewProposalRequestV1,
    ) -> Result<WorkProjection, ApplicationProblem> {
        let disposition = match request.disposition {
            ReviewProposalDispositionV1::Rejected => ProposalDisposition::Rejected,
            ReviewProposalDispositionV1::Superseded => ProposalDisposition::Superseded,
        };
        self.apply_proposal_disposition(context, request.review, disposition)
    }

    pub fn reject_proposal(
        &self,
        context: &RequestContext,
        command: ReviewProposalCommand,
    ) -> Result<WorkProjection, ApplicationProblem> {
        self.apply_proposal_disposition(context, command, ProposalDisposition::Rejected)
    }

    pub fn supersede_proposal(
        &self,
        context: &RequestContext,
        command: ReviewProposalCommand,
    ) -> Result<WorkProjection, ApplicationProblem> {
        self.apply_proposal_disposition(context, command, ProposalDisposition::Superseded)
    }

    pub fn admit_execution(
        &self,
        context: &RequestContext,
        command: AdmitExecutionCommand,
    ) -> Result<WorkProjection, ApplicationProblem> {
        admit(context, command.occurred_at)?;
        let authority = work_authority(context)?;
        let input_digest = work_input_digest(&(
            WORK_INPUT_DIGEST_DOMAIN,
            "admit_execution",
            &command.task_id,
            command.expected_version,
            command.occurred_at,
        ))?;
        self.append_mutation(
            authority,
            command.task_id,
            command.expected_version,
            command.command_id,
            input_digest,
            command.occurred_at,
            WorkEventKind::ExecutionAdmitted,
        )
    }

    pub fn attach_runtime_evidence(
        &self,
        context: &RequestContext,
        command: AttachRuntimeEvidenceCommand,
    ) -> Result<WorkProjection, ApplicationProblem> {
        admit(context, command.occurred_at)?;
        let authority = work_authority(context)?;
        let input_digest = work_input_digest(&(
            WORK_INPUT_DIGEST_DOMAIN,
            "attach_runtime_evidence",
            &command.task_id,
            &command.evidence,
            command.expected_version,
            command.occurred_at,
        ))?;
        self.append_mutation(
            authority,
            command.task_id,
            command.expected_version,
            command.command_id,
            input_digest,
            command.occurred_at,
            WorkEventKind::RuntimeEvidenceAttached {
                evidence: command.evidence,
            },
        )
    }

    pub fn accept_task(
        &self,
        context: &RequestContext,
        command: AcceptTaskCommand,
    ) -> Result<WorkProjection, ApplicationProblem> {
        admit(context, command.occurred_at)?;
        let authority = work_authority(context)?;
        let input_digest = work_input_digest(&(
            WORK_INPUT_DIGEST_DOMAIN,
            "accept_task",
            &command.task_id,
            command.expected_version,
            command.occurred_at,
        ))?;
        self.append_mutation(
            authority,
            command.task_id,
            command.expected_version,
            command.command_id,
            input_digest,
            command.occurred_at,
            WorkEventKind::TaskAccepted,
        )
    }

    pub fn readiness(
        &self,
        context: &RequestContext,
        task_id: &TaskId,
    ) -> Result<WorkReadiness, ApplicationProblem> {
        let authority = work_authority(context)?;
        let projection = rebuild(self.load_history(&authority, task_id)?)?;
        if projection.is_task_accepted() {
            return Ok(WorkReadiness::Accepted);
        }

        let mut active_dependencies = BTreeSet::new();
        for dependency in projection.dependencies() {
            match self.storage.load(&authority, dependency) {
                Ok(history) => {
                    if !rebuild(history)?.is_task_accepted() {
                        active_dependencies.insert(dependency.clone());
                    }
                }
                Err(WorkStorageError::NotFoundOrNotAuthorized) => {
                    active_dependencies.insert(dependency.clone());
                }
                Err(error) => return Err(storage_problem(error)),
            }
        }
        if active_dependencies.is_empty() {
            Ok(WorkReadiness::Ready)
        } else {
            Ok(WorkReadiness::Blocked {
                active_dependencies,
            })
        }
    }

    fn apply_proposal_disposition(
        &self,
        context: &RequestContext,
        command: ReviewProposalCommand,
        disposition: ProposalDisposition,
    ) -> Result<WorkProjection, ApplicationProblem> {
        admit(context, command.occurred_at)?;
        let authority = work_authority(context)?;
        let operation = disposition.operation();
        let input_digest = work_input_digest(&(
            WORK_INPUT_DIGEST_DOMAIN,
            operation,
            &command.task_id,
            &command.proposal_id,
            &command.proposal_digest,
            command.expected_version,
            command.occurred_at,
        ))?;
        let event = match disposition {
            ProposalDisposition::Accepted => WorkEventKind::ProposalAccepted {
                proposal_id: command.proposal_id,
                proposal_digest: command.proposal_digest,
            },
            ProposalDisposition::Rejected => WorkEventKind::ProposalRejected {
                proposal_id: command.proposal_id,
                proposal_digest: command.proposal_digest,
            },
            ProposalDisposition::Superseded => WorkEventKind::ProposalSuperseded {
                proposal_id: command.proposal_id,
                proposal_digest: command.proposal_digest,
            },
        };
        self.append_mutation(
            authority,
            command.task_id,
            command.expected_version,
            command.command_id,
            input_digest,
            command.occurred_at,
            event,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn append_mutation(
        &self,
        authority: WorkAuthority,
        task_id: TaskId,
        expected_version: WorkVersion,
        command_id: WorkCommandId,
        input_digest: ManifestDigest,
        occurred_at: UtcMicros,
        event_kind: WorkEventKind,
    ) -> Result<WorkProjection, ApplicationProblem> {
        if let WorkEventKind::DependenciesReplanned { dependencies } = &event_kind
            && self.would_create_dependency_cycle(&authority, &task_id, dependencies)?
        {
            return Err(invalid_problem(
                "application.work.dependency-cycle",
                "Work dependencies must remain acyclic.",
            ));
        }
        let event = WorkEvent::new(
            task_id,
            expected_version.next().map_err(domain_problem)?,
            authority,
            occurred_at,
            command_id,
            input_digest,
            event_kind,
        )
        .map_err(domain_problem)?;
        self.append(WorkAppendRequest {
            expected_version: Some(expected_version),
            event,
        })
    }

    fn append(&self, request: WorkAppendRequest) -> Result<WorkProjection, ApplicationProblem> {
        self.storage
            .append(&request)
            .map(WorkAppendOutcome::into_projection)
            .map_err(storage_problem)
    }

    fn load_history(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
    ) -> Result<Vec<WorkEvent>, ApplicationProblem> {
        self.storage
            .load(authority, task_id)
            .map_err(storage_problem)
    }

    fn would_create_dependency_cycle(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        dependencies: &BTreeSet<TaskId>,
    ) -> Result<bool, ApplicationProblem> {
        let mut pending: VecDeque<TaskId> = dependencies.iter().cloned().collect();
        let mut visited = BTreeSet::new();
        while let Some(candidate) = pending.pop_front() {
            if &candidate == task_id {
                return Ok(true);
            }
            if !visited.insert(candidate.clone()) {
                continue;
            }
            match self.storage.load(authority, &candidate) {
                Ok(history) => {
                    pending.extend(rebuild(history)?.dependencies().iter().cloned());
                }
                Err(WorkStorageError::NotFoundOrNotAuthorized) => {}
                Err(error) => return Err(storage_problem(error)),
            }
        }
        Ok(false)
    }
}

#[derive(Clone, Copy)]
enum ProposalDisposition {
    Accepted,
    Rejected,
    Superseded,
}

impl ProposalDisposition {
    const fn operation(self) -> &'static str {
        match self {
            Self::Accepted => "accept_proposal",
            Self::Rejected => "reject_proposal",
            Self::Superseded => "supersede_proposal",
        }
    }
}

pub(crate) fn work_authority(
    context: &RequestContext,
) -> Result<WorkAuthority, ApplicationProblem> {
    WorkAuthority::new(
        context.scope().project_id.clone(),
        context.scope().repository_id.clone(),
        context.scope().worktree_id.clone(),
        context.actor().clone(),
        context.grant().digest.clone(),
    )
    .map_err(domain_problem)
}

fn work_input_digest<T: Serialize>(value: &T) -> Result<ManifestDigest, ApplicationProblem> {
    canonical_sha256(value).map_err(|_| {
        invalid_problem(
            "application.work.invalid-command",
            "The Work command could not be canonicalized.",
        )
    })
}

fn admit(context: &RequestContext, observed_at: UtcMicros) -> Result<(), ApplicationProblem> {
    match context.admission_at(observed_at) {
        RequestAdmission::Admitted => Ok(()),
        RequestAdmission::Cancelled => Err(ApplicationProblem::cancelled_before_admission()),
        RequestAdmission::TimedOut => Err(ApplicationProblem::timed_out_before_admission()),
    }
}

fn rebuild(history: Vec<WorkEvent>) -> Result<WorkProjection, ApplicationProblem> {
    WorkProjection::rebuild(&history).map_err(domain_problem)
}

fn domain_problem(_error: WorkContractError) -> ApplicationProblem {
    invalid_problem(
        "application.work.invalid-history",
        "The Work command or stored history is invalid.",
    )
}

fn storage_problem(error: WorkStorageError) -> ApplicationProblem {
    match error {
        WorkStorageError::NotFoundOrNotAuthorized => not_found_problem(),
        WorkStorageError::VersionConflict => conflict_problem(
            "application.work.version-conflict",
            "Work changed after this command was prepared.",
        ),
        WorkStorageError::IdempotencyConflict => conflict_problem(
            "application.work.idempotency-conflict",
            "The Work command identity was already used with different input.",
        ),
        WorkStorageError::Unavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "application.work.storage-unavailable".to_owned(),
            message: "The Work authority is unavailable.".to_owned(),
        }),
    }
}

fn not_found_problem() -> ApplicationProblem {
    ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
}

fn invalid_problem(code: &str, message: &str) -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: code.to_owned(),
            message: message.to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::CorrectRequest],
    }
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
