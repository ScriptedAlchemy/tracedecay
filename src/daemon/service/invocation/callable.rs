//! Shared callable-code context, page-admission, and response boundaries.

use super::*;

pub(super) fn callable_code_operation_kind(
    surface_operation: crate::application_surface::ApplicationSurfaceOperation,
    request: &crate::application_surface::CallableCodeSurfaceRequest,
) -> Option<CallableCodeOperationKind> {
    match (request, surface_operation) {
        (
            crate::application_surface::CallableCodeSurfaceRequest::ExactOccurrence(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeExactOccurrence,
        ) => Some(CallableCodeOperationKind::ExactOccurrence),
        (
            crate::application_surface::CallableCodeSurfaceRequest::PhraseSearch(_),
            crate::application_surface::ApplicationSurfaceOperation::CodePhraseSearch,
        ) => Some(CallableCodeOperationKind::PhraseSearch),
        (
            crate::application_surface::CallableCodeSurfaceRequest::Callees(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeCallees,
        ) => Some(CallableCodeOperationKind::Callees),
        (
            crate::application_surface::CallableCodeSurfaceRequest::Facets(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeFacets,
        ) => Some(CallableCodeOperationKind::Facets),
        (
            crate::application_surface::CallableCodeSurfaceRequest::Timeline(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeTimeline,
        ) => Some(CallableCodeOperationKind::Timeline),
        (
            crate::application_surface::CallableCodeSurfaceRequest::Declaration(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeDeclaration,
        ) => Some(CallableCodeOperationKind::Declaration),
        (
            crate::application_surface::CallableCodeSurfaceRequest::Definition(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeDefinition,
        ) => Some(CallableCodeOperationKind::Definition),
        (
            crate::application_surface::CallableCodeSurfaceRequest::TypeDefinition(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeTypeDefinition,
        ) => Some(CallableCodeOperationKind::TypeDefinition),
        (
            crate::application_surface::CallableCodeSurfaceRequest::References(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeReferences,
        ) => Some(CallableCodeOperationKind::References),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn admit_callable_page(
    service: &DaemonInvocationService,
    binding_id: BindingId,
    operation: ApplicationWireOperation,
    context: &RequestContext,
    observed_at: UtcMicros,
    body_digest: ManifestDigest,
    page: PageRequest,
) -> Result<PageRequest, PageAdmissionError> {
    let request = PageAdmissionRequest::new(
        binding_id,
        operation,
        context.clone(),
        observed_at,
        context.scope().scope_digest.clone(),
        body_digest,
        page,
    )?;
    PageAdmissionService::new(service.code_index_schedulers.clone())
        .admit(request)
        .await
        .map(|admitted| admitted.into_page())
}

pub(super) fn callable_page_admission_problem(
    wire_request_id: String,
    error: PageAdmissionError,
) -> DaemonInvocationResponse {
    let diagnostic = |code: &'static str, message: &'static str| SafeDiagnostic {
        code: code.to_owned(),
        message: message.to_owned(),
    };
    let problem = match error {
        PageAdmissionError::Denied | PageAdmissionError::BindingMismatch => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        PageAdmissionError::Stale => ApplicationProblem::stale(diagnostic(
            "callable_code.page_stale",
            "The callable code continuation is stale",
        )),
        PageAdmissionError::Unavailable => ApplicationProblem::unavailable(diagnostic(
            "callable_code.page_unavailable",
            "The callable code continuation authority is unavailable",
        )),
        PageAdmissionError::InvalidRequest => ApplicationProblem::InvalidRequest {
            diagnostic: diagnostic(
                "callable_code.page_invalid",
                "The callable code page request is invalid",
            ),
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        },
        PageAdmissionError::Unsupported => ApplicationProblem::Unsupported {
            diagnostic: diagnostic(
                "callable_code.page_unsupported",
                "The callable code operation does not support page admission",
            ),
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        },
    };
    application_problem(wire_request_id, problem)
}

pub(super) fn invalid_callable_code_request(wire_request_id: String) -> DaemonInvocationResponse {
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

pub(super) fn callable_code_response<T: Serialize>(
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
