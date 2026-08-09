//! Durable admitted-provider attempt authority over the canonical Work
//! runtime contracts.
//!
//! This module owns lease acquisition, fenced state transitions, cancellation
//! progression, resume-after-restart fencing, and terminal-evidence
//! projection back into Work. It owns no process handling: the daemon's
//! provider runtime drives these transitions and is the only component that
//! touches an executable.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    AttemptId, CommitId, ManifestDigest, ObservationSourceIdentityV1, ProjectId, RefId,
    RepositoryId, RunId, RuntimeEvidenceRef, TaskId, UtcMicros, WorkAttemptIdentityV1,
    WorkAttemptStateV1, WorkAttemptV1, WorkAuthority, WorkCancellationAcknowledgementV1,
    WorkCancellationEscalationV1, WorkCancellationRequestId, WorkCancellationRequestV1,
    WorkCancellationStateV1, WorkCommandId, WorkEffectStateV1, WorkExecutionSnapshot,
    WorkFenceEpochV1, WorkLeaseFenceV1, WorkLeaseId, WorkProviderBackendV1, WorkProviderRouteV1,
    WorkRecoveryStateV1, WorkRestartReasonV1, WorkRuntimeContractError, WorkTerminalEvidenceV1,
    WorkTopologyPolicyV1, WorkflowOperationRef, WorktreeId, canonical_sha256,
};

use crate::work::{AttachRuntimeEvidenceCommand, WorkService, WorkStoragePort, work_authority};
use crate::work_read::WorkProjectionReadPort;
use crate::{ApplicationProblem, RequestAdmission, RequestContext};
use crate::{VerifiedWorkGraphVersionV1, WorkProductSelectionScopeV1};

mod capacity;
mod problem;
mod product_admission;
mod synthesis_admission;
pub use capacity::{
    MAX_WORK_ATTEMPT_CAPACITY_TASKS, WorkAttemptCapacityScopeV1, WorkAttemptCapacityV1,
    WorkAttemptCapacityVerdictV1,
};
use problem::{
    conflict_problem, contract_problem, denied_problem, invalid_problem,
    list_page_contract_problem, not_found_problem, projection_problem, stale_cursor_problem,
    storage_problem,
};
pub use product_admission::WorkProductAttemptServiceV1;
pub(crate) use product_admission::{
    CurrentWorkProductAttemptGraphV1, accepted_attempt_draft, current_work_product_attempt_graph,
    product_admission_problem, product_attempt_projection_binding,
};
pub use synthesis_admission::{
    WorkAttemptAdmissionKind, WorkSynthesisAdmissionStoragePort, WorkSynthesisInsertOutcome,
};

const WORK_ATTEMPT_EVIDENCE_DOMAIN: &str = "tracedecay.application.work-attempt-evidence.v1";

/// Storage refusal for the durable attempt rows. Refusals never disclose
/// whether a differently scoped attempt exists.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkAttemptStorageError {
    #[error("work attempt was not found or is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("work attempt identity was reused with different content")]
    AttemptConflict,
    #[error("work attempt conflicts with the run's first admitted deadline or topology")]
    RunAdmissionConflict,
    #[error("work attempt reservation is fenced by run control")]
    ReservationFenced,
    #[error("work attempt lease fence changed")]
    FenceConflict,
    #[error("work attempt concurrency capacity is exhausted")]
    CapacityExceeded,
    #[error("work attempt storage is unavailable")]
    Unavailable,
}

/// Outcome of an idempotent attempt insertion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkAttemptInsertOutcome {
    Inserted,
    Replayed(Box<WorkAttemptV1>),
}

/// Durable attempt persistence. Every transition is a compare-and-swap on the
/// exact prior lease fence and state, so a fenced-out writer cannot advance a
/// row it no longer owns.
pub trait WorkAttemptStoragePort: Send + Sync {
    /// Mints the next monotonic fence epoch for this authority scope.
    fn next_fence_epoch(&self, authority: &WorkAuthority) -> Result<u64, WorkAttemptStorageError>;

    /// Inserts a new attempt, or replays the stored attempt when the exact
    /// same identity and content were already inserted.
    fn insert(
        &self,
        authority: &WorkAuthority,
        attempt: &WorkAttemptV1,
    ) -> Result<WorkAttemptInsertOutcome, WorkAttemptStorageError>;

    /// Inserts a lease only while the registered topology still has room.
    /// The capacity check and insertion are one storage transaction so two
    /// concurrent admissions cannot overbook the project, repository, or task.
    fn insert_bounded(
        &self,
        authority: &WorkAuthority,
        attempt: &WorkAttemptV1,
        concurrency: &tracedecay_domain::configuration::TopologyConcurrencyPolicyV1,
    ) -> Result<WorkAttemptInsertOutcome, WorkAttemptStorageError>;

    /// Reads the same exact project/repository/task capacity counts used by
    /// bounded insertion without reserving or mutating capacity.
    fn admission_capacities(
        &self,
        authority: &WorkAuthority,
        task_ids: &[TaskId],
        concurrency: &tracedecay_domain::configuration::TopologyConcurrencyPolicyV1,
    ) -> Result<std::collections::BTreeMap<TaskId, WorkAttemptCapacityV1>, WorkAttemptStorageError>;

