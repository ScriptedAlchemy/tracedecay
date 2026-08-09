//! Canonical product-graph preparation for public Work attempt admission.

use std::collections::BTreeSet;

use tracedecay_domain::{
    ManifestDigest, UtcMicros, WorkAttemptIdentityV1, WorkAttemptProjectionBindingV1,
    WorkAttemptStateV1, WorkAttemptV1, WorkAuthority, WorkCancellationStateV1, WorkCommandId,
    WorkExecutionEnvelopeV1, WorkFenceEpochV1, WorkGraphChangeV1, WorkLeaseFenceV1, WorkLeaseId,
    WorkProductEventPayloadV1, WorkProductProfileScopeV1, WorkProviderRouteV1, WorkRecoveryStateV1,
    canonical_sha256,
};

use crate::{
    ApplicationProblem, RequestAdmission, RequestContext, WorkGraphReadPortV1,
    WorkGraphReadRequestV1, WorkGraphReadV1, WorkProductApplicationErrorV1,
    WorkProductAttemptAdmissionErrorV1, WorkProductAttemptAdmissionOutcomeV1,
    WorkProductAttemptAdmissionPortV1, WorkProductAttemptAdmissionV1, WorkProductBindingV1,
    WorkProductEventDraftV1, WorkProductOwnerAuthorizationErrorV1,
    WorkProductOwnerAuthorizationPortV1, WorkProductPortContextV1, WorkProductRevisionPinsV1,
    WorkProductSelectionScopeV1, WorkRelationScopeV1,
};

use super::{
    StartWorkAttemptCommand, WorkAttemptAdmissionKind, WorkAttemptStorageError,
    WorkAttemptStoragePort, conflict_problem, contract_problem, denied_problem, not_found_problem,
    storage_problem,
};

const WORK_PRODUCT_START_INPUT_DIGEST_DOMAIN: &str =
    "tracedecay.application.work-product-start-attempt.final-v2";
const WORK_PRODUCT_START_COMMAND_DOMAIN: &str =
    "tracedecay.application.work-product-start-attempt-command.final-v2";
const WORK_PRODUCT_START_LEASE_DOMAIN: &str =
    "tracedecay.application.work-product-start-attempt-lease.final-v2";

/// The exact product graph head and authorized product context a public
/// attempt admission is bound to. This is assembled before the combined port
/// starts its transaction; the port rechecks the graph version as its CAS.
pub(crate) struct CurrentWorkProductAttemptGraphV1 {
    pub(crate) context: WorkProductPortContextV1,
    pub(crate) verified: crate::VerifiedWorkGraphVersionV1,
    pub(crate) graph: tracedecay_domain::WorkProductGraphV1,
}

/// Reads the current verified product graph under the exact relation scope
/// resolved for this request. The caller cannot select a different profile or
/// repository relation for attempt admission.
pub(crate) fn current_work_product_attempt_graph<S>(
    storage: &S,
    context: &RequestContext,
    binding: &WorkProductBindingV1,
    observed_at: UtcMicros,
) -> Result<CurrentWorkProductAttemptGraphV1, ApplicationProblem>
where
    S: WorkGraphReadPortV1 + WorkProductOwnerAuthorizationPortV1,
{
    if !context.allows(binding.capability_id(), binding.use_case_id()) {
        return Err(not_found_problem());
    }
    match context.admission_at(observed_at) {
        RequestAdmission::Admitted => {}
        RequestAdmission::Cancelled => return Err(ApplicationProblem::cancelled_before_admission()),
        RequestAdmission::TimedOut => return Err(ApplicationProblem::timed_out_before_admission()),
    }
    let selection =
        WorkProductSelectionScopeV1::relations(BTreeSet::from([WorkRelationScopeV1::Repository {
            project_id: context.scope().project_id.clone(),
            repository_id: context.scope().repository_id.clone(),
        }]))
        .map_err(|_| invalid_start_problem())?;
    let authorized_scope = storage
        .authorize_scope(context, &selection, observed_at)
        .map_err(owner_problem)?;
    if authorized_scope.selection() != &selection {
        return Err(ApplicationProblem::unavailable(crate::SafeDiagnostic {
            code: "application.work-attempt.product-scope-unavailable".to_owned(),
            message: "The canonical Work product scope is unavailable.".to_owned(),
        }));
    }
    let product_context =
        WorkProductPortContextV1::from_request(context, authorized_scope, observed_at);
    let request = WorkGraphReadRequestV1::current(selection, observed_at);
    let read = storage
        .read_graph(&product_context, &request)
        .map_err(|error| product_problem(WorkProductApplicationErrorV1::from(error)))?;
    crate::work_product::validate_result(&request, product_context.authorized_scope(), &read)
        .map_err(product_problem)?;
    let WorkGraphReadV1::Current { snapshot, .. } = read else {
        return Err(ApplicationProblem::unavailable(crate::SafeDiagnostic {
            code: "application.work-attempt.product-read-unavailable".to_owned(),
            message: "The canonical Work product graph is unavailable.".to_owned(),
        }));
    };
    Ok(CurrentWorkProductAttemptGraphV1 {
        context: product_context,
        verified: snapshot.verified_version().clone(),
        graph: snapshot.graph().clone(),
    })
}

