//! Canonical problem envelopes for application-surface refusals and invocation failures.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use tracedecay_contracts::{
    ApplicationContractError, ApplicationProblem, ApplicationProblemEnvelope, LegalAction,
    ProblemOwningLayer, RequestId, ResultContractRef, RetryDirective, SafeDiagnostic,
};
use tracedecay_daemon_protocol::{
    ApplicationSurfaceAdapterError, CatalogBindingResolver, DispatchError,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::{ApplicationSurfaceOperation, BindingSurface, SchemaId};

use super::catalog::{application_surface_catalog_ref, resolve_application_binding};

/// Refuse a registered request that never reached dispatch in the canonical envelope.
///
/// Everything before the executor call is adapter territory: the catalog would
/// not build, the operation is not advertised, or its binding carries no public
/// route. A bare status here would answer a registered route with an empty body no
/// client can read a code, a retry directive or a request id out of, so these
/// failures are reported as the same `ApplicationProblemEnvelope` the dispatched
/// path returns, owned by the adapter layer rather than the runtime.
pub(super) fn registered_adapter_unavailable(
    request_id: RequestId,
    code: &str,
    message: &str,
) -> Response {
    let Ok(schema_id) = SchemaId::new("schema.tracedecay.http.adapter-problem.v1") else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(contract) = ResultContractRef::new(schema_id, 1) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match ApplicationProblemEnvelope::new(
        contract,
        request_id,
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: code.to_owned(),
            message: message.to_owned(),
        }),
    ) {
        Ok(problem) => tracedecay_api::application_problem_response(
            problem.with_owning_layer(ProblemOwningLayer::Adapter),
        ),
        Err(error) => application_contract_error_response(error),
    }
}

pub(super) fn application_contract_error_response(error: ApplicationContractError) -> Response {
    tracing::error!(%error, "application problem envelope violated its canonical contract");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

pub(super) fn http_adapter_problem(
    contract: ResultContractRef,
    request_id: RequestId,
    error: ApplicationSurfaceAdapterError,
) -> std::result::Result<ApplicationProblemEnvelope, ApplicationContractError> {
    let problem = match error {
        ApplicationSurfaceAdapterError::UnknownOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        ApplicationSurfaceAdapterError::InvalidRequestHandle
        | ApplicationSurfaceAdapterError::InvalidSurfaceRequest => {
            ApplicationProblem::InvalidRequest {
                diagnostic: SafeDiagnostic {
                    code: "application.surface.invalid_request".to_owned(),
                    message: "The application request is invalid".to_owned(),
                },
                retry: RetryDirective::Never,
                legal_actions: Vec::new(),
            }
        }
        // Catalog composition is derived from `const` application specs, so
        // these failures are deterministic for the lifetime of the process.
        // Reporting them as `unavailable` told clients to retry a request that
        // can never succeed.
        ApplicationSurfaceAdapterError::Catalog(_)
        | ApplicationSurfaceAdapterError::Contract(_)
        | ApplicationSurfaceAdapterError::Identifier(_)
        | ApplicationSurfaceAdapterError::CatalogValidation(_) => ApplicationProblem::Unsupported {
            diagnostic: SafeDiagnostic {
                code: "application.surface.catalog_unavailable".to_owned(),
                message: "The application catalog for this operation could not be composed"
                    .to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::ContactAdministrator],
        },
        // Genuinely transient: the owning daemon transport is not answering.
        ApplicationSurfaceAdapterError::DaemonUnavailable => {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "application.surface.unavailable".to_owned(),
                message: "The application service for this operation is unavailable".to_owned(),
            })
        }
        // Also transient, with the connect diagnostic preserved: no daemon
        // accepted the connection, so the request was never sent.
        ApplicationSurfaceAdapterError::DaemonUnreachable {
            reason_code,
            detail,
        } => ApplicationProblem::unavailable(SafeDiagnostic {
            code: reason_code,
            message: detail,
        }),
    };
    ApplicationProblemEnvelope::new(contract, request_id, problem)
        .map(|problem| problem.with_owning_layer(ProblemOwningLayer::Adapter))
}

