//! PR12 primitive, callable-code, and context-scout daemon invocation handlers.

use super::*;

mod page_admission;

const fn page_admission_problem(
    error: tracedecay_application::PageAdmissionError,
) -> DaemonInvocationProblem {
    match error {
        tracedecay_application::PageAdmissionError::Denied
        | tracedecay_application::PageAdmissionError::BindingMismatch => {
            DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
        tracedecay_application::PageAdmissionError::Stale
        | tracedecay_application::PageAdmissionError::Unavailable => {
            DaemonInvocationProblem::Unavailable
        }
        tracedecay_application::PageAdmissionError::InvalidRequest
        | tracedecay_application::PageAdmissionError::Unsupported => {
            DaemonInvocationProblem::InvalidRequest
        }
    }
}

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
    if let Err(error) = page_admission::admit_primitive_page(
        Arc::clone(&dispatch),
        surface_operation,
        &request,
        &context,
        observed_at,
    )
    .await
    {
        return DaemonInvocationResponse::problem(wire_request_id, page_admission_problem(error));
    }
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
    binding_id: BindingId,
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
    let Some(kind) = callable_code_operation_kind(surface_operation, &request) else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::InvalidRequest,
        );
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
    macro_rules! admit_page_or_return {
        ($request:expr, $kind:expr, $operation:expr, $digest:expr) => {{
            if $request.validate().is_err() {
                return invalid_callable_code_request(wire_request_id);
            }
            let authorization_admission = match query
                .admit_authorization(&context, $kind, observed_at)
                .await
            {
                Ok(admission) => admission,
                Err(problem) => return application_problem(wire_request_id, problem),
            };
            let Ok(body_digest) = $digest else {
                return invalid_callable_code_request(wire_request_id);
            };
            match admit_callable_page(
                service,
                binding_id.clone(),
                $operation,
                &context,
                observed_at,
                body_digest,
                $request.meta.page.clone(),
            )
            .await
            {
                Ok(page) => $request.meta.page = page,
                Err(error) => {
                    return callable_page_admission_problem(wire_request_id, error);
                }
            }
            authorization_admission
        }};
    }
    match request {
        crate::application_surface::CallableCodeSurfaceRequest::ExactOccurrence(request) => {
            let Ok(mut request) = request.into_application_request(page) else {
                return invalid_callable_code_request(wire_request_id);
            };
            let authorization_admission = admit_page_or_return!(
                request,
                kind,
                ApplicationWireOperation::CodeExactOccurrence,
                crate::daemon::code_index_scheduler::callable_page_binding::exact_occurrence_page_body_digest(
                    &request,
                )
            );
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query
                    .exact_occurrence_with_admission(
                        &context,
                        request,
                        observed_at,
                        authorization_admission,
                    )
                    .await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::PhraseSearch(request) => {
            let Ok(mut request) = request.into_application_request(
                crate::daemon::code_index_scheduler::queries::callable_query_sanitizer_revision(),
                crate::daemon::code_index_scheduler::queries::callable_query_normalization_revision(
                ),
                page,
            ) else {
                return invalid_callable_code_request(wire_request_id);
            };
            let authorization_admission = admit_page_or_return!(
                request,
                kind,
                ApplicationWireOperation::CodePhraseSearch,
                crate::daemon::code_index_scheduler::callable_page_binding::phrase_search_page_body_digest(
                    &request,
                )
            );
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query
                    .phrase_search_with_admission(
                        &context,
                        request,
                        observed_at,
                        authorization_admission,
                    )
                    .await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::Callees(request) => {
            let mut request = request.into_application_request(page);
            let authorization_admission = admit_page_or_return!(
                request,
                kind,
                ApplicationWireOperation::CodeCallees,
                crate::daemon::code_index_scheduler::callable_page_binding::callees_page_body_digest(&request)
            );
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query
                    .callees_with_admission(&context, request, observed_at, authorization_admission)
                    .await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::Facets(request) => {
            let mut request = request.into_application_request(page);
            let authorization_admission = admit_page_or_return!(
                request,
                kind,
                ApplicationWireOperation::CodeFacets,
                crate::daemon::code_index_scheduler::callable_page_binding::facets_page_body_digest(
                    &request
                )
            );
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query
                    .facets_with_admission(&context, request, observed_at, authorization_admission)
                    .await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::Timeline(request) => {
            let mut request = request.into_application_request(page);
            let authorization_admission = admit_page_or_return!(
                request,
                kind,
                ApplicationWireOperation::CodeTimeline,
                crate::daemon::code_index_scheduler::callable_page_binding::timeline_page_body_digest(&request)
            );
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query
                    .timeline_with_admission(
                        &context,
                        request,
                        observed_at,
                        authorization_admission,
                    )
                    .await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::Declaration(request) => {
            let mut request = request.into_application_request(page);
            let authorization_admission = admit_page_or_return!(
                request,
                kind,
                ApplicationWireOperation::CodeDeclaration,
                crate::daemon::code_index_scheduler::callable_page_binding::navigation_page_body_digest(
                    "code_declaration",
                    &request,
                )
            );
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query
                    .declaration_with_admission(
                        &context,
                        request,
                        observed_at,
                        authorization_admission,
                    )
                    .await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::Definition(request) => {
            let mut request = request.into_application_request(page);
            let authorization_admission = admit_page_or_return!(
                request,
                kind,
                ApplicationWireOperation::CodeDefinition,
                crate::daemon::code_index_scheduler::callable_page_binding::navigation_page_body_digest(
                    "code_definition",
                    &request,
                )
            );
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query
                    .definition_with_admission(
                        &context,
                        request,
                        observed_at,
                        authorization_admission,
                    )
                    .await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::TypeDefinition(request) => {
            let mut request = request.into_application_request(page);
            let authorization_admission = admit_page_or_return!(
                request,
                kind,
                ApplicationWireOperation::CodeTypeDefinition,
                crate::daemon::code_index_scheduler::callable_page_binding::navigation_page_body_digest(
                    "code_type_definition",
                    &request,
                )
            );
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query
                    .type_definition_with_admission(
                        &context,
                        request,
                        observed_at,
                        authorization_admission,
                    )
                    .await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::References(request) => {
            let mut request = request.into_application_request(page);
            let authorization_admission = admit_page_or_return!(
                request,
                kind,
                ApplicationWireOperation::CodeReferences,
                crate::daemon::code_index_scheduler::callable_page_binding::navigation_page_body_digest(
                    "code_references",
                    &request,
                )
            );
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query
                    .references_with_admission(
                        &context,
                        request,
                        observed_at,
                        authorization_admission,
                    )
                    .await,
            )
        }
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
