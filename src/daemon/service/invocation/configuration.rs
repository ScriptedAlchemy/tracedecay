//! Configuration daemon invocation handlers: mutation, evidence, preview, and semantic-profile transitions.

use super::*;

pub(super) async fn execute_configuration(
    wire_request_id: String,
    registered: Option<RegisteredConfigurationRuntime>,
    surface_operation: crate::application_surface::ApplicationSurfaceOperation,
    request: ConfigurationSurfaceRequest,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(registered) = registered else {
        return concealed_application_problem(wire_request_id);
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
                ConfigurationSurfaceRequest::List(_),
            ) => configuration_evidence(
                serde_json::to_value(client.list(actor).await?)
                    .map_err(|_| ConfigurationError::Unavailable)?,
                authority,
                observed_at,
                deadline,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationExplain,
                ConfigurationSurfaceRequest::Explain(request),
            ) => configuration_evidence(
                serde_json::to_value(client.explain(actor, request.key).await?)
                    .map_err(|_| ConfigurationError::Unavailable)?,
                authority,
                observed_at,
                deadline,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationGet,
                ConfigurationSurfaceRequest::Get(request),
            ) => configuration_evidence(
                serde_json::to_value(client.get(actor, request.key).await?)
                    .map_err(|_| ConfigurationError::Unavailable)?,
                authority,
                observed_at,
                deadline,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationObservedState,
                ConfigurationSurfaceRequest::ObservedState(_),
            ) => configuration_evidence(
                serde_json::to_value(client.observed_state(actor).await?)
                    .map_err(|_| ConfigurationError::Unavailable)?,
                authority,
                observed_at,
                deadline,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationAudit,
                ConfigurationSurfaceRequest::Audit(request),
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
                ConfigurationSurfaceRequest::Set(request),
            ) => {
                let mutation = DirectConfigurationMutation::Set {
                    layer: request.layer,
                    key: request.key,
                    value: request.value,
                };
                let mutation_authority = issue_direct_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    &mutation,
                    request.expected_revision.clone(),
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
                    &wire_request_id,
                    &request.expected_revision,
                    receipt.operation_digest,
                    observed_at,
                    deadline,
                )
            }
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationUnset,
                ConfigurationSurfaceRequest::Unset(request),
            ) => {
                let mutation = DirectConfigurationMutation::Unset {
                    layer: request.layer,
                    key: request.key,
                };
                let mutation_authority = issue_direct_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    &mutation,
                    request.expected_revision.clone(),
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
                    &wire_request_id,
                    &request.expected_revision,
                    receipt.operation_digest,
                    observed_at,
                    deadline,
                )
            }
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationBatch,
                ConfigurationSurfaceRequest::Batch(request),
            ) => {
                let mutations = request
                    .mutations
                    .into_iter()
                    .map(|mutation| match mutation {
                        crate::application_surface::ConfigurationDirectMutationSurfaceRequest::Set {
                            layer,
                            key,
                            value,
                        } => DirectConfigurationMutation::Set { layer, key, value },
                        crate::application_surface::ConfigurationDirectMutationSurfaceRequest::Unset {
                            layer,
                            key,
                        } => DirectConfigurationMutation::Unset { layer, key },
                    })
                    .collect();
                let mutation = DirectConfigurationMutation::Batch { mutations };
                let mutation_authority = issue_direct_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    &mutation,
                    request.expected_revision.clone(),
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
                    &wire_request_id,
                    &request.expected_revision,
                    receipt.operation_digest,
                    observed_at,
                    deadline,
                )
            }
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationWriteCredential,
                ConfigurationSurfaceRequest::WriteCredential(request),
            ) => {
                let mutation_authority = issue_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    ConfigurationMutationOperationV1::CredentialWrite,
                    registered.scope.scope_digest.clone(),
                    request.expected_revision.clone(),
                    ConfigurationMutationSinkV1::CredentialStore,
                    ConfigurationMutationEffectV1::WriteCredentialReference,
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
                let digest = canonical_sha256(&(
                    "tracedecay.configuration.credential-surface.v1",
                    &payload,
                ))
                .map_err(ConfigurationError::validation)?;
                configuration_effect(
                    payload,
                    authority,
                    &registered.actor,
                    &registered.scope,
                    surface_operation,
                    &wire_request_id,
                    &request.expected_revision,
                    digest,
                    observed_at,
                    deadline,
                )
            }
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationProtectedPreview,
                ConfigurationSurfaceRequest::ProtectedPreview(request),
            ) => {
                let mutation_authority = issue_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    ConfigurationMutationOperationV1::ProtectedDryRun,
                    registered.scope.scope_digest.clone(),
                    request.expected_revision.clone(),
                    ConfigurationMutationSinkV1::ConfigurationStore,
                    ConfigurationMutationEffectV1::CreateProtectedChangePlan,
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
                ConfigurationSurfaceRequest::ProtectedApply(request),
            ) => {
                let mutation_authority = issue_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    ConfigurationMutationOperationV1::ProtectedApply,
                    registered.scope.scope_digest.clone(),
                    request.expected_base_revision_id.clone(),
                    ConfigurationMutationSinkV1::ConfigurationStore,
                    ConfigurationMutationEffectV1::CommitConfigurationRevision,
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
                            idempotency_key: request.idempotency_key,
                        },
                    )
                    .await?;
                configuration_effect(
                    serde_json::to_value(&receipt).map_err(|_| ConfigurationError::Unavailable)?,
                    authority,
                    &registered.actor,
                    &registered.scope,
                    surface_operation,
                    &wire_request_id,
                    &request.expected_base_revision_id,
                    receipt.operation_digest,
                    observed_at,
                    deadline,
                )
            }
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationRollbackPreview,
                ConfigurationSurfaceRequest::RollbackPreview(request),
            ) => {
                let current = client.current().await?;
                let mutation_authority = issue_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    ConfigurationMutationOperationV1::RollbackDryRun,
                    registered.scope.scope_digest.clone(),
                    current.revision_id.clone(),
                    ConfigurationMutationSinkV1::ConfigurationStore,
                    ConfigurationMutationEffectV1::CreateProtectedChangePlan,
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
                ConfigurationSurfaceRequest::RollbackApply(request),
            ) => {
                let mutation_authority = issue_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    ConfigurationMutationOperationV1::RollbackApply,
                    registered.scope.scope_digest.clone(),
                    request.expected_base_revision_id.clone(),
                    ConfigurationMutationSinkV1::ConfigurationStore,
                    ConfigurationMutationEffectV1::CommitConfigurationRevision,
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
                            idempotency_key: request.idempotency_key,
                        },
                    )
                    .await?;
                configuration_effect(
                    serde_json::to_value(&receipt).map_err(|_| ConfigurationError::Unavailable)?,
                    authority,
                    &registered.actor,
                    &registered.scope,
                    surface_operation,
                    &wire_request_id,
                    &request.expected_base_revision_id,
                    receipt.operation_digest,
                    observed_at,
                    deadline,
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
) -> Result<crate::application::configuration::ConfigurationMutationReceipt, ConfigurationError> {
    let semantic_profile = semantic_profile_transition(&mutation)?;
    let receipt = if let Some(semantic_profile) = semantic_profile {
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
    let current = registered.runtime.client().current().await?;
    let root_current = crate::config::root_runtime_configuration(&current)
        .map_err(|_| ConfigurationError::Unavailable)?;
    crate::config::install_pinned_runtime_configuration(root_current)
        .map_err(|_| ConfigurationError::Unavailable)?;
    registered
        .runtime
        .record_runtime_activation(Some(current.revision_id), None, now)
        .await?;
    Ok(receipt)
}

fn semantic_profile_transition(
    mutation: &DirectConfigurationMutation,
) -> Result<Option<Option<crate::config::SemanticProfileSelection>>, ConfigurationError> {
    match mutation {
        DirectConfigurationMutation::Set { key, value, .. }
            if key.as_str() == crate::config::SEMANTIC_RUNTIME_SETTING_KEY =>
        {
            let tracedecay_domain::configuration::ConfigurationValueV1::Text(value) = value else {
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
        | SemanticActivationCoordinationErrorV1::Runtime(_) => {
            ConfigurationError::validation_message("semantic configuration transition rejected")
        }
    }
}

fn issue_configuration_mutation_authority(
    registered: &RegisteredConfigurationRuntime,
    request_id: &str,
    operation: ConfigurationMutationOperationV1,
    scope_digest: ManifestDigest,
    expected_revision: ConfigurationRevisionId,
    sink: ConfigurationMutationSinkV1,
    effect: ConfigurationMutationEffectV1,
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
            observed_at,
        )
        .map_err(|_| ConfigurationError::Unavailable)
}

fn issue_direct_configuration_mutation_authority(
    registered: &RegisteredConfigurationRuntime,
    request_id: &str,
    mutation: &DirectConfigurationMutation,
    expected_revision: ConfigurationRevisionId,
    observed_at: UtcMicros,
) -> Result<ConfigurationMutationAuthority, ConfigurationError> {
    registered
        .grants
        .issue_direct(request_id, mutation, expected_revision, observed_at)
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

#[allow(clippy::too_many_arguments)]
fn configuration_effect(
    payload: serde_json::Value,
    authority: AuthorityReceipt,
    actor: &ActorId,
    scope: &ResolvedScope,
    operation: crate::application_surface::ApplicationSurfaceOperation,
    request_id: &str,
    expected_revision: &ConfigurationRevisionId,
    operation_digest: ManifestDigest,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<ApplicationOutcome<serde_json::Value>, ConfigurationError> {
    let application_operation =
        tracedecay_application::configuration::configuration_surface_operation(operation.as_str())
            .map_err(ConfigurationError::validation)?
            .ok_or_else(|| {
                ConfigurationError::validation_message("unknown configuration operation")
            })?;
    let idempotency_digest = derive_logical_effect_idempotency(
        LogicalEffectIdempotencyDomain::ConfigurationEffect,
        &(
            actor,
            scope,
            operation.as_str(),
            expected_revision,
            &operation_digest,
        ),
    )
    .map_err(|error| ConfigurationError::validation_message(error.to_string()))?;
    let idempotency_suffix = idempotency_digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| {
            ConfigurationError::validation_message(
                "configuration effect idempotency digest is malformed",
            )
        })?;
    let idempotency_key = IdempotencyKey::new(format!("configuration.effect.{idempotency_suffix}"))
        .map_err(ConfigurationError::validation)?;
    let expected_state = canonical_sha256(&(
        "tracedecay.configuration.expected-revision.v1",
        expected_revision,
    ))
    .map_err(ConfigurationError::validation)?;
    let committed_state = canonical_sha256(&(
        "tracedecay.configuration.committed-effect.v1",
        &operation_digest,
        &payload,
    ))
    .map_err(ConfigurationError::validation)?;
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )
    .map_err(ConfigurationError::validation)?;
    let receipt = EffectReceipt {
        operation: application_operation.use_case_id().clone(),
        request_id: RequestId::new(request_id).map_err(ConfigurationError::validation)?,
        actor: actor.clone(),
        scope: scope.clone(),
        effect_class: EffectClass::ConfigurationWrite,
        idempotency_key: idempotency_key.clone(),
        input_digest: operation_digest,
        expected_state: expected_state.clone(),
        policy_digest: authority.policy.digest.clone(),
        configuration_digest: committed_state.clone(),
        catalog_digest: stable_digest(&"tracedecay.application.catalog.v1")
            .map_err(|_| ConfigurationError::Unavailable)?,
        privacy_digest: stable_digest(&"tracedecay.application.privacy.v1")
            .map_err(|_| ConfigurationError::Unavailable)?,
        outcome: EffectTermination::Completed,
        committed_state: Some(committed_state),
        external_proof: None,
    };
    let effect = EffectResult::new(
        EffectId::new(format!("effect.configuration.{idempotency_suffix}"))
            .map_err(ConfigurationError::validation)?,
        EffectClass::ConfigurationWrite,
        idempotency_key,
        authority,
        expected_state,
        execution,
        ReconciliationState::Reconciled,
        receipt,
        Some(payload),
    )
    .map_err(ConfigurationError::validation)?;
    Ok(ApplicationOutcome::Effect(effect))
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
                    crate::application::semantic_runtime::register_project_semantic_runtime(
                        project_root.clone(),
                        handle.clone(),
                    );
                    Ok(handle)
                },
            )
            .await
    }
}