pub(crate) fn accepted_attempt_draft(
    product: &CurrentWorkProductAttemptGraphV1,
    revisions: &WorkProductRevisionPinsV1,
    command_id: WorkCommandId,
    canonical_input_digest: ManifestDigest,
    expected_graph_version: tracedecay_domain::WorkGraphVersionV1,
    identity: &WorkAttemptIdentityV1,
    occurred_at: UtcMicros,
) -> Result<WorkProductEventDraftV1, ApplicationProblem> {
    let result_graph_version = expected_graph_version
        .next()
        .map_err(|_| invalid_start_problem())?;
    let authorized_relation_scopes = product
        .context
        .authorized_scope()
        .selection()
        .relation_scopes()
        .map_or_else(Vec::new, |relations| relations.iter().cloned().collect());
    Ok(WorkProductEventDraftV1 {
        actor_id: product.context.actor().clone(),
        owner_scope: WorkProductProfileScopeV1 {
            brain_id: product.context.authorized_scope().owner_brain_id().clone(),
            profile_id: product
                .context
                .authorized_scope()
                .owner_profile_id()
                .clone(),
        },
        authorized_relation_scopes,
        expected_graph_version: Some(expected_graph_version),
        result_graph_version,
        command_id,
        canonical_input_digest,
        causation_event_id: None,
        evidence: Vec::new(),
        source_watermark: product.verified.source_watermark().clone(),
        occurred_at,
        policy_revision_id: revisions.policy_revision_id.clone(),
        configuration_revision_id: revisions.configuration_revision_id.clone(),
        catalog_generation_id: revisions.catalog_generation_id.clone(),
        payload: WorkProductEventPayloadV1::Changed {
            change: Box::new(WorkGraphChangeV1::AcceptedAttemptLinked {
                task_id: identity.task_id().clone(),
                based_on_version: expected_graph_version,
                identity: identity.clone(),
                linked_at: occurred_at,
            }),
        },
    })
}

pub(crate) fn product_attempt_projection_binding(
    product: &CurrentWorkProductAttemptGraphV1,
    accepted_proposal: tracedecay_domain::ProposalId,
) -> Result<WorkAttemptProjectionBindingV1, ApplicationProblem> {
    WorkAttemptProjectionBindingV1::new(
        product.verified.graph_version(),
        product.verified.event_sequence(),
        product.verified.source_watermark().clone(),
        product.verified.recovered_graph_digest().clone(),
        accepted_proposal,
    )
    .map_err(contract_problem)
}

