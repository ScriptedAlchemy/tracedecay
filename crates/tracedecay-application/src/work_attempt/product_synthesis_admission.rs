//! Atomic synthesis admission against the verified Work product graph.

use tracedecay_domain::{
    ManifestDigest, WorkAttemptIdentityV1, WorkAttemptStateV1, WorkAttemptV1, WorkAuthority,
    WorkCancellationStateV1, WorkCommandId, WorkExecutionEnvelopeV1, WorkFenceEpochV1,
    WorkLeaseFenceV1, WorkLeaseId, WorkRecoveryStateV1, canonical_sha256,
};

use crate::{
    ApplicationProblem, RequestContext, WorkGraphReadPortV1, WorkProductAttemptAdmissionPortV1,
    WorkProductAttemptAdmissionV1, WorkProductBindingV1, WorkProductOwnerAuthorizationPortV1,
    WorkProductRevisionPinsV1, WorkProductSynthesisAdmissionV1, WorkSynthesisAdmissionRecordV1,
    WorkSynthesisAdmissionV1,
};

use super::{
    CurrentWorkProductAttemptGraphV1, StartWorkAttemptCommand, WorkAttemptStorageError,
    WorkAttemptStoragePort, WorkSynthesisAdmissionStoragePort, WorkSynthesisInsertOutcome,
    accepted_attempt_draft, conflict_problem, contract_problem, current_work_product_attempt_graph,
    denied_problem, not_found_problem, product_admission_problem,
    product_attempt_projection_binding, storage_problem,
};

const COMMAND_DOMAIN: &str = "tracedecay.application.work-product-synthesis-command.final-v2";
const LEASE_DOMAIN: &str = "tracedecay.application.work-product-synthesis-lease.final-v2";

pub struct WorkProductSynthesisAttemptServiceV1<S> {
    storage: S,
}

struct PreparedSynthesisV1 {
    product: CurrentWorkProductAttemptGraphV1,
    authority: WorkAuthority,
    identity: WorkAttemptIdentityV1,
    binding: tracedecay_domain::WorkAttemptProjectionBindingV1,
}