impl DaemonInvocationService {
    pub(super) async fn configuration_runtime(
        &self,
        project_root: Option<&Path>,
    ) -> Option<RegisteredConfigurationRuntime> {
        self.project_runtimes.get(project_root?).await
    }

    pub(super) async fn execute_semantic_evaluation(
        &self,
        project_root: Option<&Path>,
        request_id: String,
        candidate: crate::application::semantic_runtime::SemanticEvaluationProfileCandidateV1,
    ) -> DaemonInvocationResponse {
        let Some(project_root) = project_root else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        };
        let Some(registered) = self.configuration_runtime(Some(project_root)).await else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        };
        let Some(operation) = registered.semantic_operation.get().cloned() else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        };
        let canonical_root = match project_root.canonicalize() {
            Ok(root) => root,
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            }
        };
        let authority =
            crate::daemon::semantic_evaluation::DaemonSemanticEvaluationSnapshotAuthorityV1::new(
                canonical_root.clone(),
                registered.scope.clone(),
                self.code_index_schedulers.clone(),
                candidate.clone(),
            );
        match operation
            .evaluate_and_publish_profile(&authority, &canonical_root, candidate)
            .await
        {
            Ok(publication) => DaemonInvocationResponse::with_outcome(
                request_id,
                DaemonInvocationOutcome::SemanticEvaluatedProfilePublished {
                    scope: publication.snapshot.scope,
                    profile_digest: publication.accepted_profile.profile_digest().clone(),
                    report_digest: publication
                        .accepted_profile
                        .evaluation()
                        .report_digest()
                        .clone(),
                    report: publication.report,
                    source_generation: publication.snapshot.code_generation,
                    snapshot_digest: publication.snapshot.code_snapshot_digest,
                },
            ),
            Err(SemanticActivationCoordinationErrorV1::Rejected) => {
                DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::InvalidRequest,
                )
            }
            Err(SemanticActivationCoordinationErrorV1::Conflict) => {
                DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable)
            }
            Err(SemanticActivationCoordinationErrorV1::Runtime(_)) => {
                DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable)
            }
            Err(SemanticActivationCoordinationErrorV1::Unavailable) => {
                DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable)
            }
        }
    }
}
