use tracedecay_application::{
    ApplicationEnvelope, ApplicationProblem, ApplicationProblemEnvelope, ApplicationResult,
    RequestId, ResultContractRef, RetryDirective, SafeDiagnostic,
};

use tracedecay_daemon_protocol::{DaemonInvocationOutcome, DaemonInvocationProblem};
use tracedecay_domain::errors::{Result, TraceDecayError};

fn retained_contract_error(
    context: &'static str,
    error: tracedecay_application::ApplicationContractError,
) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("{context}: {error}"),
    }
}

pub fn retained_safe_diagnostic(
    code: &'static str,
    message: &'static str,
) -> Result<SafeDiagnostic> {
    SafeDiagnostic::new(code, message)
        .map_err(|error| retained_contract_error("invalid retained application diagnostic", error))
}

pub fn retained_problem_envelope(
    contract: ResultContractRef,
    request_id: RequestId,
    problem: ApplicationProblem,
) -> Result<ApplicationProblemEnvelope> {
    ApplicationProblemEnvelope::new(contract, request_id, problem).map_err(|error| {
        retained_contract_error("invalid retained application problem envelope", error)
    })
}

#[hotpath::measure(label = "mcp.retained.response_validate")]
pub fn validated_retained_response(
    outcome: DaemonInvocationOutcome,
    operation: tracedecay_application::RetainedSurfaceOperation,
    request_id: &RequestId,
    result_contract: &ResultContractRef,
) -> Result<ApplicationResult<tracedecay_application::retained_surfaces::RetainedSurfaceResultV1>> {
    match outcome {
        DaemonInvocationOutcome::RetainedApplication { scope, outcome }
            if tracedecay_application::retained_surface_outcome_matches_terminal(
                operation, request_id, &scope, &outcome,
            ) =>
        {
            Ok(Ok(ApplicationEnvelope {
                contract: result_contract.clone(),
                request_id: request_id.clone(),
                scope,
                outcome,
            }))
        }
        DaemonInvocationOutcome::RetainedApplicationProblem { scope, problem }
            if tracedecay_application::retained_surface_problem_matches_terminal(
                operation,
                request_id,
                Some(&scope),
                &problem,
            ) =>
        {
            Ok(Err(retained_problem_envelope(
                result_contract.clone(),
                request_id.clone(),
                problem,
            )?))
        }
        DaemonInvocationOutcome::ApplicationProblem { problem }
            if tracedecay_application::retained_surface_problem_matches_terminal(
                operation, request_id, None, &problem,
            ) =>
        {
            Ok(Err(retained_problem_envelope(
                result_contract.clone(),
                request_id.clone(),
                problem,
            )?))
        }
        DaemonInvocationOutcome::Problem { problem } => {
            let problem = invocation_problem(problem)?;
            Ok(Err(retained_problem_envelope(
                result_contract.clone(),
                request_id.clone(),
                problem,
            )?))
        }
        _ => Ok(Err(retained_problem_envelope(
            result_contract.clone(),
            request_id.clone(),
            ApplicationProblem::unavailable(retained_safe_diagnostic(
                "application.surface.invalid_response",
                "The daemon returned an invalid retained application response",
            )?),
        )?)),
    }
}

fn invocation_problem(problem: DaemonInvocationProblem) -> Result<ApplicationProblem> {
    Ok(match problem {
        DaemonInvocationProblem::InvalidRequest | DaemonInvocationProblem::UnsupportedRevision => {
            ApplicationProblem::InvalidRequest {
                diagnostic: retained_safe_diagnostic(
                    "application.surface.invalid_request",
                    "The daemon rejected the retained application request",
                )?,
                retry: RetryDirective::Never,
                legal_actions: Vec::new(),
            }
        }
        DaemonInvocationProblem::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        DaemonInvocationProblem::ResetRequired => {
            ApplicationProblem::reset_required(retained_safe_diagnostic(
                "application.surface.reset_required",
                "The retained application store requires an explicit reset",
            )?)
        }
        DaemonInvocationProblem::ApplicationContractViolation => {
            ApplicationProblem::unavailable(retained_safe_diagnostic(
                "application.surface.contract_violation",
                "The retained application result violated its canonical contract",
            )?)
        }
        DaemonInvocationProblem::Unavailable => {
            ApplicationProblem::unavailable(retained_safe_diagnostic(
                "application.surface.unavailable",
                "The retained application service is unavailable",
            )?)
        }
    })
}
