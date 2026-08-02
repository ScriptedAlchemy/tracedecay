//! PR12 primitive, callable-code, and context-scout daemon invocation handlers.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_primitive(
    service: &DaemonInvocationService,
    project_root: Option<&Path>,
    wire_request_id: String,
    surface_operation: crate::application_surface::ApplicationSurfaceOperation,
    request: Pr12PrimitiveRequest,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(project_root) = project_root else {
        return concealed_application_problem(wire_request_id);
    };
    let dispatch = service
        .project_runtimes
        .read(project_root, Pr12PrimitiveProjectRuntime::dispatch)
        .await;
    let Some(dispatch) = dispatch else {
        return concealed_application_problem(wire_request_id);
    };
    let registered = service
        .project_runtimes
        .get::<RegisteredCallableCodeRuntime>(project_root)
        .await;
    let Some(registered) = registered else {
        return concealed_application_problem(wire_request_id);
    };
    let access = match registered.authorization.current(observed_at).await {
        Ok(access) if access.scope == registered.scope => access,
        Ok(_) | Err(_) => return concealed_application_problem(wire_request_id),
    };
    let Ok(Some(operation)) =
        tracedecay_application::feedback::feedback_surface_operation(surface_operation.as_str())
            .and_then(|operation| {
                operation.map_or_else(
                    || {
                        tracedecay_application::retrieval::catalog::primitive_read_operation(
                            surface_operation.as_str(),
                        )
                    },
                    |operation| Ok(Some(operation)),
                )
            })
    else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::InvalidRequest,
        );
    };
    let context = match callable_code_request_context(
        &registered.scope,
        &access,
        &wire_request_id,
        &operation,
        observed_at,
        deadline,
        cancellation,
    ) {
        Ok(context) => context,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let authorization = registered.authorization.authorize(access);
    let admission = match authorization.admit(&context, &operation, observed_at).await {
        Ok(admission) => admission,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let mut result = dispatch
        .dispatch(
            Pr12PrimitiveInvocation {
                operation: operation.clone(),
                request,
            },
            context.clone(),
            observed_at,
        )
        .await;
    if result.is_ok() {
        let finished_at = current_micros();
        let publication_authority = match authorization
            .recheck_publication(&context, &operation, &admission, finished_at)
            .await
        {
            Ok(authority) => authority,
            Err(problem) => return application_problem(wire_request_id, problem),
        };
        if !crate::application::primitives::runtime::reauthorize_primitive_evidence(
            &mut result,
            publication_authority,
        ) {
            return DaemonInvocationResponse::problem(
                wire_request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    }
    match feedback_invocation_result(result) {
        Ok(result) => DaemonInvocationResponse::with_outcome(
            wire_request_id,
            DaemonInvocationOutcome::Primitive {
                scope: result.scope,
                result: DaemonFeedbackResult::from_application(result.evidence),
            },
        ),
        Err(problem) => application_problem(wire_request_id, problem),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_callable_code(
    service: &DaemonInvocationService,
    project_root: Option<&Path>,
    wire_request_id: String,
    surface_operation: crate::application_surface::ApplicationSurfaceOperation,
    request: crate::application_surface::CallableCodeSurfaceRequest,
    page: PageRequest,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(project_root) = project_root else {
        return concealed_application_problem(wire_request_id);
    };
    let registered = service
        .project_runtimes
        .get::<RegisteredCallableCodeRuntime>(project_root)
        .await;
    let Some(registered) = registered else {
        return concealed_application_problem(wire_request_id);
    };
    let access = match registered.authorization.current(observed_at).await {
        Ok(access) => access,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let kind = match (&request, surface_operation) {
        (
            crate::application_surface::CallableCodeSurfaceRequest::ExactOccurrence(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeExactOccurrence,
        ) => CallableCodeOperationKind::ExactOccurrence,
        (
            crate::application_surface::CallableCodeSurfaceRequest::PhraseSearch(_),
            crate::application_surface::ApplicationSurfaceOperation::CodePhraseSearch,
        ) => CallableCodeOperationKind::PhraseSearch,
        (
            crate::application_surface::CallableCodeSurfaceRequest::Callees(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeCallees,
        ) => CallableCodeOperationKind::Callees,
        (
            crate::application_surface::CallableCodeSurfaceRequest::Facets(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeFacets,
        ) => CallableCodeOperationKind::Facets,
        (
            crate::application_surface::CallableCodeSurfaceRequest::Timeline(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeTimeline,
        ) => CallableCodeOperationKind::Timeline,
        (
            crate::application_surface::CallableCodeSurfaceRequest::Declaration(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeDeclaration,
        ) => CallableCodeOperationKind::Declaration,
        (
            crate::application_surface::CallableCodeSurfaceRequest::Definition(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeDefinition,
        ) => CallableCodeOperationKind::Definition,
        (
            crate::application_surface::CallableCodeSurfaceRequest::TypeDefinition(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeTypeDefinition,
        ) => CallableCodeOperationKind::TypeDefinition,
        (
            crate::application_surface::CallableCodeSurfaceRequest::References(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeReferences,
        ) => CallableCodeOperationKind::References,
        _ => {
            return DaemonInvocationResponse::problem(
                wire_request_id,
                DaemonInvocationProblem::InvalidRequest,
            );
        }
    };
    let Ok(operations) = callable_code_operations() else {
        return application_problem(
            wire_request_id,
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "callable_code.operation_unavailable".to_owned(),
                message: "The callable code operation is unavailable".to_owned(),
            }),
        );
    };
    let context = match callable_code_request_context(
        &registered.scope,
        &access,
        &wire_request_id,
        operations.get(kind),
        observed_at,
        deadline,
        cancellation,
    ) {
        Ok(context) => context,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let query = CallableCodeQueryService::new(
        service.code_index_schedulers.clone(),
        registered.authorization.authorize(access),
        operations,
    );
    match request {
        crate::application_surface::CallableCodeSurfaceRequest::ExactOccurrence(request) => {
            let Ok(request) = request.into_application_request(page) else {
                return invalid_callable_code_request(wire_request_id);
            };
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query.exact_occurrence(&context, request, observed_at).await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::PhraseSearch(request) => {
            let Ok(request) = request.into_application_request(
                crate::daemon::code_index_scheduler::queries::callable_query_sanitizer_revision(),
                crate::daemon::code_index_scheduler::queries::callable_query_normalization_revision(
                ),
                page,
            ) else {
                return invalid_callable_code_request(wire_request_id);
            };
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query.phrase_search(&context, request, observed_at).await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::Callees(request) => {
            let request = request.into_application_request(page);
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query.callees(&context, request, observed_at).await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::Facets(request) => {
            let request = request.into_application_request(page);
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query.facets(&context, request, observed_at).await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::Timeline(request) => {
            let request = request.into_application_request(page);
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query.timeline(&context, request, observed_at).await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::Declaration(request) => {
            let request = request.into_application_request(page);
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query.declaration(&context, request, observed_at).await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::Definition(request) => {
            let request = request.into_application_request(page);
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query.definition(&context, request, observed_at).await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::TypeDefinition(request) => {
            let request = request.into_application_request(page);
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query.type_definition(&context, request, observed_at).await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::References(request) => {
            let request = request.into_application_request(page);
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query.references(&context, request, observed_at).await,
            )
        }
    }
}

fn invalid_callable_code_request(wire_request_id: String) -> DaemonInvocationResponse {
    application_problem(
        wire_request_id,
        ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic {
                code: "callable_code.invalid_query".to_owned(),
                message: "The callable code query is invalid".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        },
    )
}

pub(super) fn callable_code_request_context(
    scope: &ResolvedScope,
    access: &ProjectSourceAccessSnapshot,
    wire_request_id: &str,
    operation: &ApplicationOperation,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<RequestContext, ApplicationProblem> {
    if scope != &access.scope {
        return Err(ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ));
    }
    if cancellation.is_cancelled() {
        return Err(ApplicationProblem::cancelled_before_admission());
    }
    if deadline.is_elapsed_at(observed_at) || deadline.is_elapsed_at(current_micros()) {
        return Err(ApplicationProblem::timed_out_before_admission());
    }
    let expires_at = UtcMicros(deadline.expires_at.0.min(access.grant_expires_at.0));
    if expires_at.0 <= observed_at.0 {
        return Err(ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ));
    }
    let request_id =
        RequestId::new(wire_request_id).map_err(|_| ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic {
                code: "callable_code.invalid_request_id".to_owned(),
                message: "The callable code request identifier is invalid".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        })?;
    // Correlation IDs stay on the RequestContext. The route authority is a
    // function of the access and the operation, so the same authorized call
    // resolves the same grant from any surface and across durable retries.
    let grant_digest = canonical_sha256(&(
        "tracedecay.daemon.callable-code-grant.v1",
        scope,
        &access.requester,
        &access.configuration_digest,
        operation.capability_id(),
        operation.use_case_id(),
    ))
    .map_err(|_| {
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "callable_code.grant_unavailable".to_owned(),
            message: "The callable code route grant is unavailable".to_owned(),
        })
    })?;
    let grant_id = CapabilityGrantId::new(format!(
        "grant.daemon.callable-code.{}",
        grant_digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|_| {
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "callable_code.grant_unavailable".to_owned(),
            message: "The callable code route grant is unavailable".to_owned(),
        })
    })?;
    let grant = CapabilityGrantSnapshot::new(
        grant_id,
        1,
        grant_digest.clone(),
        access.requester.clone(),
        observed_at,
        expires_at,
        scope.clone(),
        std::collections::BTreeSet::from([operation.capability_id().clone()]),
        std::collections::BTreeSet::from([operation.use_case_id().clone()]),
        DisclosureClass::Evidence,
    )
    .map_err(|_| {
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "callable_code.grant_unavailable".to_owned(),
            message: "The callable code route grant is unavailable".to_owned(),
        })
    })?;
    RequestContext::new(
        access.requester.clone(),
        scope.clone(),
        grant,
        request_id,
        Deadline::new(expires_at).map_err(|_| {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "callable_code.deadline_unavailable".to_owned(),
                message: "The callable code request deadline is unavailable".to_owned(),
            })
        })?,
        cancellation,
    )
    .map_err(|_| {
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "callable_code.context_unavailable".to_owned(),
            message: "The callable code request context is unavailable".to_owned(),
        })
    })
}

fn callable_code_response<T: Serialize>(
    wire_request_id: String,
    registered_scope: &ResolvedScope,
    result: ApplicationResult<T>,
) -> DaemonInvocationResponse {
    match feedback_invocation_result(result) {
        Ok(result) if &result.scope == registered_scope => DaemonInvocationResponse::with_outcome(
            wire_request_id,
            DaemonInvocationOutcome::CallableCode {
                scope: result.scope,
                result: DaemonFeedbackResult::from_application(result.evidence),
            },
        ),
        Ok(_) => concealed_application_problem(wire_request_id),
        Err(problem) => application_problem(wire_request_id, problem),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_context_scout(
    service: &DaemonInvocationService,
    wire_request_id: String,
    registered: Option<RegisteredConfigurationRuntime>,
    surface_operation: crate::application_surface::ApplicationSurfaceOperation,
    request: ContextScoutSurfaceRequest,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(registered) = registered else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::NotFoundOrNotAuthorized,
        );
    };
    let current = match registered.runtime.client().current().await {
        Ok(current) => crate::application::configuration::ConfigurationCurrentStateV1 {
            revision_id: current.revision_id,
            snapshot: current.snapshot,
        },
        Err(error) => {
            return application_problem(wire_request_id, configuration_problem(error));
        }
    };
    let Some(configuration) =
        crate::agents::context_scout_ports::ContextScoutConfigurationPinV1::from_current(&current)
    else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::NotFoundOrNotAuthorized,
        );
    };
    let registry = service
        .context_scout_registries
        .lock()
        .await
        .get(&registered.scope.project_id)
        .cloned();
    let Some(registry) = registry else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::NotFoundOrNotAuthorized,
        );
    };
    let address = request.address();
    if !registry
        .authorize_current_exact_address(address, &configuration, &registered.scope)
        .await
    {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::NotFoundOrNotAuthorized,
        );
    }
    let mut owner = None;
    for candidate in crate::agents::context_scout_owner::lookup_registered_context_scout_owners(
        address.project_id,
    ) {
        if candidate.configured_status().await.is_ok_and(|status| {
            status.configuration_revision == configuration.control().configuration_revision
        }) {
            if owner.is_some() {
                return DaemonInvocationResponse::problem(
                    wire_request_id,
                    DaemonInvocationProblem::NotFoundOrNotAuthorized,
                );
            }
            owner = Some(candidate);
        }
    }
    let Some(owner) = owner else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::NotFoundOrNotAuthorized,
        );
    };
    if let ContextScoutSurfaceRequest::Pause(control)
    | ContextScoutSurfaceRequest::Resume(control) = &request
    {
        let target = match &request {
            ContextScoutSurfaceRequest::Pause(_) => {
                tracedecay_domain::configuration::ContextScoutConfigurationStateV1::Paused
            }
            ContextScoutSurfaceRequest::Resume(_) => {
                tracedecay_domain::configuration::ContextScoutConfigurationStateV1::Active
            }
            _ => unreachable!("pause/resume matched above"),
        };
        return execute_context_scout_state_transition(
            wire_request_id,
            registered,
            owner,
            control,
            target,
            current,
            observed_at,
            deadline,
            cancellation,
        )
        .await;
    }
    let authority = match context_scout_request_authority(
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
    let payload = match request {
        ContextScoutSurfaceRequest::Status(_) => owner
            .configured_status()
            .await
            .ok()
            .and_then(|status| serde_json::to_value(status).ok()),
        ContextScoutSurfaceRequest::Recent(request) => owner
            .recent_exact(request.address, request.limit)
            .await
            .ok()
            .and_then(|recent| serde_json::to_value(recent).ok()),
        ContextScoutSurfaceRequest::Explain(request) => owner
            .explain_exact(request.address, request.limit)
            .await
            .ok()
            .and_then(|explanation| serde_json::to_value(explanation).ok()),
        ContextScoutSurfaceRequest::Capability(_) => owner
            .capability()
            .await
            .ok()
            .and_then(|capability| serde_json::to_value(capability).ok()),
        ContextScoutSurfaceRequest::Budget(_) => owner
            .budget()
            .await
            .ok()
            .and_then(|budget| serde_json::to_value(budget).ok()),
        ContextScoutSurfaceRequest::Cancel(request) if request.work.address == request.address => {
            owner
                .cancel(request.work)
                .await
                .ok()
                .filter(|outcome| {
                    *outcome
                        != crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1::Unavailable
                })
                .map(|outcome| {
                    serde_json::json!({ "outcome": context_scout_store_outcome(outcome) })
                })
        }
        ContextScoutSurfaceRequest::Claim(request) => {
            let window = match request.window {
                crate::application_surface::ContextScoutClaimWindowSurfaceV1::IdleWindow => {
                    crate::agents::context_scout_v2::ContextScoutDeliveryWindowV1::IdleWindow
                }
                crate::application_surface::ContextScoutClaimWindowSurfaceV1::OnRequest => {
                    crate::agents::context_scout_v2::ContextScoutDeliveryWindowV1::OnRequest
                }
            };
            let digest = canonical_sha256(&(
                "tracedecay.context-scout.delivery-lease.v1",
                &wire_request_id,
                request.address,
                request.window,
                observed_at,
            ))
            .ok();
            let lease = digest.and_then(|digest| {
                let bytes = digest.as_str().as_bytes();
                (bytes.len() >= 16).then(|| {
                    let mut lease_id = [0; 16];
                    lease_id.copy_from_slice(&bytes[..16]);
                    crate::agents::context_scout_v2::ContextScoutLeaseV1 {
                        lease_id,
                        expires_at: UtcMicros(
                            deadline
                                .expires_at
                                .0
                                .min(observed_at.0.saturating_add(30_000_000)),
                        ),
                    }
                })
            });
            match lease {
                Some(lease) => match owner
                    .claim_delivery_exact(request.address, window, observed_at, lease)
                    .await
                {
                    crate::agents::context_scout_v2::ContextScoutDurableClaimOutcomeV1::Claimed(
                        claim,
                    ) => serde_json::to_value(claim).ok(),
                    crate::agents::context_scout_v2::ContextScoutDurableClaimOutcomeV1::Empty => {
                        Some(serde_json::json!({ "outcome": "empty" }))
                    }
                    crate::agents::context_scout_v2::ContextScoutDurableClaimOutcomeV1::Unavailable => {
                        None
                    }
                },
                None => None,
            }
        }
        ContextScoutSurfaceRequest::Delivery(request)
            if request.claim.entry.work.address == request.address =>
        {
            let outcome = owner
                .record_delivery(&request.claim, &request.receipt)
                .await;
            (outcome
                != crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1::Unavailable)
                .then(|| {
                    serde_json::json!({
                        "outcome": context_scout_store_outcome(outcome)
                    })
                })
        }
        ContextScoutSurfaceRequest::Feedback(request) => {
            let outcome = owner
                .record_feedback_exact(request.address, &request.receipt, request.feedback)
                .await;
            (outcome
                != crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1::Unavailable)
                .then(|| {
                    serde_json::json!({
                        "outcome": context_scout_store_outcome(outcome)
                    })
                })
        }
        ContextScoutSurfaceRequest::Pause(_)
        | ContextScoutSurfaceRequest::Resume(_)
        | ContextScoutSurfaceRequest::Cancel(_)
        | ContextScoutSurfaceRequest::Delivery(_) => None,
    };
    let Some(payload) = payload else {
        return application_problem(
            wire_request_id,
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "context_scout.unavailable".to_owned(),
                message: "The exact-address Context Scout operation is unavailable".to_owned(),
            }),
        );
    };
    match configuration_evidence(payload, authority, observed_at, deadline) {
        Ok(outcome) => DaemonInvocationResponse::with_outcome(
            wire_request_id,
            DaemonInvocationOutcome::ContextScout {
                scope: registered.scope,
                outcome,
            },
        ),
        Err(error) => application_problem(wire_request_id, configuration_problem(error)),
    }
}

async fn execute_context_scout_state_transition(
    wire_request_id: String,
    registered: RegisteredConfigurationRuntime,
    owner: Arc<crate::agents::context_scout_owner::ProjectContextScoutOwnerV1>,
    control: &crate::application_surface::ContextScoutControlSurfaceRequest,
    target: tracedecay_domain::configuration::ContextScoutConfigurationStateV1,
    current: crate::application::configuration::ConfigurationCurrentStateV1,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    if control.expected_revision != current.revision_id {
        return application_problem(
            wire_request_id,
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "context_scout.configuration_stale".to_owned(),
                message: "The Context Scout configuration revision is stale".to_owned(),
            }),
        );
    }
    let Some(key) = tracedecay_domain::configuration::SettingKey::new(
        tracedecay_domain::configuration::CONTEXT_SCOUT_SETTINGS_SETTING_KEY,
    )
    .ok() else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::Unavailable,
        );
    };
    let Some(tracedecay_domain::configuration::ConfigurationValueV1::ContextScoutSettings(
        mut settings,
    )) = current.snapshot.effective_values.get(&key).cloned()
    else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::NotFoundOrNotAuthorized,
        );
    };
    let valid_transition = matches!(
        (settings.state, target),
        (
            tracedecay_domain::configuration::ContextScoutConfigurationStateV1::Active,
            tracedecay_domain::configuration::ContextScoutConfigurationStateV1::Paused
        ) | (
            tracedecay_domain::configuration::ContextScoutConfigurationStateV1::Paused,
            tracedecay_domain::configuration::ContextScoutConfigurationStateV1::Active
        )
    );
    if !valid_transition {
        return application_problem(
            wire_request_id,
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "context_scout.invalid_state_transition".to_owned(),
                message: "The Context Scout state transition is unavailable".to_owned(),
            }),
        );
    }
    settings.state = target;
    let response = execute_configuration(
        wire_request_id,
        Some(registered.clone()),
        crate::application_surface::ApplicationSurfaceOperation::ConfigurationSet,
        ConfigurationSurfaceRequest::Set(
            crate::application_surface::ConfigurationSetSurfaceRequest {
                layer: tracedecay_domain::configuration::ConfigurationLayerIdV1::Project {
                    project_id: registered.scope.project_id.clone(),
                },
                key,
                value: tracedecay_domain::configuration::ConfigurationValueV1::ContextScoutSettings(
                    settings,
                ),
                expected_revision: current.revision_id,
            },
        ),
        observed_at,
        deadline,
        cancellation,
    )
    .await;
    let DaemonInvocationResponse {
        protocol,
        revision,
        request_id,
        outcome,
    } = response;
    let DaemonInvocationOutcome::Configuration { scope, outcome } = outcome else {
        return DaemonInvocationResponse {
            protocol,
            revision,
            request_id,
            outcome,
        };
    };
    let refreshed = registered
        .runtime
        .client()
        .current()
        .await
        .ok()
        .map(
            |current| crate::application::configuration::ConfigurationCurrentStateV1 {
                revision_id: current.revision_id,
                snapshot: current.snapshot,
            },
        )
        .and_then(|current| {
            crate::agents::context_scout_ports::ContextScoutConfigurationPinV1::from_current(
                &current,
            )
        });
    if let Some(refreshed) = refreshed {
        if owner.install_state_transition(refreshed).await.is_err() {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    } else {
        return DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable);
    }
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::ContextScout { scope, outcome },
    )
}