    fn load(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkAttemptV1, WorkAttemptStorageError>;

    /// Identifies which admission authority owns an existing attempt row.
    fn load_admission_kind(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkAttemptAdmissionKind, WorkAttemptStorageError>;

    /// Replaces the attempt row only when the stored lease fence and state
    /// still match the expected pair.
    fn update(
        &self,
        authority: &WorkAuthority,
        expected_fence: &WorkLeaseFenceV1,
        expected_state: WorkAttemptStateV1,
        next: &WorkAttemptV1,
        evidence: Option<&WorkAttemptEvidenceRecordV1>,
    ) -> Result<(), WorkAttemptStorageError>;

    /// Every non-terminal attempt in this authority scope, in identity order.
    fn open_attempts(
        &self,
        authority: &WorkAuthority,
    ) -> Result<Vec<WorkAttemptV1>, WorkAttemptStorageError>;

    /// Whether any non-terminal attempt holds this exact registered Work
    /// scope, independent of the actor and policy lineage that admitted it.
    /// Cleanup is an infrastructure safety read and must see old-policy and
    /// delegated-actor rows without granting ordinary cross-authority access.
    fn has_open_attempts_in_exact_scope(
        &self,
        _project_id: &ProjectId,
        _repository_id: &RepositoryId,
        _worktree_id: &WorktreeId,
    ) -> Result<bool, WorkAttemptStorageError> {
        Err(WorkAttemptStorageError::Unavailable)
    }

    /// One page of attempts in this authority scope, in stable
    /// task/run/attempt identity order, strictly after `start_after`.
    ///
    /// The page and its remaining count are read under one consistent view,
    /// so `remaining` always covers exactly the rows the cursor has not yet
    /// returned (this page included).
    fn list(
        &self,
        authority: &WorkAuthority,
        start_after: Option<&WorkAttemptIdentityV1>,
        limit: u32,
    ) -> Result<WorkAttemptListPageV1, WorkAttemptStorageError>;
}

/// One stable-ordered storage page of attempt rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkAttemptListPageV1 {
    pub attempts: Vec<WorkAttemptV1>,
    /// Attempts in scope strictly after the page start, including this page.
    pub remaining: u32,
}

/// Typed provider availability observed at negotiation. These are product
/// states, not transport errors: each one names why the configured native
/// provider could not run, without inventing a fallback.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkProviderAvailabilityV1 {
    /// No executable binding is configured for the pinned executable identity.
    Absent,
    /// The configured binding no longer matches the pinned reference.
    Stale,
    /// The configured binding does not admit the pinned backend/protocol.
    Unsupported,
    /// The on-disk executable bytes do not match the pinned digest.
    DigestMismatch,
    /// The configured executable could not be read or is not executable.
    Unavailable,
}

/// How one provider attempt ended, as observed by the daemon runtime.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WorkAttemptProviderOutcomeV1 {
    Exited {
        code: i32,
    },
    Signalled {
        signal: i32,
    },
    TimedOut,
    Cancelled,
    ProviderUnavailable {
        state: WorkProviderAvailabilityV1,
    },
    StreamOverflow {
        channel: WorkAttemptStreamChannelV1,
    },
    LaunchFailed,
    /// The provider started but its typed protocol session did not reach a
    /// terminal answer: a malformed, out-of-order, oversized, or lost frame.
    /// Plan 32 requires such a stream to seal failed evidence rather than a
    /// text-scraped success.
    ProtocolFailed,
}

/// Why an admitted attempt did not run on the backend its pinned execution
/// snapshot preferred.
///
/// Plan 32 (`docs/plans/tracedecay-v2/32-dynamic-workflow-runtime-and-sdk.md`,
/// "Native provider execution") admits the Codex CLI "only when app-server is
/// unsupported or absent before session start and the pinned Plan 20 snapshot
/// explicitly allows that fallback", and the plan index requires that fallback
/// to be "reported rather than hidden". This record is that report: it names
/// the preferred backend, the typed state that disqualified it, and the
/// configuration-bounded fallback that was selected, so a fallback run can
/// never be read as a first choice.
///
/// It is also written when the fallback itself was refused, so a denial keeps
/// both failures instead of collapsing them into one state.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProviderFallbackRecordV1 {
    /// The backend the pinned snapshot named first.
    pub preferred_backend: WorkProviderBackendV1,
    /// The route that backend would have run on.
    pub preferred_route: WorkProviderRouteV1,
    /// Why the preferred backend could not be used.
    pub preferred_state: WorkProviderAvailabilityV1,
    /// The backend named by the snapshot's configured fallback topology.
    pub fallback_backend: WorkProviderBackendV1,
    /// The route that fallback runs on.
    pub fallback_route: WorkProviderRouteV1,
    /// `None` when the fallback actually started; `Some(state)` when the
    /// fallback was itself refused.
    pub fallback_state: Option<WorkProviderAvailabilityV1>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkAttemptStreamChannelV1 {
    Stdout,
    Stderr,
}