impl<S> WorkProductSynthesisAttemptServiceV1<S>
where
    S: WorkSynthesisAdmissionStoragePort
        + WorkGraphReadPortV1
        + WorkProductOwnerAuthorizationPortV1
        + WorkProductAttemptAdmissionPortV1,
{
    pub const fn new(storage: S) -> Self {
        Self { storage }
    }

    pub fn status(
        &self,
        context: &RequestContext,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        self.storage
            .load(&crate::work::work_authority(context)?, identity)
            .map_err(storage_problem)
    }

    pub fn replay(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        command: &StartWorkAttemptCommand,
        request_digest: &ManifestDigest,
    ) -> Result<Option<WorkSynthesisAdmissionV1>, ApplicationProblem> {
        let prepared = self.prepare(context, binding, command)?;
        match self
            .storage
            .load_synthesis(&prepared.authority, &prepared.identity)
        {
            Ok(record) if &record.request_digest == request_digest => {
                if record.result.attempt.projection_binding() != &prepared.binding {
                    return Err(identity_conflict());
                }
                Ok(Some(record.result))
            }
            Ok(_) => Err(identity_conflict()),
            Err(WorkAttemptStorageError::NotFoundOrNotAuthorized) => Ok(None),
            Err(error) => Err(storage_problem(error)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn admit<F>(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        revisions: &WorkProductRevisionPinsV1,
        topology: &tracedecay_domain::WorkTopologyPolicyV1,
        command: StartWorkAttemptCommand,
        request_digest: ManifestDigest,
        build_result: F,
    ) -> Result<WorkSynthesisAdmissionV1, ApplicationProblem>
    where
        F: FnOnce(WorkAttemptV1) -> WorkSynthesisAdmissionV1,
    {
        let prepared = self.prepare(context, binding, &command)?;
        match self
            .storage
            .load_synthesis(&prepared.authority, &prepared.identity)
        {
            Ok(record) if record.request_digest == request_digest => {
                if record.result.attempt.projection_binding() != &prepared.binding {
                    return Err(identity_conflict());
                }
                return Ok(record.result);
            }
            Ok(_) => return Err(identity_conflict()),
            Err(WorkAttemptStorageError::NotFoundOrNotAuthorized) => {}
            Err(error) => return Err(storage_problem(error)),
        }
        let requested_route = command.execution_snapshot.route().clone();
        let envelope = WorkExecutionEnvelopeV1::new(
            prepared.identity.clone(),
            prepared.binding.clone(),
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
        let attempt = WorkAttemptV1::new(
            prepared.identity.clone(),
            prepared.binding,
            envelope,
            mint_lease(&self.storage, &prepared.authority, &prepared.identity)?,
            WorkAttemptStateV1::Leased,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            requested_route,
            None,
            None,
        )
        .map_err(contract_problem)?;
        let record = WorkSynthesisAdmissionRecordV1 {
            request_digest: request_digest.clone(),
            result: build_result(attempt.clone()),
        };
        let draft = accepted_attempt_draft(
            &prepared.product,
            revisions,
            command_id(&prepared.identity)?,
            request_digest,
            attempt.projection_binding().graph_version(),
            &prepared.identity,
            prepared.product.context.observed_at(),
        )?;
        let admission = WorkProductSynthesisAdmissionV1 {
            admission: WorkProductAttemptAdmissionV1 {
                product_context: prepared.product.context,
                product_draft: draft,
                authority: prepared.authority,
                attempt,
                concurrency: topology.concurrency.clone(),
            },
            synthesis: record.clone(),
        };
        match self
            .storage
            .admit_synthesis(&admission)
            .map_err(product_admission_problem)?
            .1
        {
            WorkSynthesisInsertOutcome::Inserted => Ok(record.result),
            WorkSynthesisInsertOutcome::Replayed(result) => Ok(*result),
        }
    }

    fn prepare(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        command: &StartWorkAttemptCommand,
    ) -> Result<PreparedSynthesisV1, ApplicationProblem> {
        let product = current_work_product_attempt_graph(
            &self.storage,
            context,
            binding,
            command.occurred_at,
        )?;
        let item = product
            .graph
            .item(&command.task_id)
            .ok_or_else(not_found_problem)?;
        if !item.is_execution_admitted() {
            return Err(denied_problem(
                "application.work-attempt.execution-not-admitted",
                "Work execution has not been admitted for this task.",
            ));
        }
        let proposal = item.accepted_proposal().cloned().ok_or_else(|| {
            denied_problem(
                "application.work-attempt.no-accepted-proposal",
                "Work has no accepted proposal to execute.",
            )
        })?;
        let authority = crate::work::work_authority(context)?;
        let identity = WorkAttemptIdentityV1::new(
            command.task_id.clone(),
            command.run_id.clone(),
            command.attempt_id.clone(),
        )
        .map_err(contract_problem)?;
        let binding = product_attempt_projection_binding(&product, proposal)?;
        Ok(PreparedSynthesisV1 {
            product,
            authority,
            identity,
            binding,
        })
    }
}

fn command_id(identity: &WorkAttemptIdentityV1) -> Result<WorkCommandId, ApplicationProblem> {
    let digest = canonical_sha256(&(COMMAND_DOMAIN, identity)).map_err(|_| identity_conflict())?;
    WorkCommandId::new(format!(
        "work-product-synthesis:{}",
        digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|_| identity_conflict())
}

fn mint_lease<S>(
    storage: &S,
    authority: &WorkAuthority,
    identity: &WorkAttemptIdentityV1,
) -> Result<WorkLeaseFenceV1, ApplicationProblem>
where
    S: WorkAttemptStoragePort,
{
    let digest = canonical_sha256(&(LEASE_DOMAIN, identity)).map_err(|_| identity_conflict())?;
    let lease_id = WorkLeaseId::new(format!(
        "work-product-synthesis-lease:{}",
        digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|_| identity_conflict())?;
    let epoch = storage
        .next_fence_epoch(authority)
        .map_err(storage_problem)?;
    WorkLeaseFenceV1::new(
        lease_id,
        WorkFenceEpochV1::new(epoch).map_err(contract_problem)?,
    )
    .map_err(contract_problem)
}

fn identity_conflict() -> ApplicationProblem {
    conflict_problem(
        "application.work-attempt.identity-conflict",
        "The Work attempt identity was already used with different content.",
    )
}
