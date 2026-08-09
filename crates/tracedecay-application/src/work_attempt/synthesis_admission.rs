//! Atomic ordinary/synthesis attempt admission over one durable row authority.

use tracedecay_domain::{
    ManifestDigest, WorkAttemptIdentityV1, WorkAttemptProjectionBindingV1, WorkAttemptStateV1,
    WorkAttemptV1, WorkAuthority, WorkCancellationStateV1, WorkExecutionEnvelopeV1,
    WorkProviderRouteV1, WorkRecoveryStateV1,
};

use crate::work::{WorkStoragePort, work_authority};
use crate::work_read::WorkProjectionReadPort;
use crate::work_synthesis::{WorkSynthesisAdmissionRecordV1, WorkSynthesisAdmissionV1};
use crate::{ApplicationProblem, RequestContext};

use super::{
    StartWorkAttemptCommand, WorkAttemptInsertOutcome, WorkAttemptService, WorkAttemptStorageError,
    WorkAttemptStoragePort, admit, conflict_problem, contract_problem, denied_problem,
    projection_problem, storage_problem,
};

/// Outcome of atomically inserting an admitted synthesis and its attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkSynthesisInsertOutcome {
    Inserted,
    Replayed(Box<WorkSynthesisAdmissionV1>),
}

/// Which durable admission authority owns an attempt identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkAttemptAdmissionKind {
    Ordinary,
    Synthesis,
}

/// Atomic synthesis persistence required by each synthesis-capable attempt authority.
pub trait WorkSynthesisAdmissionStoragePort: WorkAttemptStoragePort {
    /// Inserts the attempt and complete synthesis admission as one durable
    /// record, or returns the stored admission for an identical request.
    fn insert_synthesis(
        &self,
        authority: &WorkAuthority,
        record: &WorkSynthesisAdmissionRecordV1,
    ) -> Result<WorkSynthesisInsertOutcome, WorkAttemptStorageError>;

    fn insert_synthesis_bounded(
        &self,
        authority: &WorkAuthority,
        record: &WorkSynthesisAdmissionRecordV1,
        concurrency: &tracedecay_domain::configuration::TopologyConcurrencyPolicyV1,
    ) -> Result<WorkSynthesisInsertOutcome, WorkAttemptStorageError>;