/// Bounded summary of one captured provider stream.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptStreamSummaryV1 {
    pub byte_length: u64,
    pub truncated: bool,
    pub digest: ManifestDigest,
}

/// Sealed terminal evidence for one provider attempt. The digest of this
/// record is the evidence digest carried by [`WorkTerminalEvidenceV1`] and by
/// the `RuntimeEvidenceRef` attached to the Work projection, so the receipt
/// in Work always names an inspectable record.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptEvidenceRecordV1 {
    pub identity: WorkAttemptIdentityV1,
    pub requested_route: WorkProviderRouteV1,
    pub actual_route: Option<WorkProviderRouteV1>,
    pub outcome: WorkAttemptProviderOutcomeV1,
    pub stdout: Option<WorkAttemptStreamSummaryV1>,
    pub stderr: Option<WorkAttemptStreamSummaryV1>,
    /// Native provider-qualified session/thread identity, when the provider
    /// reported one. Admission cannot supply this value and no fallback is
    /// fabricated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session: Option<ObservationSourceIdentityV1>,
    /// Present only when the pinned preferred backend was disqualified before
    /// startup. `None` means the attempt ran on its first-choice backend.
    pub provider_fallback: Option<WorkProviderFallbackRecordV1>,
    pub observed_at: UtcMicros,
}

impl WorkAttemptEvidenceRecordV1 {
    pub fn digest(&self) -> Result<ManifestDigest, ApplicationProblem> {
        canonical_sha256(&(WORK_ATTEMPT_EVIDENCE_DOMAIN, self)).map_err(|_| {
            invalid_problem(
                "application.work-attempt.invalid-evidence",
                "The Work attempt evidence record could not be canonicalized.",
            )
        })
    }
}

/// Starts one admitted provider attempt. Every field is a typed fact; there
/// is no argv, environment entry, executable path, or shell string here. The
/// projection binding and admission facts are re-read from the canonical Work
/// authority, never trusted from the caller.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StartWorkAttemptCommand {
    pub selection: WorkProductSelectionScopeV1,
    pub verified_graph_version: VerifiedWorkGraphVersionV1,
    pub task_id: TaskId,
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub operation: WorkflowOperationRef,
    pub execution_snapshot: WorkExecutionSnapshot,
    pub worktree_root: String,
    pub reference: Option<RefId>,
    pub commit: CommitId,
    pub instructions: String,
    pub effect_state: WorkEffectStateV1,
    pub occurred_at: UtcMicros,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptStatusRequestV1 {
    pub task_id: TaskId,
    pub run_id: RunId,
    pub attempt_id: AttemptId,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CancelWorkAttemptCommand {
    pub task_id: TaskId,
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub request_id: WorkCancellationRequestId,
    pub occurred_at: UtcMicros,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResumeWorkAttemptsCommand {
    pub occurred_at: UtcMicros,
}

/// What resume-after-restart did to each open attempt.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptRecoveryReportV1 {
    /// Attempts fenced onto a new epoch and now awaiting recovery execution.
    pub recovery_required: Vec<WorkAttemptV1>,
    /// Attempts whose in-flight cancellation was completed during recovery.
    pub cancelled: Vec<WorkAttemptV1>,
}

pub const MAX_WORK_ATTEMPT_LIST_PAGE_SIZE: u32 = 1_000;

/// The verified Work topology snapshot one attempt-list page was read under.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptTopologyBindingV1 {
    /// The verified graph generation the topology snapshot is published under.
    pub generation: String,
    /// The number of tasks in the verified topology.
    pub task_count: u32,
}

/// Typed availability of the verified Work topology for a list read. The
/// caller resolves this through the project graph publication mount; the
/// service refuses to page attempts against anything else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkAttemptTopologyStateV1 {
    /// No Work has ever been recorded in this authority scope.
    Absent,
    /// The topology snapshot was published and verified.
    Verified(WorkAttemptTopologyBindingV1),
}

/// Resume point for the next attempt-list page, bound to the exact verified
/// topology generation it was minted under.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptListCursorV1 {
    /// The verified topology generation the cursor was minted under.
    pub generation: String,
    /// The last attempt identity the prior page returned.
    pub start_after: WorkAttemptIdentityV1,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptListRequestV1 {
    pub page_size: u32,
    #[serde(default)]
    pub cursor: Option<WorkAttemptListCursorV1>,
}

/// How much of the authorized attempt set one page covers.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "coverage", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkAttemptListCoverageV1 {
    /// Every attempt after the page start was returned; zero returned is the
    /// explicit empty authorized result, not a concealment.
    Complete { returned: u32 },
    /// The page cap was reached; `resume` continues under the same verified
    /// topology generation.
    Capped {
        returned: u32,
        remaining: u32,
        resume: WorkAttemptListCursorV1,
    },
}

