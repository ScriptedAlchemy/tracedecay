//! Configuration daemon invocation handlers: mutation, evidence, preview, and semantic-profile transitions.

use super::*;

mod settlement;

use settlement::{configuration_effect, reconcile_configuration_runtime};

pub(super) async fn execute_configuration(
    wire_request_id: String,
    registered: Option<RegisteredConfigurationRuntime>,
    surface_operation: crate::application_surface::ApplicationSurfaceOperation,
    request: ConfigurationWireRequestV1,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(registered) = registered else {
        return runtime_mounting_problem(wire_request_id);
    };
    if cancellation.is_cancelled() {
        return application_problem(
            wire_request_id,
            ApplicationProblem::cancelled_before_admission(),
        );
    }
    if deadline.is_elapsed_at(observed_at) || deadline.is_elapsed_at(current_micros()) {
        return application_problem(
            wire_request_id,
            ApplicationProblem::timed_out_before_admission(),
        );
    }
    let authority = match configuration_request_authority(
        &registered,
        &wire_request_id,
        surface_operation,
        observed_at,
        deadline.clone(),
        cancellation,
    ) {
        Ok(authority) => authority,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let actor = AuthorizedActor {
        actor_id: registered.actor.clone(),
    };
    let client = registered.runtime.client();
    let result: Result<ApplicationOutcome<serde_json::Value>, ConfigurationError> = async {
        match (surface_operation, request) {
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationList,
                ConfigurationWireRequestV1::List(_),
            ) => configuration_evidence(
                serde_json::to_value(client.list(actor).await?)
                    .map_err(|_| ConfigurationError::Unavailable)?,
                authority,
                observed_at,
                deadline,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationExplain,
                ConfigurationWireRequestV1::Explain(request),
            ) => configuration_evidence(
                serde_json::to_value(client.explain(actor, request.key).await?)
                    .map_err(|_| ConfigurationError::Unavailable)?,
                authority,
                observed_at,
                deadline,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationGet,
                ConfigurationWireRequestV1::Get(request),
            ) => configuration_evidence(
                serde_json::to_value(client.get(actor, request.key).await?)
                    .map_err(|_| ConfigurationError::Unavailable)?,
                authority,
                observed_at,
                deadline,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationObservedState,
                ConfigurationWireRequestV1::ObservedState(_),
            ) => configuration_evidence(
                serde_json::to_value(client.observed_state(actor).await?)
                    .map_err(|_| ConfigurationError::Unavailable)?,
                authority,
                observed_at,
                deadline,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationAudit,
                ConfigurationWireRequestV1::Audit(request),
            ) => configuration_evidence(
                serde_json::to_value(
                    client
                        .audit(
                            actor,
                            ConfigurationAuditQuery {
                                after_event_id: request.after_event_id,
                                limit: request.limit,
                            },
                        )
                        .await?,
                )
                .map_err(|_| ConfigurationError::Unavailable)?,
                authority,
                observed_at,
                deadline,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationSet,
                ConfigurationWireRequestV1::Set(request),
            ) => {
                let idempotency_key = request.idempotency_key;
                let mutation = DirectConfigurationMutation::Set {
                    layer: request.layer,
                    key: request.key,
                    value: Box::new(request.value),
                };
                let mutation_authority = issue_direct_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    idempotency_key.clone(),
                    &mutation,
                    request.expected_revision.clone(),
                    deadline.expires_at,
                    observed_at,
                )?;
                let receipt = apply_configuration_or_semantic_transition(
                    &registered,
                    mutation_authority,
                    mutation,
                    request.expected_revision.clone(),
                    observed_at,
                )
                .await?;
                configuration_effect(
                    serde_json::to_value(&receipt).map_err(|_| ConfigurationError::Unavailable)?,
                    authority,
                    &registered.actor,
                    &registered.scope,
                    surface_operation,
                    &idempotency_key,
                    &request.expected_revision,
                    receipt.operation_digest,
                    receipt.settlement_authority,
                    receipt.created_at,
                    receipt.effective_deadline_at,
                )
            }
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationUnset,
                ConfigurationWireRequestV1::Unset(request),
            ) => {
                let idempotency_key = request.idempotency_key;
                let mutation = DirectConfigurationMutation::Unset {
                    layer: request.layer,
                    key: request.key,
                };
                let mutation_authority = issue_direct_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    idempotency_key.clone(),
                    &mutation,
                    request.expected_revision.clone(),
                    deadline.expires_at,
                    observed_at,
                )?;
                let receipt = apply_configuration_or_semantic_transition(
                    &registered,
                    mutation_authority,
                    mutation,
                    request.expected_revision.clone(),
                    observed_at,
                )
                .await?;
                configuration_effect(
                    serde_json::to_value(&receipt).map_err(|_| ConfigurationError::Unavailable)?,
                    authority,
                    &registered.actor,
                    &registered.scope,
                    surface_operation,
                    &idempotency_key,
                    &request.expected_revision,
                    receipt.operation_digest,
                    receipt.settlement_authority,
                    receipt.created_at,
                    receipt.effective_deadline_at,
                )
            }
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationBatch,
                ConfigurationWireRequestV1::Batch(request),
            ) => {
                let idempotency_key = request.idempotency_key;
                let mutations = request
                    .mutations
                    .into_iter()
                    .map(|mutation| match mutation {
                        tracedecay_application::ConfigurationDirectMutationRequestV1::Set {
                            layer,
                            key,
                            value,
                        } => DirectConfigurationMutation::Set { layer, key, value },
                        tracedecay_application::ConfigurationDirectMutationRequestV1::Unset {
                            layer,
                            key,
                        } => DirectConfigurationMutation::Unset { layer, key },
                    })
                    .collect();
                let mutation = DirectConfigurationMutation::Batch { mutations };
                let mutation_authority = issue_direct_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    idempotency_key.clone(),
                    &mutation,
                    request.expected_revision.clone(),
                    deadline.expires_at,
                    observed_at,
                )?;
                let receipt = apply_configuration_or_semantic_transition(
                    &registered,
                    mutation_authority,
                    mutation,
                    request.expected_revision.clone(),
                    observed_at,
                )
                .await?;
                configuration_effect(
                    serde_json::to_value(&receipt).map_err(|_| ConfigurationError::Unavailable)?,
                    authority,
                    &registered.actor,
                    &registered.scope,
                    surface_operation,
                    &idempotency_key,
                    &request.expected_revision,
                    receipt.operation_digest,
                    receipt.settlement_authority,
                    receipt.created_at,
                    receipt.effective_deadline_at,
                )
            }
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationWriteCredential,
                ConfigurationWireRequestV1::WriteCredential(request),
            ) => {
                let idempotency_key = request.idempotency_key;
                let mutation_authority = issue_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    Some(idempotency_key.clone()),
                    ConfigurationMutationOperationV1::CredentialWrite,
                    registered.scope.scope_digest.clone(),
                    request.expected_revision.clone(),
                    ConfigurationMutationSinkV1::CredentialStore,
                    ConfigurationMutationEffectV1::WriteCredentialReference,
                    deadline.expires_at,
                    observed_at,
                )?;
                let metadata = client
                    .write_credential(
                        mutation_authority,
                        WriteOnlyCredentialMutation {
                            expected_reference_id: request.expected_reference_id,
                            kind: request.kind,
                            write_handle: CredentialWriteHandleV1::new(request.write_handle)?,
                        },
                        request.expected_revision.clone(),
                    )
                    .await?;
                let payload =
                    serde_json::to_value(&metadata).map_err(|_| ConfigurationError::Unavailable)?;
                configuration_effect(
                    payload,
                    authority,
                    &registered.actor,
                    &registered.scope,
                    surface_operation,
                    &idempotency_key,
                    &request.expected_revision,
                    metadata.operation_digest,
                    metadata.settlement_authority,
                    metadata.created_at,
                    metadata.effective_deadline_at,
                )
            }
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationProtectedPreview,
                ConfigurationWireRequestV1::ProtectedPreview(request),
            ) => {
                let mutation_authority = issue_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    None,
                    ConfigurationMutationOperationV1::ProtectedDryRun,
                    registered.scope.scope_digest.clone(),
                    request.expected_revision.clone(),
                    ConfigurationMutationSinkV1::ConfigurationStore,
                    ConfigurationMutationEffectV1::CreateProtectedChangePlan,
                    deadline.expires_at,
                    observed_at,
                )?;
                let plan = client
                    .dry_run_protected_change(
                        mutation_authority,
                        request.change,
                        request.expected_revision.clone(),
                    )
                    .await?;
                configuration_preview(
                    serde_json::to_value(&plan).map_err(|_| ConfigurationError::Unavailable)?,
                    authority,
                    plan.plan_id.as_str(),
                    plan.operation_digest,
                    &request.expected_revision,
                    observed_at,
                    deadline,
                )
            }
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationProtectedApply,
                ConfigurationWireRequestV1::ProtectedApply(request),
            ) => {
                let mutation_authority = issue_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    Some(request.idempotency_key.clone()),
                    ConfigurationMutationOperationV1::ProtectedApply,
                    registered.scope.scope_digest.clone(),
                    request.expected_base_revision_id.clone(),
                    ConfigurationMutationSinkV1::ConfigurationStore,
                    ConfigurationMutationEffectV1::CommitConfigurationRevision,
                    deadline.expires_at,
                    observed_at,
                )?;
                let receipt = client
                    .apply_protected_change(
                        mutation_authority,
                        ProtectedApplyRequest {
                            plan_id: request.plan_id,
                            actor_id: registered.actor.clone(),
                            expected_base_revision_id: request.expected_base_revision_id.clone(),
                            operation_digest: request.operation_digest,
                            idempotency_key: request.idempotency_key.clone(),
                        },
                    )
                    .await?;
                reconcile_configuration_runtime(&registered, &receipt, observed_at).await;
                configuration_effect(
                    serde_json::to_value(&receipt).map_err(|_| ConfigurationError::Unavailable)?,
                    authority,
                    &registered.actor,
                    &registered.scope,
                    surface_operation,
                    &request.idempotency_key,
                    &request.expected_base_revision_id,
                    receipt.operation_digest,
                    receipt.settlement_authority,
                    receipt.created_at,
                    receipt.effective_deadline_at,
                )
            }
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationRollbackPreview,
                ConfigurationWireRequestV1::RollbackPreview(request),
            ) => {
                let current = client.current().await?;
                let mutation_authority = issue_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    None,
                    ConfigurationMutationOperationV1::RollbackDryRun,
                    registered.scope.scope_digest.clone(),
                    current.revision_id.clone(),
                    ConfigurationMutationSinkV1::ConfigurationStore,
                    ConfigurationMutationEffectV1::CreateProtectedChangePlan,
                    deadline.expires_at,
                    observed_at,
                )?;
                let plan = client
                    .dry_run_rollback(
                        mutation_authority,
                        ConfigurationRollbackRequest {
                            target_revision_id: request.target_revision_id,
                            mode: request.mode,
                        },
                    )
                    .await?;
                configuration_preview(
                    serde_json::to_value(&plan).map_err(|_| ConfigurationError::Unavailable)?,
                    authority,
                    plan.plan_id.as_str(),
                    plan.operation_digest,
                    &current.revision_id,
                    observed_at,
                    deadline,
                )
            }
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationRollbackApply,
                ConfigurationWireRequestV1::RollbackApply(request),
            ) => {
                let mutation_authority = issue_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    Some(request.idempotency_key.clone()),
                    ConfigurationMutationOperationV1::RollbackApply,
                    registered.scope.scope_digest.clone(),
                    request.expected_base_revision_id.clone(),
                    ConfigurationMutationSinkV1::ConfigurationStore,
                    ConfigurationMutationEffectV1::CommitConfigurationRevision,
                    deadline.expires_at,
                    observed_at,
                )?;
                let receipt = client
                    .apply_rollback(
                        mutation_authority,
                        ProtectedApplyRequest {
                            plan_id: request.plan_id,
                            actor_id: registered.actor.clone(),
                            expected_base_revision_id: request.expected_base_revision_id.clone(),
                            operation_digest: request.operation_digest,
                            idempotency_key: request.idempotency_key.clone(),
                        },
                    )
                    .await?;
                reconcile_configuration_runtime(&registered, &receipt, observed_at).await;
                configuration_effect(
                    serde_json::to_value(&receipt).map_err(|_| ConfigurationError::Unavailable)?,
                    authority,
                    &registered.actor,
                    &registered.scope,
                    surface_operation,
                    &request.idempotency_key,
                    &request.expected_base_revision_id,
                    receipt.operation_digest,
                    receipt.settlement_authority,
                    receipt.created_at,
                    receipt.effective_deadline_at,
                )
            }
            _ => Err(ConfigurationError::validation_message(
                "configuration surface operation does not match its request",
            )),
        }
    }
    .await;

    match result {
        Ok(outcome) => DaemonInvocationResponse::with_outcome(
            wire_request_id,
            DaemonInvocationOutcome::Configuration {
                scope: registered.scope,
                outcome,
            },
        ),
        Err(error) => application_problem(wire_request_id, configuration_problem(error)),
    }
}

