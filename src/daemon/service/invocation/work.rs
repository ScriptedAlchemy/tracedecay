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
    _observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let observed_at = crate::daemon_client::invocation_now_micros();
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
        WorkflowApplicationInvocation::RegisterDefinition(request) => {
            let prepared =
                match prepare_workflow_definition_registration(&context, request.definition) {
                    Ok(definition) => WorkflowEffectPreparedV1::register_definition(
                        input_digest.clone(),
                        definition,
                    ),
                    Err(error) => WorkflowEffectPreparedV1::problem(
                        input_digest.clone(),
                        workflow_effect_problem(workflow_coordination_problem(error)),
                    ),
                };
            execute_journaled_workflow_effect(
                &registered,
                services.effects(),
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                prepared,
                observed_at,
                deadline,
            )
        }
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
        WorkflowApplicationInvocation::HandoffIssue(request) => {
            let prepared = match TaskHandoffToken::new(request.secret).and_then(|token| {
                prepare_task_handoff_issue(&context, request.scope, &token, observed_at)
            }) {
                Ok(grant) => WorkflowEffectPreparedV1::handoff_issue(input_digest.clone(), grant),
                Err(error) => WorkflowEffectPreparedV1::problem(
                    input_digest.clone(),
                    workflow_effect_problem(task_handoff_problem(error)),
                ),
            };
            execute_journaled_workflow_effect(
                &registered,
                services.effects(),
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                prepared,
                observed_at,
                deadline,
            )
        }
        WorkflowApplicationInvocation::HandoffRedeem(request) => {
            let scope = request.expected_scope;
            let prepared = match TaskHandoffToken::new(request.secret)
                .and_then(|token| prepare_task_handoff_redeem(&context, &token, &scope))
            {
                Ok(token_digest) => WorkflowEffectPreparedV1::handoff_redeem(
                    input_digest.clone(),
                    token_digest,
                    scope,
                    observed_at,
                ),
                Err(error) => WorkflowEffectPreparedV1::problem(
                    input_digest.clone(),
                    workflow_effect_problem(task_handoff_problem(error)),
                ),
            };
            execute_journaled_workflow_effect(
                &registered,
                services.effects(),
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                prepared,
                observed_at,
                deadline,
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
fn execute_journaled_workflow_effect(
    registered: &RegisteredWorkRuntime,
    authority: &impl WorkflowEffectAuthorityPortV1,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    prepared: WorkflowEffectPreparedV1,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> DaemonInvocationResponse {
    let operation = match workflow_effect_operation(operation_key) {
        Some(operation) => operation,
        None => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::InvalidRequest,
            );
        }
    };
    let receipt_context = match workflow_effect_receipt_context(
        registered,
        context,
        operation_key,
        use_case,
        &input_digest,
        observed_at,
    ) {
        Ok(receipt_context) => receipt_context,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let receipt_binding_digest = match receipt_context.binding_digest() {
        Ok(digest) => digest,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let idempotency_digest = match canonical_sha256(&(
        "tracedecay.daemon.workflow-effect-idempotency.v1",
        operation_key,
        &input_digest,
        context.actor(),
        context.scope(),
        receipt_binding_digest,
    )) {
        Ok(digest) => digest,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let suffix = match idempotency_digest.as_str().strip_prefix("sha256:") {
        Some(suffix) => suffix,
        None => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let idempotency_key = match IdempotencyKey::new(format!("workflow.{operation_key}.{suffix}")) {
        Ok(key) => key,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let identity = match WorkflowEffectIdentityV1::new(
        operation,
        idempotency_key,
        canonical_request_id,
        context.actor().clone(),
        context.scope().clone(),
        input_digest,
        observed_at,
        deadline,
        receipt_context,
    ) {
        Ok(identity) => identity,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let prepared = if identity.deadline().is_elapsed_at(current_micros()) {
        WorkflowEffectPreparedV1::problem(
            identity.input_digest().clone(),
            WorkflowEffectProblemV1::TimedOut,
        )
    } else {
        prepared
    };
    let record = match authority.execute_effect(&identity, &prepared, current_micros()) {
        Ok(record) => record,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let Some(terminal) = record.terminal() else {
        return DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable);
    };
    let outcome = match workflow_effect_outcome(terminal) {
        Ok(outcome) => outcome,
        Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
    };
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::WorkflowApplication {
            scope: context.scope().clone(),
            outcome,
        },
    )
}

fn workflow_effect_receipt_context(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: &ManifestDigest,
    observed_at: UtcMicros,
) -> Result<WorkflowEffectReceiptContextV1, ApplicationContractError> {
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
    let suffix = input_digest.as_str().strip_prefix("sha256:").ok_or(
        ApplicationContractError::Inconsistent {
            field: "Work input digest",
        },
    )?;
    let expected_state = canonical_sha256(&(
        "tracedecay.work.expected-state.v1",
        operation_key,
        input_digest,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "Work expected state",
    })?;
    let catalog_digest =
        canonical_sha256(&("tracedecay.work.catalog.v1", operation_key)).map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "Work catalog digest",
            }
        })?;
    let privacy_digest = canonical_sha256(&(
        "tracedecay.work.privacy.v1",
        context.scope(),
        context.grant().disclosure,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "Work privacy digest",
    })?;
    Ok(WorkflowEffectReceiptContextV1::new(
        use_case,
        EffectId::new(format!("effect.work.{operation_key}.{suffix}"))?,
        authority,
        expected_state,
        registered.configuration_digest.clone(),
        catalog_digest,
        privacy_digest,
    ))
}

fn workflow_effect_operation(operation_key: &str) -> Option<WorkflowEffectOperationV1> {
    match operation_key {
        "register_definition" => Some(WorkflowEffectOperationV1::RegisterDefinition),
        "handoff_issue" => Some(WorkflowEffectOperationV1::HandoffIssue),
        "handoff_redeem" => Some(WorkflowEffectOperationV1::HandoffRedeem),
        _ => None,
    }
}

fn workflow_effect_problem(problem: DaemonInvocationProblem) -> WorkflowEffectProblemV1 {
    match problem {
        DaemonInvocationProblem::NotFoundOrNotAuthorized => {
            WorkflowEffectProblemV1::NotFoundOrNotAuthorized
        }
        DaemonInvocationProblem::InvalidRequest
        | DaemonInvocationProblem::UnsupportedRevision
        | DaemonInvocationProblem::ResetRequired => WorkflowEffectProblemV1::InvalidRequest,
        DaemonInvocationProblem::Unavailable => WorkflowEffectProblemV1::Conflict,
    }
}

fn workflow_effect_daemon_problem(problem: WorkflowEffectProblemV1) -> DaemonInvocationProblem {
    match problem {
        WorkflowEffectProblemV1::NotFoundOrNotAuthorized => {
            DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
        WorkflowEffectProblemV1::InvalidRequest
        | WorkflowEffectProblemV1::Conflict
        | WorkflowEffectProblemV1::TimedOut => DaemonInvocationProblem::InvalidRequest,
    }
}

fn workflow_effect_outcome(
    terminal: &WorkflowEffectTerminalV1,
) -> Result<WorkflowApplicationOutcome, DaemonInvocationProblem> {
    match terminal.outcome() {
        WorkflowEffectOutcomeV1::Problem(WorkflowEffectProblemV1::TimedOut) => {
            let termination = EffectTermination::TimedOut;
            match terminal.identity().operation() {
                WorkflowEffectOperationV1::RegisterDefinition => work_effect::<
                    tracedecay_domain::WorkflowDefinition,
                >(
                    terminal, None, termination
                )
                .map(WorkflowApplicationOutcome::RegisterDefinition)
                .map_err(|_| DaemonInvocationProblem::Unavailable),
                WorkflowEffectOperationV1::HandoffIssue => {
                    work_effect::<TaskHandoffGrant>(terminal, None, termination)
                        .map(WorkflowApplicationOutcome::HandoffIssue)
                        .map_err(|_| DaemonInvocationProblem::Unavailable)
                }
                WorkflowEffectOperationV1::HandoffRedeem => {
                    work_effect::<TaskHandoffRedeemed>(terminal, None, termination)
                        .map(WorkflowApplicationOutcome::HandoffRedeem)
                        .map_err(|_| DaemonInvocationProblem::Unavailable)
                }
            }
        }
        WorkflowEffectOutcomeV1::Problem(problem) => Err(workflow_effect_daemon_problem(*problem)),
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::DefinitionRegistered(result)) => {
            work_effect(terminal, Some(result.clone()), EffectTermination::Completed)
                .map(WorkflowApplicationOutcome::RegisterDefinition)
                .map_err(|_| DaemonInvocationProblem::Unavailable)
        }
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::HandoffIssued(result)) => {
            work_effect(terminal, Some(result.clone()), EffectTermination::Completed)
                .map(WorkflowApplicationOutcome::HandoffIssue)
                .map_err(|_| DaemonInvocationProblem::Unavailable)
        }
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::HandoffRedeemed(result)) => {
            work_effect(terminal, Some(result.clone()), EffectTermination::Completed)
                .map(WorkflowApplicationOutcome::HandoffRedeem)
                .map_err(|_| DaemonInvocationProblem::Unavailable)
        }
    }
}

fn workflow_coordination_problem(error: WorkflowCoordinationError) -> DaemonInvocationProblem {
    match error {
        WorkflowCoordinationError::AuthorityUnavailable(_) => DaemonInvocationProblem::Unavailable,
        WorkflowCoordinationError::DefinitionNotFound => {
            DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
        WorkflowCoordinationError::ScopeMismatch => {
            DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
        WorkflowCoordinationError::InvalidDefinition
        | WorkflowCoordinationError::ImmutableDefinitionConflict => {
            DaemonInvocationProblem::InvalidRequest
        }
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
    terminal: &WorkflowEffectTerminalV1,
    result: Option<T>,
    termination: EffectTermination,
) -> Result<ApplicationOutcome<T>, ApplicationContractError>
where
    T: Serialize,
{
    let identity = terminal.identity();
    let receipt_context = identity.receipt_context();
    let committed_state = result
        .as_ref()
        .map(|result| {
            canonical_sha256(&(
                "tracedecay.work.committed-state.v1",
                identity.operation().as_str(),
                result,
            ))
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "Work committed state",
            })
        })
        .transpose()?;
    let execution = OperationReceipt {
        started_at: identity.started_at(),
        ended_at: terminal.ended_at(),
        effective_deadline: identity.deadline().clone(),
        cancellation: workflow_effect_terminal_observation(termination, terminal.ended_at()),
        budget: OperationBudgetUsage::default(),
        termination: termination.into(),
    };
    execution.validate()?;
    let receipt = EffectReceipt {
        operation: receipt_context.operation().clone(),
        request_id: identity.request_id().clone(),
        actor: identity.actor().clone(),
        scope: identity.scope().clone(),
        effect_class: EffectClass::Administrative,
        idempotency_key: identity.idempotency_key().clone(),
        input_digest: identity.input_digest().clone(),
        expected_state: receipt_context.expected_state().clone(),
        policy_digest: receipt_context.authority().policy.digest.clone(),
        configuration_digest: receipt_context.configuration_digest().clone(),
        catalog_digest: receipt_context.catalog_digest().clone(),
        privacy_digest: receipt_context.privacy_digest().clone(),
        outcome: termination,
        committed_state,
        external_proof: None,
    };
    Ok(ApplicationOutcome::Effect(EffectResult::new(
        receipt_context.effect_id().clone(),
        EffectClass::Administrative,
        identity.idempotency_key().clone(),
        receipt_context.authority().clone(),
        receipt_context.expected_state().clone(),
        execution,
        ReconciliationState::Reconciled,
        receipt,
        result,
    )?))
}

fn workflow_effect_terminal_observation(
    termination: EffectTermination,
    observed_at: UtcMicros,
) -> Option<CancellationObservation> {
    (termination == EffectTermination::TimedOut).then_some(CancellationObservation {
        stage: CancellationStage::BeforeEffect,
        observed_at,
    })
}

impl DaemonInvocationService {
    pub(super) async fn work_runtime(
        &self,
        project_root: Option<&Path>,
    ) -> Option<RegisteredWorkRuntime> {
        self.project_runtimes.get(project_root?).await
    }
}

#[cfg(test)]
mod workflow_effect_receipt_tests {
    use super::*;

    #[test]
    fn deadline_before_mutation_is_not_labeled_in_flight() {
        let observed_at = UtcMicros(42);
        assert_eq!(
            workflow_effect_terminal_observation(EffectTermination::TimedOut, observed_at),
            Some(CancellationObservation {
                stage: CancellationStage::BeforeEffect,
                observed_at,
            })
        );
        assert_eq!(
            workflow_effect_terminal_observation(EffectTermination::Completed, observed_at),
            None
        );
    }
}