/// One authority-scoped attempt-list read. Absence of any Work in scope is a
/// typed state, distinct from an authorized-but-empty page.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkAttemptListV1 {
    /// No Work exists in this authority scope, so there is no attempt set to
    /// page. Concealed scopes never reach this state: they are refused as
    /// not-found-or-not-authorized before any read.
    Absent,
    /// One page of durable attempts under the verified topology snapshot.
    Listed {
        topology: WorkAttemptTopologyBindingV1,
        attempts: Vec<WorkAttemptV1>,
        coverage: WorkAttemptListCoverageV1,
    },
}

/// The lease, transition, cancellation, recovery, and evidence authority for
/// admitted provider attempts.
pub struct WorkAttemptService<S, P, W> {
    attempts: S,
    projections: P,
    work: WorkService<W>,
}

impl<S, P, W> WorkAttemptService<S, P, W>
where
    S: WorkAttemptStoragePort,
    P: WorkProjectionReadPort,
    W: WorkStoragePort,
{
    pub const fn new(attempts: S, projections: P, work: WorkService<W>) -> Self {
        Self {
            attempts,
            projections,
            work,
        }
    }

    /// Admits and leases one provider attempt against the exact current Work
    /// projection. Execution must already be admitted on the projection; a
    /// missing admission or accepted proposal is a typed denial, not a queue.
    pub fn start(
        &self,
        context: &RequestContext,
        command: StartWorkAttemptCommand,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        self.start_attempt(context, command)
    }

    /// Starts an attempt only when its caller-supplied snapshot agrees with
    /// the topology authority pinned by the daemon registration. The command
    /// cannot self-attest a different topology and gain a provider lease.
    pub fn start_against_registered_topology(
        &self,
        context: &RequestContext,
        registered_topology: &WorkTopologyPolicyV1,
        command: StartWorkAttemptCommand,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        require_registered_work_topology(&command.execution_snapshot, registered_topology)?;
        self.start_attempt_bounded(context, command, registered_topology)
    }

    pub fn status(
        &self,
        context: &RequestContext,
        request: &WorkAttemptStatusRequestV1,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        let authority = work_authority(context)?;
        let identity = WorkAttemptIdentityV1::new(
            request.task_id.clone(),
            request.run_id.clone(),
            request.attempt_id.clone(),
        )
        .map_err(contract_problem)?;
        self.attempts
            .load(&authority, &identity)
            .map_err(storage_problem)
    }

    /// Lists one authority-scoped, page-bounded slice of provider attempts in
    /// stable task/run/attempt order, read under the verified Work topology
    /// snapshot the caller resolves through the graph publication mount.
    ///
    /// Every non-success is typed: an out-of-bounds page size is an invalid
    /// request, a cursor minted under a superseded topology generation is
    /// stale, a scope with no Work at all is the explicit `Absent` state, and
    /// an authorized scope with no attempts is an explicit zero-complete page.
    pub fn list(
        &self,
        context: &RequestContext,
        request: &WorkAttemptListRequestV1,
        topology: impl FnOnce(&WorkAuthority) -> Result<WorkAttemptTopologyStateV1, ApplicationProblem>,
    ) -> Result<WorkAttemptListV1, ApplicationProblem> {
        if request.page_size == 0 || request.page_size > MAX_WORK_ATTEMPT_LIST_PAGE_SIZE {
            return Err(invalid_problem(
                "application.work-attempt.invalid-page-size",
                "The Work attempt list page size must be between 1 and 1000.",
            ));
        }
        let authority = work_authority(context)?;
        let binding = match topology(&authority)? {
            WorkAttemptTopologyStateV1::Absent => {
                return if request.cursor.is_some() {
                    // The snapshot the cursor was minted under no longer
                    // exists for this scope; resuming would fabricate a page.
                    Err(stale_cursor_problem())
                } else {
                    Ok(WorkAttemptListV1::Absent)
                };
            }
            WorkAttemptTopologyStateV1::Verified(binding) => binding,
        };
        if let Some(cursor) = &request.cursor
            && cursor.generation != binding.generation
        {
            return Err(stale_cursor_problem());
        }
        let page = self
            .attempts
            .list(
                &authority,
                request.cursor.as_ref().map(|cursor| &cursor.start_after),
                request.page_size,
            )
            .map_err(storage_problem)?;
        let returned = u32::try_from(page.attempts.len())
            .ok()
            .filter(|returned| *returned <= request.page_size && *returned <= page.remaining)
            .ok_or_else(list_page_contract_problem)?;
        let coverage = if returned == page.remaining {
            WorkAttemptListCoverageV1::Complete { returned }
        } else {
            let last = page
                .attempts
                .last()
                .ok_or_else(list_page_contract_problem)?;
            WorkAttemptListCoverageV1::Capped {
                returned,
                remaining: page.remaining - returned,
                resume: WorkAttemptListCursorV1 {
                    generation: binding.generation.clone(),
                    start_after: last.identity().clone(),
                },
            }
        };
        Ok(WorkAttemptListV1::Listed {
            topology: binding,
            attempts: page.attempts,
            coverage,
        })
    }

    /// Records a cancellation request against an open attempt. A leased or
    /// recovery-required attempt can be cancelled before provider startup;
    /// the daemon runtime observes the durable request and produces no
    /// provider effect.
    pub fn request_cancellation(
        &self,
        context: &RequestContext,
        command: CancelWorkAttemptCommand,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        admit(context, command.occurred_at)?;
        let authority = work_authority(context)?;
        let identity = WorkAttemptIdentityV1::new(
            command.task_id.clone(),
            command.run_id.clone(),
            command.attempt_id.clone(),
        )
        .map_err(contract_problem)?;
        let attempt = self
            .attempts
            .load(&authority, &identity)
            .map_err(storage_problem)?;
        if let Some(request) = cancellation_request(attempt.cancellation()) {
            return if request.request_id() == &command.request_id {
                Ok(attempt)
            } else {
                Err(conflict_problem(
                    "application.work-attempt.cancellation-conflict",
                    "A different cancellation request is already recorded.",
                ))
            };
        }
        if !matches!(
            attempt.state(),
            WorkAttemptStateV1::Leased
                | WorkAttemptStateV1::Running
                | WorkAttemptStateV1::RecoveryRequired
        ) {
            return Err(conflict_problem(
                "application.work-attempt.not-cancellable",
                "Only an open Work attempt can accept a cancellation request.",
            ));
        }
        let request = WorkCancellationRequestV1::new(command.request_id, command.occurred_at)
            .map_err(contract_problem)?;
        let next = attempt
            .transition(
                WorkAttemptStateV1::CancellationRequested,
                attempt.progress(),
                attempt.artifacts().to_vec(),
                WorkCancellationStateV1::Requested(request),
                attempt.recovery().clone(),
                attempt.actual_route().cloned(),
                None,
                attempt.lease().clone(),
            )
            .map_err(contract_problem)?;
        self.persist_transition(&authority, &attempt, &next, None)?;
        Ok(next)
    }

    /// Fences every open attempt onto a fresh epoch after a daemon restart.
    ///
    /// Leased and running attempts become `RecoveryRequired` under the new
    /// fence: the old lease can no longer advance the row, and no process
    /// exit, PID, or elapsed time is accepted as proof of anything.
    /// Attempts with an in-flight cancellation complete their cancellation,
    /// because the process they were cancelling is gone.
    pub fn resume(
        &self,
        context: &RequestContext,
        command: &ResumeWorkAttemptsCommand,
    ) -> Result<WorkAttemptRecoveryReportV1, ApplicationProblem> {
        admit(context, command.occurred_at)?;
        let authority = work_authority(context)?;
        let open = self
            .attempts
            .open_attempts(&authority)
            .map_err(storage_problem)?;
        let mut recovery_required = Vec::new();
        let mut cancelled = Vec::new();
        for attempt in open {
            match attempt.state() {
                WorkAttemptStateV1::Leased | WorkAttemptStateV1::Running => {
                    let fenced = self.fence_to_recovery(
                        &authority,
                        &attempt,
                        WorkRestartReasonV1::ProcessLost,
                    )?;
                    recovery_required.push(fenced);
                }
                WorkAttemptStateV1::CancellationRequested
                | WorkAttemptStateV1::CancellationAcknowledged
                | WorkAttemptStateV1::CancellationEscalated => {
                    let completed =
                        self.complete_lost_cancellation(&authority, attempt, command.occurred_at)?;
                    cancelled.push(completed);
                }
                WorkAttemptStateV1::RecoveryRequired => {
                    recovery_required.push(attempt);
                }
                WorkAttemptStateV1::Succeeded
                | WorkAttemptStateV1::Failed
                | WorkAttemptStateV1::TimedOut
                | WorkAttemptStateV1::Cancelled => {}
            }
        }
        Ok(WorkAttemptRecoveryReportV1 {
            recovery_required,
            cancelled,
        })
    }

    /// Marks negotiation success: the provider process is running under the
    /// exact admitted route.
    pub fn mark_running(
        &self,
        context: &RequestContext,
        identity: &WorkAttemptIdentityV1,
        actual_route: WorkProviderRouteV1,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        let authority = work_authority(context)?;
        let attempt = self
            .attempts
            .load(&authority, identity)
            .map_err(storage_problem)?;
        let recovery = match attempt.state() {
            WorkAttemptStateV1::Leased => attempt.recovery().clone(),
            WorkAttemptStateV1::RecoveryRequired => match attempt.recovery() {
                WorkRecoveryStateV1::RecoveryRequired {
                    source_attempt_id: Some(source),
                    reason,
                } => WorkRecoveryStateV1::Restarted {
                    source_attempt_id: source.clone(),
                    reason: *reason,
                },
                _ => WorkRecoveryStateV1::Fresh,
            },
            _ => attempt.recovery().clone(),
        };
        let next = attempt
            .transition(
                WorkAttemptStateV1::Running,
                attempt.progress(),
                attempt.artifacts().to_vec(),
                WorkCancellationStateV1::None,
                recovery,
                Some(actual_route),
                None,
                attempt.lease().clone(),
            )
            .map_err(contract_problem)?;
        self.persist_transition(&authority, &attempt, &next, None)?;
        Ok(next)
    }

    /// Records a typed provider-availability denial before the process ever
    /// started. This is a product state, not a transport error, and it never
    /// routes to a different provider.
    pub fn mark_provider_unavailable(
        &self,
        context: &RequestContext,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        let authority = work_authority(context)?;
        let attempt = self
            .attempts
            .load(&authority, identity)
            .map_err(storage_problem)?;
        let fenced = self.fence_to_recovery(
            &authority,
            &attempt,
            WorkRestartReasonV1::ProviderUnavailable,
        )?;
        Ok(fenced)
    }

    /// Acknowledges a durable cancellation request from inside the runtime.
    pub fn acknowledge_cancellation(
        &self,
        context: &RequestContext,
        identity: &WorkAttemptIdentityV1,
        acknowledged_at: UtcMicros,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        let authority = work_authority(context)?;
        let attempt = self
            .attempts
            .load(&authority, identity)
            .map_err(storage_problem)?;
        let WorkCancellationStateV1::Requested(request) = attempt.cancellation().clone() else {
            return Err(conflict_problem(
                "application.work-attempt.cancellation-not-requested",
                "There is no pending cancellation request to acknowledge.",
            ));
        };
        let acknowledgement = WorkCancellationAcknowledgementV1::new(request, acknowledged_at)
            .map_err(contract_problem)?;
        let next = attempt
            .transition(
                WorkAttemptStateV1::CancellationAcknowledged,
                attempt.progress(),
                attempt.artifacts().to_vec(),
                WorkCancellationStateV1::Acknowledged(acknowledgement),
                attempt.recovery().clone(),
                attempt.actual_route().cloned(),
                None,
                attempt.lease().clone(),
            )
            .map_err(contract_problem)?;
        self.persist_transition(&authority, &attempt, &next, None)?;
        Ok(next)
    }

    /// Escalates an acknowledged cancellation to forced termination.
    pub fn escalate_cancellation(
        &self,
        context: &RequestContext,
        identity: &WorkAttemptIdentityV1,
        escalated_at: UtcMicros,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        let authority = work_authority(context)?;
        let attempt = self
            .attempts
            .load(&authority, identity)
            .map_err(storage_problem)?;
        let WorkCancellationStateV1::Acknowledged(acknowledgement) = attempt.cancellation().clone()
        else {
            return Err(conflict_problem(
                "application.work-attempt.cancellation-not-acknowledged",
                "There is no acknowledged cancellation to escalate.",
            ));
        };
        let escalation = WorkCancellationEscalationV1::new(acknowledgement, escalated_at)
            .map_err(contract_problem)?;
        let next = attempt
            .transition(
                WorkAttemptStateV1::CancellationEscalated,
                attempt.progress(),
                attempt.artifacts().to_vec(),
                WorkCancellationStateV1::Escalated(escalation),
                attempt.recovery().clone(),
                attempt.actual_route().cloned(),
                None,
                attempt.lease().clone(),
            )
            .map_err(contract_problem)?;
        self.persist_transition(&authority, &attempt, &next, None)?;
        Ok(next)
    }

    /// Seals the attempt with terminal evidence and attaches the resulting
    /// `RuntimeEvidenceRef` to the Work projection through the canonical Work
    /// command authority. The attach command identity is derived from the
    /// attempt, so a replayed settlement cannot double-append.
    pub fn settle(
        &self,
        context: &RequestContext,
        identity: &WorkAttemptIdentityV1,
        evidence: &WorkAttemptEvidenceRecordV1,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        self.settle_with_artifacts(context, identity, evidence, Vec::new())
    }

    pub fn settle_with_artifacts(
        &self,
        context: &RequestContext,
        identity: &WorkAttemptIdentityV1,
        evidence: &WorkAttemptEvidenceRecordV1,
        artifacts: Vec<tracedecay_domain::WorkArtifactRefV1>,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        let authority = work_authority(context)?;
        let attempt = self
            .attempts
            .load(&authority, identity)
            .map_err(storage_problem)?;
        let digest = evidence.digest()?;
        let (state, terminal) =
            terminal_for_outcome(&evidence.outcome, digest, evidence.observed_at)
                .map_err(contract_problem)?;
        let artifacts = if artifacts.is_empty() {
            attempt.artifacts().to_vec()
        } else {
            artifacts
        };
        let next = attempt
            .transition(
                state,
                attempt.progress(),
                artifacts,
                attempt.cancellation().clone(),
                attempt.recovery().clone(),
                evidence
                    .actual_route
                    .clone()
                    .or_else(|| attempt.actual_route().cloned()),
                Some(terminal.clone()),
                attempt.lease().clone(),
            )
            .map_err(contract_problem)?;
        self.persist_transition(&authority, &attempt, &next, Some(evidence))?;
        self.attach_evidence(context, &next, &terminal, evidence.observed_at)?;
        Ok(next)
    }

    /// Fails an attempt that cannot be recovered, sealing denial evidence.
    pub fn fail_recovery(
        &self,
        context: &RequestContext,
        identity: &WorkAttemptIdentityV1,
        evidence: &WorkAttemptEvidenceRecordV1,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        let authority = work_authority(context)?;
        let attempt = self
            .attempts
            .load(&authority, identity)
            .map_err(storage_problem)?;
        if attempt.state() != WorkAttemptStateV1::RecoveryRequired {
            return Err(conflict_problem(
                "application.work-attempt.not-recovery-required",
                "Only an attempt awaiting recovery can be failed this way.",
            ));
        }
        let digest = evidence.digest()?;
        let terminal = WorkTerminalEvidenceV1::failed(digest, evidence.observed_at)
            .map_err(contract_problem)?;
        // A recovery-required attempt may never have negotiated a provider;
        // Failed requires an actual route, so denial keeps the requested
        // route as the truthfully-not-started actual route only when the
        // provider had already been negotiated before the loss.
        let actual_route = evidence
            .actual_route
            .clone()
            .or_else(|| attempt.actual_route().cloned())
            .unwrap_or_else(|| attempt.requested_route().clone());
        let next = attempt
            .transition(
                WorkAttemptStateV1::Failed,
                attempt.progress(),
                attempt.artifacts().to_vec(),
                attempt.cancellation().clone(),
                attempt.recovery().clone(),
                Some(actual_route),
                Some(terminal.clone()),
                attempt.lease().clone(),
            )
            .map_err(contract_problem)?;
        self.persist_transition(&authority, &attempt, &next, Some(evidence))?;
        self.attach_evidence(context, &next, &terminal, evidence.observed_at)?;
        Ok(next)
    }

    fn attach_evidence(
        &self,
        context: &RequestContext,
        attempt: &WorkAttemptV1,
        terminal: &WorkTerminalEvidenceV1,
        observed_at: UtcMicros,
    ) -> Result<(), ApplicationProblem> {
        let evidence: RuntimeEvidenceRef = terminal
            .runtime_evidence_ref(attempt.identity().run_id().clone())
            .map_err(contract_problem)?;
        let projection = self.work.load(context, attempt.identity().task_id())?;
        let command_id = derived_command_id(attempt.identity())?;
        self.work.attach_runtime_evidence(
            context,
            AttachRuntimeEvidenceCommand {
                task_id: attempt.identity().task_id().clone(),
                evidence,
                expected_version: projection.version(),
                command_id,
                occurred_at: observed_at,
            },
        )?;
        Ok(())
    }

    fn mint_lease(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkLeaseFenceV1, ApplicationProblem> {
        let digest = canonical_sha256(&("tracedecay.application.work-lease.v1", identity))
            .map_err(|_| {
                invalid_problem(
                    "application.work-attempt.invalid-lease",
                    "The Work attempt lease identity could not be derived.",
                )
            })?;
        let lease_id = WorkLeaseId::new(format!(
            "work-lease:{}",
            digest.as_str().trim_start_matches("sha256:")
        ))
        .map_err(|_| {
            invalid_problem(
                "application.work-attempt.invalid-lease",
                "The Work attempt lease identity could not be derived.",
            )
        })?;
        let epoch = self
            .attempts
            .next_fence_epoch(authority)
            .map_err(storage_problem)?;
        let epoch = WorkFenceEpochV1::new(epoch).map_err(contract_problem)?;
        WorkLeaseFenceV1::new(lease_id, epoch).map_err(contract_problem)
    }

    fn fence_to_recovery(
        &self,
        authority: &WorkAuthority,
        attempt: &WorkAttemptV1,
        reason: WorkRestartReasonV1,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        let epoch = self
            .attempts
            .next_fence_epoch(authority)
            .map_err(storage_problem)?;
        let epoch = WorkFenceEpochV1::new(epoch).map_err(contract_problem)?;
        let fence = WorkLeaseFenceV1::new(attempt.lease().lease_id().clone(), epoch)
            .map_err(contract_problem)?;
        let next = attempt
            .transition(
                WorkAttemptStateV1::RecoveryRequired,
                attempt.progress(),
                attempt.artifacts().to_vec(),
                WorkCancellationStateV1::None,
                WorkRecoveryStateV1::RecoveryRequired {
                    source_attempt_id: None,
                    reason,
                },
                attempt.actual_route().cloned(),
                None,
                fence,
            )
            .map_err(contract_problem)?;
        self.persist_transition(authority, attempt, &next, None)?;
        Ok(next)
    }

    fn complete_lost_cancellation(
        &self,
        authority: &WorkAuthority,
        attempt: WorkAttemptV1,
        observed_at: UtcMicros,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        // Advance the ladder as far as the recorded request allows, then seal
        // a truthful Cancelled terminal: the process is provably gone with
        // the daemon that owned it.
        let cancellation = match attempt.cancellation().clone() {
            WorkCancellationStateV1::Requested(request) => WorkCancellationStateV1::Acknowledged(
                WorkCancellationAcknowledgementV1::new(request, observed_at)
                    .map_err(contract_problem)?,
            ),
            other => other,
        };
        let intermediate = if matches!(attempt.state(), WorkAttemptStateV1::CancellationRequested) {
            let next = attempt
                .transition(
                    WorkAttemptStateV1::CancellationAcknowledged,
                    attempt.progress(),
                    attempt.artifacts().to_vec(),
                    cancellation.clone(),
                    attempt.recovery().clone(),
                    attempt.actual_route().cloned(),
                    None,
                    attempt.lease().clone(),
                )
                .map_err(contract_problem)?;
            self.persist_transition(authority, &attempt, &next, None)?;
            next
        } else {
            attempt
        };
        let evidence = WorkAttemptEvidenceRecordV1 {
            identity: intermediate.identity().clone(),
            requested_route: intermediate.requested_route().clone(),
            actual_route: intermediate.actual_route().cloned(),
            outcome: WorkAttemptProviderOutcomeV1::Cancelled,
            stdout: None,
            stderr: None,
            provider_session: None,
            // Route selection happens in the daemon runtime, which is not on
            // this path: a cancellation observed by the authority itself
            // never re-decides a backend.
            provider_fallback: None,
            observed_at,
        };
        let digest = evidence.digest()?;
        let terminal =
            WorkTerminalEvidenceV1::cancelled(digest, observed_at).map_err(contract_problem)?;
        let next = intermediate
            .transition(
                WorkAttemptStateV1::Cancelled,
                intermediate.progress(),
                intermediate.artifacts().to_vec(),
                intermediate.cancellation().clone(),
                intermediate.recovery().clone(),
                intermediate.actual_route().cloned(),
                Some(terminal),
                intermediate.lease().clone(),
            )
            .map_err(contract_problem)?;
        self.persist_transition(authority, &intermediate, &next, Some(&evidence))?;
        Ok(next)
    }

    fn persist_transition(
        &self,
        authority: &WorkAuthority,
        previous: &WorkAttemptV1,
        next: &WorkAttemptV1,
        evidence: Option<&WorkAttemptEvidenceRecordV1>,
    ) -> Result<(), ApplicationProblem> {
        self.attempts
            .update(
                authority,
                previous.lease(),
                previous.state(),
                next,
                evidence,
            )
            .map_err(storage_problem)
    }
}

/// Refuses a caller-provided execution snapshot that does not agree with the
/// registered topology authority. Both ordinary and synthesis admission call
/// this before a provider lease can be observed by the daemon.
pub fn require_registered_work_topology(
    snapshot: &WorkExecutionSnapshot,
    registered_topology: &WorkTopologyPolicyV1,
) -> Result<(), ApplicationProblem> {
    if snapshot.topology() == registered_topology {
        return Ok(());
    }
    Err(conflict_problem(
        "application.work-attempt.topology-conflict",
        "The Work attempt topology differs from the registered runtime authority.",
    ))
}

fn terminal_for_outcome(
    outcome: &WorkAttemptProviderOutcomeV1,
    digest: ManifestDigest,
    observed_at: UtcMicros,
) -> Result<(WorkAttemptStateV1, WorkTerminalEvidenceV1), WorkRuntimeContractError> {
    match outcome {
        WorkAttemptProviderOutcomeV1::Exited { code: 0 } => Ok((
            WorkAttemptStateV1::Succeeded,
            WorkTerminalEvidenceV1::succeeded(digest, observed_at)?,
        )),
        WorkAttemptProviderOutcomeV1::Exited { .. }
        | WorkAttemptProviderOutcomeV1::Signalled { .. }
        | WorkAttemptProviderOutcomeV1::ProviderUnavailable { .. }
        | WorkAttemptProviderOutcomeV1::StreamOverflow { .. }
        | WorkAttemptProviderOutcomeV1::LaunchFailed
        | WorkAttemptProviderOutcomeV1::ProtocolFailed => Ok((
            WorkAttemptStateV1::Failed,
            WorkTerminalEvidenceV1::failed(digest, observed_at)?,
        )),
        WorkAttemptProviderOutcomeV1::TimedOut => Ok((
            WorkAttemptStateV1::TimedOut,
            WorkTerminalEvidenceV1::timed_out(digest, observed_at)?,
        )),
        WorkAttemptProviderOutcomeV1::Cancelled => Ok((
            WorkAttemptStateV1::Cancelled,
            WorkTerminalEvidenceV1::cancelled(digest, observed_at)?,
        )),
    }
}

fn cancellation_request(state: &WorkCancellationStateV1) -> Option<&WorkCancellationRequestV1> {
    match state {
        WorkCancellationStateV1::None => None,
        WorkCancellationStateV1::Requested(request) => Some(request),
        WorkCancellationStateV1::Acknowledged(acknowledgement) => Some(acknowledgement.request()),
        WorkCancellationStateV1::Escalated(escalation) => {
            Some(escalation.acknowledgement().request())
        }
    }
}

fn derived_command_id(
    identity: &WorkAttemptIdentityV1,
) -> Result<WorkCommandId, ApplicationProblem> {
    let digest = canonical_sha256(&("tracedecay.application.work-attempt-attach.v1", identity))
        .map_err(|_| {
            invalid_problem(
                "application.work-attempt.invalid-command",
                "The Work attempt attach command identity could not be derived.",
            )
        })?;
    WorkCommandId::new(format!(
        "work-attempt-evidence:{}",
        digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|_| {
        invalid_problem(
            "application.work-attempt.invalid-command",
            "The Work attempt attach command identity could not be derived.",
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