async fn apply_configuration_or_semantic_transition(
    registered: &RegisteredConfigurationRuntime,
    authority: ConfigurationMutationAuthority,
    mutation: DirectConfigurationMutation,
    expected_revision: ConfigurationRevisionId,
    now: UtcMicros,
) -> Result<tracedecay_usecases::configuration::ConfigurationMutationReceipt, ConfigurationError> {
    let requested_semantic_profile = semantic_profile_transition(&mutation)?;
    let current = registered.runtime.client().current().await?;
    let semantic_profile = requested_semantic_profile.filter(|requested| {
        requires_coordinated_semantic_profile_transition(
            current.config.semantic.active_profile.is_some(),
            requested.is_some(),
        )
    });
    let receipt = if current.revision_id != expected_revision {
        registered
            .runtime
            .client()
            .mutate_direct(authority, mutation, expected_revision)
            .await?
    } else if let Some(semantic_profile) = semantic_profile {
        let operation = registered
            .semantic_operation
            .get()
            .cloned()
            .ok_or(ConfigurationError::Unavailable)?;
        match semantic_profile {
            Some(selected_profile) => operation
                .activate(SemanticProtectedActivationOperationV1 {
                    authority,
                    selected_profile,
                    central_mutation: mutation,
                    now,
                })
                .await
                .map(|applied| applied.configuration_receipt)
                .map_err(map_semantic_configuration_error)?,
            None => operation
                .rollback(SemanticProtectedRollbackOperationV1 {
                    authority,
                    central_mutation: mutation,
                    trigger: "configuration_semantic_profile_disabled".to_owned(),
                    now,
                })
                .await
                .map(|applied| applied.configuration_receipt)
                .map_err(map_semantic_configuration_error)?,
        }
    } else {
        registered
            .runtime
            .client()
            .mutate_direct(authority, mutation, expected_revision)
            .await?
    };
    reconcile_configuration_runtime(registered, &receipt, now).await;
    Ok(receipt)
}