const fn context_scout_store_outcome(
    outcome: crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1,
) -> &'static str {
    match outcome {
        crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1::Stored => "stored",
        crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1::Duplicate => {
            "duplicate"
        }
        crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1::Superseded => {
            "superseded"
        }
        crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1::Unavailable => {
            "unavailable"
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum DaemonContextScoutRuntimeRegistrationError {
    #[error("a Context Scout address registry is already mounted for this project")]
    AlreadyRegistered,
    #[error("the Context Scout address registry could not be opened")]
    InvalidProjectIdentity,
}

#[derive(Clone)]
pub(crate) struct DaemonContextScoutRuntimeRegistrar {
    service: DaemonInvocationService,
}

impl DaemonContextScoutRuntimeRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    pub(crate) async fn open_and_register(
        &self,
        database: Database,
        project_id: ProjectId,
    ) -> Result<Arc<ProjectContextScoutAddressRegistryV1>, DaemonContextScoutRuntimeRegistrationError>
    {
        let Some(registry) =
            ProjectContextScoutAddressRegistryV1::new(database, project_id.clone())
        else {
            return Err(DaemonContextScoutRuntimeRegistrationError::InvalidProjectIdentity);
        };
        let mut registries = self.service.context_scout_registries.lock().await;
        if registries.contains_key(&project_id) {
            return Err(DaemonContextScoutRuntimeRegistrationError::AlreadyRegistered);
        }
        registries.insert(project_id, Arc::clone(&registry));
        Ok(registry)
    }

    pub(crate) async fn get(
        &self,
        project_id: &ProjectId,
    ) -> Option<Arc<ProjectContextScoutAddressRegistryV1>> {
        self.service
            .context_scout_registries
            .lock()
            .await
            .get(project_id)
            .cloned()
    }
}

#[derive(Debug, Error)]
pub(crate) enum DaemonPrimitiveRuntimeRegistrationError {
    #[error("a PR12 primitive runtime is already mounted for this project")]
    AlreadyRegistered,
    #[error("the daemon project runtime registry is closed")]
    RegistryClosed,
}

/// Central project-open registration for the owned primitive facade.
#[derive(Clone)]
pub(crate) struct DaemonPrimitiveRuntimeRegistrar {
    service: DaemonInvocationService,
}

impl DaemonPrimitiveRuntimeRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    /// Retains the already-opened project runtime as its teardown owner.
    /// Scope/access were bound by the concrete project-open factory.
    pub(crate) async fn register(
        &self,
        project_root: PathBuf,
        project_runtime: Pr12PrimitiveProjectRuntime,
    ) -> Result<Arc<dyn Pr12PrimitiveDispatch>, DaemonPrimitiveRuntimeRegistrationError> {
        let dispatch = project_runtime.dispatch();
        self.service
            .project_runtimes
            .register(project_root, project_runtime)
            .await
            .map_err(|error| match error {
                ProjectRuntimeRegistryError::AlreadyRegistered => {
                    DaemonPrimitiveRuntimeRegistrationError::AlreadyRegistered
                }
                ProjectRuntimeRegistryError::Closed => {
                    DaemonPrimitiveRuntimeRegistrationError::RegistryClosed
                }
            })?;
        Ok(dispatch)
    }
}
