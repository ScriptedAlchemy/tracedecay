//! Work/workflow application daemon invocation handlers.

use super::*;

pub(super) fn application_problem(
    request_id: String,
    problem: ApplicationProblem,
) -> DaemonInvocationResponse {
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::ApplicationProblem { problem },
    )
}

pub(super) fn concealed_application_problem(request_id: String) -> DaemonInvocationResponse {
    application_problem(
        request_id,
        ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_workflow_application(
    registered: RegisteredWorkRuntime,
    project_root: &Path,
    request_id: String,
    request: WorkflowApplicationInvocation,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let operation_key = request.operation_key();
    let Some((_, capability, use_case)) =
        tracedecay_application::WORKFLOW_APPLICATION_OPERATION_IDS
            .iter()
            .find(|(operation, _, _)| *operation == operation_key)
    else {
        return DaemonInvocationResponse::problem(
            request_id,
            DaemonInvocationProblem::InvalidRequest,
        );
    };
    let (context, canonical_request_id, use_case) = match work_request_context(
        &registered,
        &request_id,
        capability,
        use_case,
        observed_at,
        deadline.clone(),
        cancellation,
    ) {
        Ok(context) => context,
        Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
    };
    let input_digest = match canonical_sha256(&request) {
        Ok(digest) => digest,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::InvalidRequest,
            );
        }
    };
    let services = match registered.database.workflow_application_services() {
        Ok(services) => services,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };

    match request {
        WorkflowApplicationInvocation::RegisterDefinition(request) => complete_workflow_effect(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services
                .definitions()
                .register(request.definition)
                .map_err(workflow_coordination_problem),
            observed_at,
            deadline,
            WorkflowApplicationOutcome::RegisterDefinition,
        ),
        WorkflowApplicationInvocation::ValidateDefinition(request) => complete_workflow_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services
                .definitions()
                .validate(request.definition)
                .map_err(workflow_coordination_problem),
            observed_at,
            deadline,
            WorkflowApplicationOutcome::ValidateDefinition,
        ),
        WorkflowApplicationInvocation::GetDefinition(request) => complete_workflow_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services
                .definitions()
                .get(&request.definition_id, request.definition_version)
                .map_err(workflow_coordination_problem),
            observed_at,
            deadline,
            WorkflowApplicationOutcome::GetDefinition,
        ),
        WorkflowApplicationInvocation::ListDefinitions(_) => complete_workflow_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services
                .definitions()
                .list()
                .map_err(workflow_coordination_problem),
            observed_at,
            deadline,
            WorkflowApplicationOutcome::ListDefinitions,
        ),
        WorkflowApplicationInvocation::DefinitionHistory(request) => complete_workflow_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services
                .definitions()
                .history(&request.definition_id)
                .map_err(workflow_coordination_problem),
            observed_at,
            deadline,
            WorkflowApplicationOutcome::DefinitionHistory,
        ),
        WorkflowApplicationInvocation::DiffDefinition(request) => complete_workflow_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services
                .definitions()
                .diff(
                    &request.definition_id,
                    request.from_version,
                    request.to_version,
                )
                .map_err(workflow_coordination_problem),
            observed_at,
            deadline,
            WorkflowApplicationOutcome::DiffDefinition,
        ),
        WorkflowApplicationInvocation::ActivateDefinition(request) => complete_workflow_effect(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services
                .definitions()
                .activate(
                    &request.definition_id,
                    request.expected_active_version,
                    request.replacement_version,
                )
                .map_err(workflow_coordination_problem),
            observed_at,
            deadline,
            WorkflowApplicationOutcome::ActivateDefinition,
        ),
        WorkflowApplicationInvocation::RetireDefinition(request) => complete_workflow_effect(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services
                .definitions()
                .retire(
                    &request.definition_id,
                    Some(request.expected_active_version),
                )
                .map_err(workflow_coordination_problem),
            observed_at,
            deadline,
            WorkflowApplicationOutcome::RetireDefinition,
        ),
        WorkflowApplicationInvocation::HandoffIssue(request) => {
            let result = TaskHandoffToken::new(request.secret)
                .map_err(task_handoff_problem)
                .and_then(|token| {
                    services
                        .handoffs()
                        .issue(
                            &request.issuer,
                            request.scope,
                            &token,
                            request.expires_at,
                            request.issued_at,
                        )
                        .map_err(task_handoff_problem)
                });
            complete_workflow_effect(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                result,
                observed_at,
                deadline,
                WorkflowApplicationOutcome::HandoffIssue,
            )
        }
        WorkflowApplicationInvocation::HandoffRedeem(request) => {
            let scope = request.expected_scope;
            let result = TaskHandoffToken::new(request.secret)
                .map_err(task_handoff_problem)
                .and_then(|token| {
                    services
                        .handoffs()
                        .redeem(&token, &scope, &request.redeemer, request.consumed_at)
                        .map_err(task_handoff_problem)
                })
                .map(|()| TaskHandoffRedeemed { scope });
            complete_workflow_effect(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                result,
                observed_at,
                deadline,
                WorkflowApplicationOutcome::HandoffRedeem,
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn complete_workflow_read<T>(
    registered: &RegisteredWorkRuntime,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: Result<T, DaemonInvocationProblem>,
    observed_at: UtcMicros,
    deadline: Deadline,
    wrap: fn(ApplicationOutcome<T>) -> WorkflowApplicationOutcome,
) -> DaemonInvocationResponse
where
    T: Serialize,
{
    let result = match result {
        Ok(result) => result,
        Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
    };
    let outcome = match work_evidence_packet(
        registered,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        result,
        observed_at,
        deadline,
    ) {
        Ok(evidence) => wrap(ApplicationOutcome::Evidence(evidence)),
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::WorkflowApplication {
            scope: context.scope().clone(),
            outcome,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn complete_workflow_effect<T>(
    registered: &RegisteredWorkRuntime,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: Result<T, DaemonInvocationProblem>,
    observed_at: UtcMicros,
    deadline: Deadline,
    wrap: fn(ApplicationOutcome<T>) -> WorkflowApplicationOutcome,
) -> DaemonInvocationResponse
where
    T: Serialize,
{
    let result = match result {
        Ok(result) => result,
        Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
    };
    let outcome = match work_effect(
        registered,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        result,
        observed_at,
        deadline,
    ) {
        Ok(outcome) => wrap(outcome),
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::WorkflowApplication {
            scope: context.scope().clone(),
            outcome,
        },
    )
}

fn workflow_coordination_problem(error: WorkflowCoordinationError) -> DaemonInvocationProblem {
    match error {
        WorkflowCoordinationError::AuthorityUnavailable(_) => DaemonInvocationProblem::Unavailable,
        WorkflowCoordinationError::DefinitionNotFound => {
            DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
        WorkflowCoordinationError::InvalidDefinition
        | WorkflowCoordinationError::ImmutableDefinitionConflict
        | WorkflowCoordinationError::UnsupportedOperation
        | WorkflowCoordinationError::CatalogDigestMismatch
        | WorkflowCoordinationError::StaleActivation => DaemonInvocationProblem::InvalidRequest,
    }
}

fn task_handoff_problem(error: TaskHandoffError) -> DaemonInvocationProblem {
    match error {
        TaskHandoffError::AuthorityUnavailable(_) => DaemonInvocationProblem::Unavailable,
        TaskHandoffError::Missing | TaskHandoffError::ScopeMismatch => {
            DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
        TaskHandoffError::InvalidToken
        | TaskHandoffError::InvalidScope
        | TaskHandoffError::Unauthorized
        | TaskHandoffError::InvalidExpiry
        | TaskHandoffError::Conflict
        | TaskHandoffError::Expired
        | TaskHandoffError::Replay => DaemonInvocationProblem::InvalidRequest,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn work_request_context(
    registered: &RegisteredWorkRuntime,
    request_id: &str,
    capability: &str,
    use_case: &str,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<(RequestContext, RequestId, UseCaseId), DaemonInvocationProblem> {
    let capability =
        CapabilityId::new(capability).map_err(|_| DaemonInvocationProblem::Unavailable)?;
    let use_case = UseCaseId::new(use_case).map_err(|_| DaemonInvocationProblem::Unavailable)?;
    let canonical_request_id =
        RequestId::new(request_id).map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
    let context = RequestContext::new(
        registered.actor.clone(),
        registered.grant.scope.clone(),
        registered.grant.clone(),
        canonical_request_id.clone(),
        deadline,
        cancellation,
    )
    .map_err(|_| DaemonInvocationProblem::NotFoundOrNotAuthorized)?;
    if context.admission_at(observed_at) != RequestAdmission::Admitted
        || !context.allows(&capability, &use_case)
    {
        return Err(DaemonInvocationProblem::NotFoundOrNotAuthorized);
    }
    Ok((context, canonical_request_id, use_case))
}

#[allow(clippy::too_many_arguments)]
fn work_evidence_packet<T>(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    _request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: T,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<EvidencePacket<T>, ApplicationContractError>
where
    T: Serialize,
{
    let policy_digest = canonical_sha256(&(
        "tracedecay.daemon.work-read-policy.v1",
        &registered.policy_digest,
        &registered.grant.digest,
        operation_key,
        &use_case,
    ))?;
    let policy = PolicyDecisionRef::new(
        format!("policy.daemon.work.{operation_key}.v1"),
        1,
        policy_digest,
        ComponentVersion::new("tracedecay.daemon.work-policy.v1").map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "Work read policy evaluator",
            }
        })?,
    )?;
    let authority = AuthorityReceipt::from_context(context, policy, observed_at)?;
    let suffix = input_digest.as_str().strip_prefix("sha256:").ok_or(
        ApplicationContractError::Inconsistent {
            field: "Work read input digest",
        },
    )?;
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )?;
    Ok(EvidencePacket {
        temporal: TemporalState::current(execution.ended_at),
        authority,
        evidence_authorities: vec![EvidenceAuthority {
            evidence_id: EvidenceIdentity::new(format!("evidence.work.{operation_key}.{suffix}"))?,
            source_kind: "work_projection".to_owned(),
            producer: operation_key.to_owned(),
            scope: context.scope().clone(),
            revision: ComponentVersion::new("tracedecay.work-projection.v1")?,
            horizon: Some(execution.ended_at),
        }],
        coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Operational], 1, 1, 1)?,
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(
            SortContractId::new(format!("sort.work.{operation_key}.v1"))?,
            1,
            Some(1),
            1,
        )?,
        execution,
        payload: Some(result),
    })
}

#[allow(clippy::too_many_arguments)]
fn work_effect<T>(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: T,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<ApplicationOutcome<T>, ApplicationContractError>
where
    T: Serialize,
{
    let policy_digest = canonical_sha256(&(
        "tracedecay.daemon.work-policy.v1",
        &registered.policy_digest,
        &registered.grant.digest,
        operation_key,
        &use_case,
    ))?;
    let policy = PolicyDecisionRef::new(
        format!("policy.daemon.work.{operation_key}.v1"),
        1,
        policy_digest,
        ComponentVersion::new("tracedecay.daemon.work-policy.v1").map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "Work policy evaluator",
            }
        })?,
    )?;
    let authority = AuthorityReceipt::from_context(context, policy, observed_at)?;
    let suffix = input_digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(ApplicationContractError::Inconsistent {
            field: "Work input digest",
        })?
        .to_owned();
    let idempotency_key = IdempotencyKey::new(format!("work.{operation_key}.{suffix}"))?;
    let expected_state = canonical_sha256(&(
        "tracedecay.work.expected-state.v1",
        operation_key,
        &input_digest,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "Work expected state",
    })?;
    let committed_state =
        canonical_sha256(&("tracedecay.work.committed-state.v1", operation_key, &result)).map_err(
            |_| ApplicationContractError::Inconsistent {
                field: "Work committed state",
            },
        )?;
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )?;
    let receipt = EffectReceipt {
        operation: use_case,
        request_id,
        actor: registered.actor.clone(),
        scope: context.scope().clone(),
        effect_class: EffectClass::Administrative,
        idempotency_key: idempotency_key.clone(),
        input_digest,
        expected_state: expected_state.clone(),
        policy_digest: authority.policy.digest.clone(),
        configuration_digest: registered.configuration_digest.clone(),
        catalog_digest: canonical_sha256(&("tracedecay.work.catalog.v1", operation_key)).map_err(
            |_| ApplicationContractError::Inconsistent {
                field: "Work catalog digest",
            },
        )?,
        privacy_digest: canonical_sha256(&(
            "tracedecay.work.privacy.v1",
            context.scope(),
            context.grant().disclosure,
        ))
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "Work privacy digest",
        })?,
        outcome: EffectTermination::Completed,
        committed_state: Some(committed_state),
        external_proof: None,
    };
    Ok(ApplicationOutcome::Effect(EffectResult::new(
        EffectId::new(format!("effect.work.{operation_key}.{suffix}"))?,
        EffectClass::Administrative,
        idempotency_key,
        authority,
        expected_state,
        execution,
        ReconciliationState::Reconciled,
        receipt,
        Some(result),
    )?))
}

impl DaemonInvocationService {
    pub(super) async fn work_runtime(
        &self,
        project_root: Option<&Path>,
    ) -> Option<RegisteredWorkRuntime> {
        self.project_runtimes.get(project_root?).await
    }
}