pub(super) fn requires_coordinated_semantic_profile_transition(
    current_active: bool,
    requested_active: bool,
) -> bool {
    current_active || requested_active
}

fn semantic_profile_transition(
    mutation: &DirectConfigurationMutation,
) -> Result<Option<Option<crate::config::SemanticProfileSelection>>, ConfigurationError> {
    match mutation {
        DirectConfigurationMutation::Set { key, value, .. }
            if key.as_str() == crate::config::SEMANTIC_RUNTIME_SETTING_KEY =>
        {
            let tracedecay_domain::configuration::ConfigurationValueV1::Text(value) =
                value.as_ref()
            else {
                return Err(ConfigurationError::validation_message(
                    "semantic runtime configuration must be canonical JSON text",
                ));
            };
            let semantic: crate::config::SemanticConfig =
                serde_json::from_str(value).map_err(|_| {
                    ConfigurationError::validation_message(
                        "semantic runtime configuration is invalid",
                    )
                })?;
            semantic.validate().map_err(|_| {
                ConfigurationError::validation_message("semantic runtime configuration is invalid")
            })?;
            Ok(Some(semantic.active_profile))
        }
        DirectConfigurationMutation::Unset { key, .. }
            if key.as_str() == crate::config::SEMANTIC_RUNTIME_SETTING_KEY =>
        {
            Ok(Some(None))
        }
        DirectConfigurationMutation::Batch { mutations } => {
            let mut semantic = None;
            for mutation in mutations {
                if let Some(next) = semantic_profile_transition(mutation)?
                    && semantic.replace(next).is_some()
                {
                    return Err(ConfigurationError::validation_message(
                        "semantic runtime configuration appears more than once",
                    ));
                }
            }
            Ok(semantic)
        }
        _ => Ok(None),
    }
}

