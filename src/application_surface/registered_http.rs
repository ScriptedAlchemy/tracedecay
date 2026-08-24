use tracedecay_application::{ApplicationProblem, RequestId, SafeDiagnostic};

use crate::daemon_client::DaemonInvocationError;

pub(crate) trait RegisteredHttpOperation: Copy {
    fn operation_id(self) -> String;
    fn is_read_only(self) -> bool;
    fn problem_family(self) -> &'static str;
    fn display_family(self) -> &'static str;
    fn application_problem_is_bound(
        self,
        _request_id: &RequestId,
        scope: Option<&tracedecay_application::ResolvedScope>,
        _problem: &ApplicationProblem,
    ) -> bool {
        scope.is_none()
    }
    fn registry(
        self,
    ) -> Result<
        tracedecay_tool_catalog::ExecutableBindingRegistryV1,
        super::ApplicationSurfaceAdapterError,
    >;
}

pub(super) fn validated_daemon_outcome<O>(
    operation: O,
    request_id: &RequestId,
    response: Result<crate::daemon_contract::DaemonInvocationResponse, DaemonInvocationError>,
) -> Result<crate::daemon_contract::DaemonInvocationOutcome, ApplicationProblem>
where
    O: RegisteredHttpOperation,
{
    let problem_code = |suffix: &str| format!("{}.{}", operation.problem_family(), suffix);
    let family = operation.display_family();
    match response {
        Ok(response)
            if response.protocol == crate::daemon_contract::DAEMON_INVOCATION_PROTOCOL
                && response.revision == crate::daemon_contract::DAEMON_INVOCATION_REVISION
                && response.request_id == request_id.as_str() =>
        {
            let problem_is_bound = match &response.outcome {
                crate::daemon_contract::DaemonInvocationOutcome::ApplicationProblem { problem } => {
                    operation.application_problem_is_bound(request_id, None, problem)
                }
                crate::daemon_contract::DaemonInvocationOutcome::RetainedApplicationProblem {
                    scope,
                    problem,
                } => operation.application_problem_is_bound(request_id, Some(scope), problem),
                _ => true,
            };
            if !problem_is_bound {
                return Err(ApplicationProblem::unavailable(SafeDiagnostic {
                    code: problem_code("invalid_terminal"),
                    message: format!("The {family} daemon returned an unbound terminal"),
                }));
            }
            Ok(response.outcome)
        }
        Ok(_) => Err(ApplicationProblem::unavailable(SafeDiagnostic {
            code: problem_code("invalid_envelope"),
            message: format!("The {family} daemon returned an invalid response envelope"),
        })),
        Err(DaemonInvocationError::Cancelled { stage }) => Err(ApplicationProblem::Cancelled {
            stage,
            retry: tracedecay_application::RetryDirective::Never,
            legal_actions: Vec::new(),
        }),
        Err(DaemonInvocationError::TimedOut { stage }) => Err(ApplicationProblem::TimedOut {
            stage,
            retry: tracedecay_application::RetryDirective::Never,
            legal_actions: Vec::new(),
        }),
        Err(DaemonInvocationError::Unavailable) => {
            Err(ApplicationProblem::unavailable(SafeDiagnostic {
                code: problem_code("transport_unavailable"),
                message: format!("The {family} application transport is unavailable"),
            }))
        }
    }
}

#[cfg(test)]
#[path = "retained_http_identity_tests.rs"]
mod tests;