pub(crate) fn product_admission_problem(
    error: WorkProductAttemptAdmissionErrorV1,
) -> ApplicationProblem {
    match error {
        WorkProductAttemptAdmissionErrorV1::InvalidAdmission => {
            ApplicationProblem::InvalidRequest {
                diagnostic: crate::SafeDiagnostic {
                    code: "application.work-attempt.invalid-product-admission".to_owned(),
                    message: "The Work attempt does not match the canonical product graph."
                        .to_owned(),
                },
                retry: crate::RetryDirective::Never,
                legal_actions: vec![crate::LegalAction::CorrectRequest],
            }
        }
        WorkProductAttemptAdmissionErrorV1::NotFoundOrNotAuthorized => not_found_problem(),
        WorkProductAttemptAdmissionErrorV1::VersionConflict => conflict_problem(
            "application.work-attempt.product-version-conflict",
            "The canonical Work product graph changed before attempt admission.",
        ),
        WorkProductAttemptAdmissionErrorV1::IdentityConflict => conflict_problem(
            "application.work-attempt.identity-conflict",
            "The Work attempt identity was already used with different content.",
        ),
        WorkProductAttemptAdmissionErrorV1::IdempotencyConflict => conflict_problem(
            "application.work-attempt.idempotency-conflict",
            "The Work attempt command identity was already used with different input.",
        ),
        WorkProductAttemptAdmissionErrorV1::CapacityExceeded => ApplicationProblem::Saturated {
            diagnostic: crate::SafeDiagnostic {
                code: "application.work-attempt.capacity-exhausted".to_owned(),
                message: "The registered Work topology has no parallel attempt capacity."
                    .to_owned(),
            },
            retry: crate::RetryDirective::AfterDelay,
            legal_actions: vec![crate::LegalAction::Retry],
        },
        WorkProductAttemptAdmissionErrorV1::Unavailable => {
            ApplicationProblem::unavailable(crate::SafeDiagnostic {
                code: "application.work-attempt.product-admission-unavailable".to_owned(),
                message: "The canonical Work product attempt authority is unavailable.".to_owned(),
            })
        }
        WorkProductAttemptAdmissionErrorV1::Cancelled => {
            ApplicationProblem::cancelled_before_admission()
        }
        WorkProductAttemptAdmissionErrorV1::TimedOut => {
            ApplicationProblem::timed_out_before_admission()
        }
        WorkProductAttemptAdmissionErrorV1::DurabilityUncertain => {
            ApplicationProblem::unavailable(crate::SafeDiagnostic {
                code: "application.work-attempt.product-durability-uncertain".to_owned(),
                message: "The Work product attempt commit outcome is uncertain.".to_owned(),
            })
        }
    }
}

/// Public initial-attempt service. It prepares against one verified canonical
/// product graph and delegates the graph link plus attempt row to the combined
/// port, which commits both or neither.
pub struct WorkProductAttemptServiceV1<S> {
    storage: S,
}