fn map_semantic_configuration_error(
    error: SemanticActivationCoordinationErrorV1,
) -> ConfigurationError {
    match error {
        SemanticActivationCoordinationErrorV1::Unavailable => ConfigurationError::Unavailable,
        SemanticActivationCoordinationErrorV1::Conflict => ConfigurationError::RevisionConflict,
        SemanticActivationCoordinationErrorV1::Rejected
        | SemanticActivationCoordinationErrorV1::RejectedDetail(_)
        | SemanticActivationCoordinationErrorV1::Runtime(_) => {
            ConfigurationError::validation_message("semantic configuration transition rejected")
        }
    }
}

fn issue_configuration_mutation_authority(
    registered: &RegisteredConfigurationRuntime,
    request_id: &str,
    idempotency_key: Option<ConfigurationIdempotencyKey>,
    operation: ConfigurationMutationOperationV1,
    scope_digest: ManifestDigest,
    expected_revision: ConfigurationRevisionId,
    sink: ConfigurationMutationSinkV1,
    effect: ConfigurationMutationEffectV1,
    effective_deadline_at: UtcMicros,
    observed_at: UtcMicros,
) -> Result<ConfigurationMutationAuthority, ConfigurationError> {
    registered
        .grants
        .issue(
            request_id,
            operation,
            scope_digest,
            expected_revision,
            sink,
            effect,
            idempotency_key,
            effective_deadline_at,
            observed_at,
        )
        .map_err(|_| ConfigurationError::Unavailable)
}