    /// Loads the immutable synthesis admission. An ordinary row is a typed
    /// attempt conflict, while a missing row remains a typed not-found result.
    fn load_synthesis(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkSynthesisAdmissionRecordV1, WorkAttemptStorageError>;
}

struct PreparedWorkAttemptAdmission {
    authority: WorkAuthority,
    identity: WorkAttemptIdentityV1,
    binding: WorkAttemptProjectionBindingV1,
    envelope: WorkExecutionEnvelopeV1,
    requested_route: WorkProviderRouteV1,
}

impl<S, P, W> WorkAttemptService<S, P, W>
where
    S: WorkAttemptStoragePort,
    P: WorkProjectionReadPort,
    W: WorkStoragePort,
{
    pub(super) fn start_attempt(
        &self,
        context: &RequestContext,
        command: StartWorkAttemptCommand,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        self.start_attempt_with_topology(context, command, None)
    }

    pub(super) fn start_attempt_bounded(
        &self,
        context: &RequestContext,
        command: StartWorkAttemptCommand,
        registered_topology: &tracedecay_domain::configuration::WorkTopologyPolicyV1,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        self.start_attempt_with_topology(context, command, Some(registered_topology))
    }

    fn start_attempt_with_topology(
        &self,
        context: &RequestContext,
        command: StartWorkAttemptCommand,
        registered_topology: Option<&tracedecay_domain::configuration::WorkTopologyPolicyV1>,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        let prepared = self.prepare_start(context, command)?;
        // Replay before minting a lease. Ordinary replay is allowed only for
        // an ordinary row; a synthesis row carries additional request
        // identity material that an ordinary start cannot prove.
        match self.attempts.load(&prepared.authority, &prepared.identity) {
            Ok(existing) => {
                let admission_kind = self
                    .attempts
                    .load_admission_kind(&prepared.authority, &prepared.identity)
                    .map_err(storage_problem)?;
                if admission_kind == WorkAttemptAdmissionKind::Synthesis {
                    return Err(storage_problem(WorkAttemptStorageError::AttemptConflict));
                }
                return if existing
                    .execution()
                    .same_admission_content(&prepared.envelope)
                    && existing.projection_binding().accepted_proposal()
                        == prepared.binding.accepted_proposal()
                {
                    Ok(existing)
                } else {
                    Err(conflict_problem(
                        "application.work-attempt.identity-conflict",
                        "The Work attempt identity was already used with different content.",
                    ))
                };
            }
            Err(WorkAttemptStorageError::NotFoundOrNotAuthorized) => {}
            Err(error) => return Err(storage_problem(error)),
        }
        let authority = prepared.authority.clone();
        let attempt = self.lease_prepared(prepared)?;
        let inserted = match registered_topology {
            Some(topology) => {
                self.attempts
                    .insert_bounded(&authority, &attempt, &topology.concurrency)
            }
            None => self.attempts.insert(&authority, &attempt),
        };
        match inserted {
            Ok(WorkAttemptInsertOutcome::Inserted) => Ok(attempt),
            Ok(WorkAttemptInsertOutcome::Replayed(existing)) => Ok(*existing),
            Err(error) => Err(storage_problem(error)),
        }
    }

    fn prepare_start(
        &self,
        context: &RequestContext,
        command: StartWorkAttemptCommand,
    ) -> Result<PreparedWorkAttemptAdmission, ApplicationProblem> {
        admit(context, command.occurred_at)?;
        let authority = work_authority(context)?;
        let snapshot = self
            .projections
            .exact_snapshot(&authority, &command.task_id)
            .map_err(projection_problem)?;
        let projection = snapshot
            .projections()
            .iter()
            .find(|projection| projection.task_id() == &command.task_id)
            .ok_or_else(super::not_found_problem)?;
        if !projection.is_execution_admitted() {
            return Err(denied_problem(
                "application.work-attempt.execution-not-admitted",
                "Work execution has not been admitted for this task.",
            ));
        }
        let accepted_proposal = projection.accepted_proposal().ok_or_else(|| {
            denied_problem(
                "application.work-attempt.no-accepted-proposal",
                "Work has no accepted proposal to execute.",
            )
        })?;
        let identity =
            WorkAttemptIdentityV1::new(command.task_id.clone(), command.run_id, command.attempt_id)
                .map_err(contract_problem)?;
        let binding = WorkAttemptProjectionBindingV1::new(
            snapshot.generation_id().clone(),
            snapshot.sequence(),
            projection.version(),
            accepted_proposal.clone(),
        )
        .map_err(contract_problem)?;
        let requested_route = command.execution_snapshot.route().clone();
        let envelope = WorkExecutionEnvelopeV1::new(
            identity.clone(),
            binding.clone(),
            command.operation,
            command.execution_snapshot,
            context.scope().project_id.clone(),
            context.scope().repository_id.clone(),
            context.scope().worktree_id.clone(),
            command.worktree_root,
            command.reference,
            command.commit,
            command.instructions,
            1,
            command.effect_state,
        )
        .map_err(contract_problem)?;
        Ok(PreparedWorkAttemptAdmission {
            authority,
            identity,
            binding,
            envelope,
            requested_route,
        })
    }

    fn lease_prepared(
        &self,
        prepared: PreparedWorkAttemptAdmission,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        let lease = self.mint_lease(&prepared.authority, &prepared.identity)?;
        WorkAttemptV1::new(
            prepared.identity,
            prepared.binding,
            prepared.envelope,
            lease,
            WorkAttemptStateV1::Leased,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            prepared.requested_route,
            None,
            None,
        )
        .map_err(contract_problem)
    }
}

impl<S, P, W> WorkAttemptService<S, P, W>
where
    S: WorkSynthesisAdmissionStoragePort,
    P: WorkProjectionReadPort,
    W: WorkStoragePort,
{
    pub(crate) fn synthesis_replay(
        &self,
        context: &RequestContext,
        command: &StartWorkAttemptCommand,
        request_digest: &ManifestDigest,
    ) -> Result<Option<WorkSynthesisAdmissionV1>, ApplicationProblem> {
        admit(context, command.occurred_at)?;
        let authority = work_authority(context)?;
        let identity = WorkAttemptIdentityV1::new(
            command.task_id.clone(),
            command.run_id.clone(),
            command.attempt_id.clone(),
        )
        .map_err(contract_problem)?;
        match self.attempts.load_synthesis(&authority, &identity) {
            Ok(record) if &record.request_digest == request_digest => Ok(Some(record.result)),
            Ok(_) => Err(storage_problem(WorkAttemptStorageError::AttemptConflict)),
            Err(WorkAttemptStorageError::NotFoundOrNotAuthorized) => Ok(None),
            Err(error) => Err(storage_problem(error)),
        }
    }

    pub(crate) fn start_synthesis<F>(
        &self,
        context: &RequestContext,
        command: StartWorkAttemptCommand,
        request_digest: ManifestDigest,
        registered_topology: Option<&tracedecay_domain::configuration::WorkTopologyPolicyV1>,
        build_result: F,
    ) -> Result<WorkSynthesisAdmissionV1, ApplicationProblem>
    where
        F: FnOnce(WorkAttemptV1) -> WorkSynthesisAdmissionV1,
    {
        let prepared = self.prepare_start(context, command)?;
        match self
            .attempts
            .load_synthesis(&prepared.authority, &prepared.identity)
        {
            Ok(record) if record.request_digest == request_digest => return Ok(record.result),
            Ok(_) => return Err(storage_problem(WorkAttemptStorageError::AttemptConflict)),
            Err(WorkAttemptStorageError::NotFoundOrNotAuthorized) => {}
            Err(error) => return Err(storage_problem(error)),
        }
        let authority = prepared.authority.clone();
        let attempt = self.lease_prepared(prepared)?;
        let result = build_result(attempt);
        let record = WorkSynthesisAdmissionRecordV1 {
            request_digest,
            result,
        };
        let inserted = match registered_topology {
            Some(topology) => {
                self.attempts
                    .insert_synthesis_bounded(&authority, &record, &topology.concurrency)
            }
            None => self.attempts.insert_synthesis(&authority, &record),
        };
        match inserted {
            Ok(WorkSynthesisInsertOutcome::Inserted) => Ok(record.result),
            Ok(WorkSynthesisInsertOutcome::Replayed(result)) => Ok(*result),
            Err(error) => Err(storage_problem(error)),
        }
    }
}