/// The canonical typed terminal for an MCP `tools/call` whose project open
/// was refused because the store requires an explicit reset.
///
/// The refusal settles before any project server exists, so the MCP boundary
/// cannot route the call to its handler; the truthful answer for the named
/// operation is the reset-required terminal under its own mounted MCP result
/// contract. Returns `None` for tools without a mounted application binding.
pub fn mcp_project_open_reset_refusal(
    tool_name: &str,
    request_id: RequestId,
    authority: &str,
    reason: &str,
) -> Option<ApplicationProblemEnvelope> {
    let operation = ApplicationSurfaceOperation::from_tool_name(tool_name)?;
    let catalog = application_surface_catalog_ref().ok()?;
    let resolver = CatalogBindingResolver::new(catalog);
    let binding = resolve_application_binding(&resolver, BindingSurface::Mcp, operation)?;
    let contract = ResultContractRef::from_schema(&binding.result_schema);
    let problem = ApplicationProblem::reset_required(SafeDiagnostic {
        code: "application.surface.reset_required".to_owned(),
        message: format!("The {authority} requires an explicit reset: {reason}"),
    });
    ApplicationProblemEnvelope::new(contract, request_id, problem)
        .ok()
        .map(|envelope| envelope.with_owning_layer(ProblemOwningLayer::Runtime))
}

pub(crate) fn current_micros() -> Result<UtcMicros, ApplicationSurfaceAdapterError> {
    tracedecay_contracts::clock::try_now_micros()
        .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
}

pub(super) fn invocation_problem(
    problem: tracedecay_daemon_protocol::DaemonInvocationProblem,
) -> Result<ApplicationProblem, ApplicationSurfaceAdapterError> {
    Ok(match problem {
        tracedecay_daemon_protocol::DaemonInvocationProblem::InvalidRequest
        | tracedecay_daemon_protocol::DaemonInvocationProblem::UnsupportedRevision => {
            ApplicationProblem::InvalidRequest {
                diagnostic: SafeDiagnostic::new(
                    "application.surface.invalid_request",
                    "The daemon rejected the application request",
                )?,
                retry: RetryDirective::Never,
                legal_actions: Vec::new(),
            }
        }
        tracedecay_daemon_protocol::DaemonInvocationProblem::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        tracedecay_daemon_protocol::DaemonInvocationProblem::ResetRequired => {
            ApplicationProblem::reset_required(SafeDiagnostic::new(
                "application.surface.reset_required",
                "The application store requires an explicit reset",
            )?)
        }
        tracedecay_daemon_protocol::DaemonInvocationProblem::ApplicationContractViolation => {
            ApplicationProblem::unavailable(SafeDiagnostic::new(
                "application.surface.contract_violation",
                "The application result violated its canonical contract",
            )?)
        }
        tracedecay_daemon_protocol::DaemonInvocationProblem::Unavailable => {
            ApplicationProblem::unavailable(SafeDiagnostic::new(
                "application.surface.unavailable",
                "The application service for this operation is unavailable",
            )?)
        }
    })
}

pub(super) fn invocation_contract_problem(
    error: tracedecay_contracts::InvocationError,
) -> Result<ApplicationProblem, ApplicationSurfaceAdapterError> {
    Ok(match error {
        tracedecay_contracts::InvocationError::Denied => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        tracedecay_contracts::InvocationError::Cancelled => {
            ApplicationProblem::cancelled_before_admission()
        }
        tracedecay_contracts::InvocationError::DeadlineExceeded => {
            ApplicationProblem::timed_out_before_admission()
        }
        tracedecay_contracts::InvocationError::InvalidRequest => {
            ApplicationProblem::InvalidRequest {
                diagnostic: SafeDiagnostic::new(
                    "application.surface.invalid_request",
                    "The daemon rejected the application request",
                )?,
                retry: RetryDirective::Never,
                legal_actions: Vec::new(),
            }
        }
        tracedecay_contracts::InvocationError::Conflict => ApplicationProblem::Conflict {
            diagnostic: SafeDiagnostic::new(
                "application.surface.conflict",
                "The application request conflicts with current state",
            )?,
            retry: RetryDirective::AfterRevalidate,
            legal_actions: vec![LegalAction::Refresh],
        },
        tracedecay_contracts::InvocationError::Unavailable => {
            ApplicationProblem::unavailable(SafeDiagnostic::new(
                "application.surface.unavailable",
                "The application service for this operation is unavailable",
            )?)
        }
        // Dispatchers intercept unreachable before republishing problems; this
        // projection keeps the connect diagnostic for any caller that still
        // renders it as a problem.
        tracedecay_contracts::InvocationError::Unreachable {
            reason_code,
            detail,
        } => ApplicationProblem::unavailable(SafeDiagnostic {
            code: reason_code,
            message: detail,
        }),
        // The daemon's typed problem is the authority; republishing it keeps
        // its diagnostic (e.g. `configuration.conflict`) intact instead of
        // substituting a generic surface code.
        tracedecay_contracts::InvocationError::Problem(problem) => *problem,
    })
}

pub fn map_dispatch_error(error: DispatchError) -> ApplicationSurfaceAdapterError {
    match error {
        DispatchError::UnknownOrNotAuthorized => {
            ApplicationSurfaceAdapterError::UnknownOrNotAuthorized
        }
    }
}