fn issue_direct_configuration_mutation_authority(
    registered: &RegisteredConfigurationRuntime,
    request_id: &str,
    idempotency_key: ConfigurationIdempotencyKey,
    mutation: &DirectConfigurationMutation,
    expected_revision: ConfigurationRevisionId,
    effective_deadline_at: UtcMicros,
    observed_at: UtcMicros,
) -> Result<ConfigurationMutationAuthority, ConfigurationError> {
    registered
        .grants
        .issue_direct(
            request_id,
            idempotency_key,
            mutation,
            expected_revision,
            effective_deadline_at,
            observed_at,
        )
        .map_err(|problem| match problem {
            DaemonInvocationProblem::NotFoundOrNotAuthorized => {
                ConfigurationError::MutationAuthorityRejected
            }
            DaemonInvocationProblem::InvalidRequest => {
                ConfigurationError::validation_message("invalid configuration mutation target")
            }
            _ => ConfigurationError::Unavailable,
        })
}

fn configuration_request_authority(
    registered: &RegisteredConfigurationRuntime,
    request_id: &str,
    operation: crate::application_surface::ApplicationSurfaceOperation,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<AuthorityReceipt, ApplicationProblem> {
    if observed_at >= registered.grants.expires_at {
        return Err(ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ));
    }
    let application_operation =
        tracedecay_application::configuration::configuration_surface_operation(operation.as_str())
            .map_err(|_| invalid_configuration_request())?
            .ok_or_else(invalid_configuration_request)?;
    let expires_at = UtcMicros(deadline.expires_at.0.min(registered.grants.expires_at.0));
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.daemon.configuration.{request_id}"))
            .map_err(|_| invalid_configuration_request())?,
        1,
        stable_digest(&(
            "tracedecay.daemon.configuration-route-grant.v1",
            request_id,
            &registered.scope,
            operation,
        ))?,
        ActorId::new("actor.tracedecay-daemon").map_err(|_| invalid_configuration_request())?,
        observed_at,
        expires_at,
        registered.scope.clone(),
        std::collections::BTreeSet::from([application_operation.capability_id().clone()]),
        std::collections::BTreeSet::from([application_operation.use_case_id().clone()]),
        DisclosureClass::Sensitive,
    )
    .map_err(|_| invalid_configuration_request())?;
    let context = RequestContext::new(
        registered.actor.clone(),
        registered.scope.clone(),
        grant,
        RequestId::new(request_id).map_err(|_| invalid_configuration_request())?,
        deadline,
        cancellation,
    )
    .map_err(|_| invalid_configuration_request())?;
    let policy_digest = ManifestDigest::new(registered.grants.policy_digest.as_str().to_owned())
        .map_err(|_| invalid_configuration_request())?;
    AuthorityReceipt::from_context(
        &context,
        PolicyDecisionRef::new(
            "policy.daemon.configuration.v1",
            registered.grants.policy_epoch,
            policy_digest,
            ComponentVersion::new("tracedecay.daemon.configuration-policy.v1")
                .map_err(|_| invalid_configuration_request())?,
        )
        .map_err(|_| invalid_configuration_request())?,
        observed_at,
    )
    .map_err(|_| invalid_configuration_request())
}