impl<S> WorkProductAttemptServiceV1<S>
where
    S: WorkAttemptStoragePort
        + WorkGraphReadPortV1
        + WorkProductOwnerAuthorizationPortV1
        + WorkProductAttemptAdmissionPortV1,
{
    pub const fn new(storage: S) -> Self {
        Self { storage }
    }

    pub fn start_against_registered_topology(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        revisions: &WorkProductRevisionPinsV1,
        topology: &tracedecay_domain::WorkTopologyPolicyV1,
        command: StartWorkAttemptCommand,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
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
        let accepted_proposal = item.accepted_proposal().cloned().ok_or_else(|| {
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
        let binding = product_attempt_projection_binding(&product, accepted_proposal)?;
        let requested_route = command_requested_route(&command);
        let digest = canonical_sha256(&(WORK_PRODUCT_START_INPUT_DIGEST_DOMAIN, &command))
            .map_err(|_| invalid_start_problem())?;
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
        let attempt = match self.storage.load(&authority, &identity) {
            Ok(existing) => {
                let admission_kind = self
                    .storage
                    .load_admission_kind(&authority, &identity)
                    .map_err(storage_problem)?;
                if admission_kind == WorkAttemptAdmissionKind::Synthesis
                    || !existing.execution().same_admission_content(&envelope)
                    || existing.projection_binding().accepted_proposal()
                        != binding.accepted_proposal()
                {
                    return Err(conflict_problem(
                        "application.work-attempt.identity-conflict",
                        "The Work attempt identity was already used with different content.",
                    ));
                }
                existing
            }
            Err(WorkAttemptStorageError::NotFoundOrNotAuthorized) => {
                let lease = mint_product_lease(&self.storage, &authority, &identity)?;
                WorkAttemptV1::new(
                    identity.clone(),
                    binding,
                    envelope,
                    lease,
                    WorkAttemptStateV1::Leased,
                    None,
                    Vec::new(),
                    WorkCancellationStateV1::None,
                    WorkRecoveryStateV1::Fresh,
                    requested_route,
                    None,
                    None,
                )
                .map_err(contract_problem)?
            }
            Err(error) => return Err(storage_problem(error)),
        };
        let command_id = start_command_id(&identity)?;
        let draft = accepted_attempt_draft(
            &product,
            revisions,
            command_id,
            digest,
            admission_binding_graph_version(&attempt),
            &identity,
            product.context.observed_at(),
        )?;
        let admission = WorkProductAttemptAdmissionV1 {
            product_context: product.context,
            product_draft: draft,
            authority,
            attempt,
            concurrency: topology.concurrency.clone(),
        };
        match self
            .storage
            .admit_attempt(&admission)
            .map_err(product_admission_problem)?
        {
            WorkProductAttemptAdmissionOutcomeV1::Inserted { attempt, .. }
            | WorkProductAttemptAdmissionOutcomeV1::Replayed { attempt, .. } => Ok(attempt),
        }
    }
}

fn admission_binding_graph_version(
    attempt: &WorkAttemptV1,
) -> tracedecay_domain::WorkGraphVersionV1 {
    attempt.projection_binding().graph_version()
}

fn command_requested_route(command: &StartWorkAttemptCommand) -> WorkProviderRouteV1 {
    command.execution_snapshot.route().clone()
}

fn mint_product_lease<S>(
    storage: &S,
    authority: &WorkAuthority,
    identity: &WorkAttemptIdentityV1,
) -> Result<WorkLeaseFenceV1, ApplicationProblem>
where
    S: WorkAttemptStoragePort,
{
    let digest = canonical_sha256(&(WORK_PRODUCT_START_LEASE_DOMAIN, identity))
        .map_err(|_| invalid_start_problem())?;
    let lease_id = WorkLeaseId::new(format!(
        "work-product-lease:{}",
        digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|_| invalid_start_problem())?;
    let epoch = storage
        .next_fence_epoch(authority)
        .map_err(storage_problem)?;
    let epoch = WorkFenceEpochV1::new(epoch).map_err(contract_problem)?;
    WorkLeaseFenceV1::new(lease_id, epoch).map_err(contract_problem)
}

fn start_command_id(identity: &WorkAttemptIdentityV1) -> Result<WorkCommandId, ApplicationProblem> {
    let digest = canonical_sha256(&(WORK_PRODUCT_START_COMMAND_DOMAIN, identity))
        .map_err(|_| invalid_start_problem())?;
    WorkCommandId::new(format!(
        "work-product-attempt:{}",
        digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|_| invalid_start_problem())
}

fn owner_problem(error: WorkProductOwnerAuthorizationErrorV1) -> ApplicationProblem {
    match error {
        WorkProductOwnerAuthorizationErrorV1::NotAuthorized => not_found_problem(),
        WorkProductOwnerAuthorizationErrorV1::Unavailable => {
            ApplicationProblem::unavailable(crate::SafeDiagnostic {
                code: "application.work-attempt.product-owner-unavailable".to_owned(),
                message: "The canonical Work product owner authority is unavailable.".to_owned(),
            })
        }
    }
}

fn product_problem(error: WorkProductApplicationErrorV1) -> ApplicationProblem {
    match error {
        WorkProductApplicationErrorV1::NotAuthorized
        | WorkProductApplicationErrorV1::NotFoundOrNotAuthorized => not_found_problem(),
        WorkProductApplicationErrorV1::Cancelled => {
            ApplicationProblem::cancelled_before_admission()
        }
        WorkProductApplicationErrorV1::TimedOut => ApplicationProblem::timed_out_before_admission(),
        WorkProductApplicationErrorV1::VersionConflict
        | WorkProductApplicationErrorV1::RevisionConflict => conflict_problem(
            "application.work-attempt.product-version-conflict",
            "The canonical Work product graph changed before attempt admission.",
        ),
        WorkProductApplicationErrorV1::IdempotencyConflict => conflict_problem(
            "application.work-attempt.product-idempotency-conflict",
            "The canonical Work product admission identity conflicts.",
        ),
        WorkProductApplicationErrorV1::InvalidRequest => invalid_start_problem(),
        WorkProductApplicationErrorV1::EventAuthorityUnavailable
        | WorkProductApplicationErrorV1::GraphAuthorityUnavailable
        | WorkProductApplicationErrorV1::EvidenceAuthorityUnavailable
        | WorkProductApplicationErrorV1::ProposalAuthorityUnavailable => {
            ApplicationProblem::unavailable(crate::SafeDiagnostic {
                code: "application.work-attempt.product-graph-unavailable".to_owned(),
                message: "The canonical Work product graph authority is unavailable.".to_owned(),
            })
        }
    }
}

fn invalid_start_problem() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: crate::SafeDiagnostic {
            code: "application.work-attempt.invalid-product-admission".to_owned(),
            message: "The Work attempt command is invalid.".to_owned(),
        },
        retry: crate::RetryDirective::Never,
        legal_actions: vec![crate::LegalAction::CorrectRequest],
    }
}