pub(super) fn context_scout_request_authority(
    registered: &RegisteredConfigurationRuntime,
    request_id: &str,
    operation: crate::application_surface::ApplicationSurfaceOperation,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<AuthorityReceipt, ApplicationProblem> {
    if observed_at >= registered.grants.expires_at {
        return Err(ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ));
    }
    let application_operation =
        tracedecay_application::context_scout::context_scout_surface_operation(operation.as_str())
            .map_err(|_| invalid_configuration_request())?
            .ok_or_else(invalid_configuration_request)?;
    let expires_at = UtcMicros(deadline.expires_at.0.min(registered.grants.expires_at.0));
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.daemon.context-scout.{request_id}"))
            .map_err(|_| invalid_configuration_request())?,
        1,
        stable_digest(&(
            "tracedecay.daemon.context-scout-route-grant.v1",
            request_id,
            &registered.scope,
            operation,
        ))?,
        ActorId::new("actor.tracedecay-daemon").map_err(|_| invalid_configuration_request())?,
        observed_at,
        expires_at,
        registered.scope.clone(),
        std::collections::BTreeSet::from([application_operation.capability_id().clone()]),
        std::collections::BTreeSet::from([application_operation.use_case_id().clone()]),
        DisclosureClass::Sensitive,
    )
    .map_err(|_| invalid_configuration_request())?;
    let context = RequestContext::new(
        registered.actor.clone(),
        registered.scope.clone(),
        grant,
        RequestId::new(request_id).map_err(|_| invalid_configuration_request())?,
        deadline,
        cancellation,
    )
    .map_err(|_| invalid_configuration_request())?;
    let policy_digest = ManifestDigest::new(registered.grants.policy_digest.as_str().to_owned())
        .map_err(|_| invalid_configuration_request())?;
    AuthorityReceipt::from_context(
        &context,
        PolicyDecisionRef::new(
            "policy.daemon.context-scout.v1",
            registered.grants.policy_epoch,
            policy_digest,
            ComponentVersion::new("tracedecay.daemon.context-scout-policy.v1")
                .map_err(|_| invalid_configuration_request())?,
        )
        .map_err(|_| invalid_configuration_request())?,
        observed_at,
    )
    .map_err(|_| invalid_configuration_request())
}

pub(super) fn configuration_evidence(
    payload: serde_json::Value,
    authority: AuthorityReceipt,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<ApplicationOutcome<serde_json::Value>, ConfigurationError> {
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )
    .map_err(ConfigurationError::validation)?;
    let packet = EvidencePacket {
        temporal: TemporalState::current(execution.ended_at),
        authority,
        evidence_authorities: Vec::new(),
        coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Operational], 1, 1, 1)
            .map_err(ConfigurationError::validation)?,
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(
            SortContractId::new("sort.configuration.stable.v1")
                .map_err(ConfigurationError::validation)?,
            1,
            Some(1),
            1,
        )
        .map_err(ConfigurationError::validation)?,
        execution,
        payload: Some(payload),
    };
    Ok(ApplicationOutcome::Evidence(packet))
}

fn configuration_preview(
    payload: serde_json::Value,
    authority: AuthorityReceipt,
    preview_id: &str,
    preview_digest: ManifestDigest,
    expected_revision: &ConfigurationRevisionId,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<ApplicationOutcome<serde_json::Value>, ConfigurationError> {
    let expected_state = canonical_sha256(&(
        "tracedecay.configuration.expected-revision.v1",
        expected_revision,
    ))
    .map_err(ConfigurationError::validation)?;
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )
    .map_err(ConfigurationError::validation)?;
    Ok(ApplicationOutcome::Preview(
        PreviewResult::new(
            PreviewId::new(preview_id.to_owned()).map_err(ConfigurationError::validation)?,
            preview_digest,
            EffectClass::ConfigurationWrite,
            authority,
            expected_state,
            execution,
            Some(payload),
        )
        .map_err(ConfigurationError::validation)?,
    ))
}

fn invalid_configuration_request() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "configuration.invalid_request".to_owned(),
            message: "The configuration request is invalid".to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: Vec::new(),
    }
}

pub(super) fn configuration_problem(error: ConfigurationError) -> ApplicationProblem {
    match error {
        ConfigurationError::TargetUnavailable
        | ConfigurationError::AuthorizedTargetAmbiguous
        | ConfigurationError::MutationAuthorityRejected
        | ConfigurationError::ProjectlessProfileRequired => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        ConfigurationError::RevisionConflict | ConfigurationError::IdempotencyConflict => {
            ApplicationProblem::Conflict {
                diagnostic: SafeDiagnostic {
                    code: "configuration.conflict".to_owned(),
                    message: "The configuration request conflicts with current state".to_owned(),
                },
                retry: RetryDirective::AfterRevalidate,
                legal_actions: vec![tracedecay_application::LegalAction::Refresh],
            }
        }
        ConfigurationError::PlanExpired | ConfigurationError::PlanStale => {
            ApplicationProblem::stale(SafeDiagnostic {
                code: "configuration.stale".to_owned(),
                message: "The configuration preview is stale".to_owned(),
            })
        }
        ConfigurationError::PolicyWideningForbidden | ConfigurationError::Validation(_) => {
            invalid_configuration_request()
        }
        ConfigurationError::Unavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "configuration.unavailable".to_owned(),
            message: "The configuration authority is unavailable".to_owned(),
        }),
        ConfigurationError::ResetRequired { .. } => {
            ApplicationProblem::reset_required(SafeDiagnostic {
                code: "configuration.reset_required".to_owned(),
                message: "The configuration store must be reset before use".to_owned(),
            })
        }
    }
}

/// Mounts one project's semantic-runtime scheduling handle as daemon-private
/// retained state. Semantic scheduling is never a wire operation: the daemon
/// consults the retained handle for status/coverage and to hand work to the
/// bounded background scheduler, and clients observe only the typed
/// freshness/coverage that ordinary operations already report.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DaemonSemanticRuntimeRegistrationError {
    #[error("a semantic runtime scheduler is already mounted for this project")]
    AlreadyRegistered,
    #[error("the daemon project runtime registry is closed")]
    RegistryClosed,
}

impl From<ProjectRuntimeAlreadyRegistered> for DaemonSemanticRuntimeRegistrationError {
    fn from(_: ProjectRuntimeAlreadyRegistered) -> Self {
        Self::AlreadyRegistered
    }
}

impl From<ProjectRuntimeRegistryError> for DaemonSemanticRuntimeRegistrationError {
    fn from(error: ProjectRuntimeRegistryError) -> Self {
        match error {
            ProjectRuntimeRegistryError::AlreadyRegistered => Self::AlreadyRegistered,
            ProjectRuntimeRegistryError::Closed => Self::RegistryClosed,
        }
    }
}

pub(crate) struct DaemonSemanticRuntimeRegistrar {
    service: DaemonInvocationService,
}

impl DaemonSemanticRuntimeRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    pub(crate) async fn register(
        &self,
        project_root: PathBuf,
        handle: crate::semantic_code::DaemonSemanticRuntimeHandleV1,
    ) -> Result<(), DaemonSemanticRuntimeRegistrationError> {
        self.service
            .project_runtimes
            .register_or_reconcile(
                project_root.clone(),
                |_: &mut crate::semantic_code::DaemonSemanticRuntimeHandleV1| {
                    Err(DaemonSemanticRuntimeRegistrationError::AlreadyRegistered)
                },
                || {
                    // The process-wide table is only joined once the project slot
                    // is known to be free, so a refused registration cannot
                    // replace a live handle there.
                    tracedecay_usecases::semantic_runtime::register_project_semantic_runtime(
                        project_root.clone(),
                        handle.clone(),
                    );
                    Ok(handle)
                },
            )
            .await
    }
}

#[cfg(test)]
mod terminal_problem_tests {
    use super::*;

    #[test]
    fn configuration_reset_preserves_its_terminal_category() {
        let problem = configuration_problem(ConfigurationError::ResetRequired {
            reason: "fixture reset".to_owned(),
        });
        let ApplicationProblem::ResetRequired {
            retry,
            legal_actions,
            ..
        } = problem
        else {
            panic!("configuration reset must remain reset-required");
        };

        assert_eq!(retry, RetryDirective::Never);
        assert_eq!(
            legal_actions,
            vec![tracedecay_application::LegalAction::Reset]
        );
    }
}
