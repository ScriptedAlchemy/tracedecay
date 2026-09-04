use tracedecay_application::{ApplicationProblem, RequestId, SafeDiagnostic};

use tracedecay_daemon_protocol::DaemonInvocationError;

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
        std::borrow::Cow<'static, tracedecay_tool_catalog::ExecutableBindingRegistryV1>,
        super::ApplicationSurfaceAdapterError,
    >;
}

#[hotpath::measure(label = "application_surface.registered.validate_outcome")]
pub(super) fn validated_daemon_outcome<O>(
    operation: O,
    request_id: &RequestId,
    response: Result<tracedecay_daemon_protocol::DaemonInvocationResponse, DaemonInvocationError>,
) -> Result<tracedecay_daemon_protocol::DaemonInvocationOutcome, ApplicationProblem>
where
    O: RegisteredHttpOperation,
{
    let problem_code = |suffix: &str| format!("{}.{}", operation.problem_family(), suffix);
    let family = operation.display_family();
    match response {
        Ok(response)
            if response.protocol == tracedecay_daemon_protocol::DAEMON_INVOCATION_PROTOCOL
                && response.revision == tracedecay_daemon_protocol::DAEMON_INVOCATION_REVISION
                && response.request_id == request_id.as_str() =>
        {
            let problem_is_bound = match &response.outcome {
                tracedecay_daemon_protocol::DaemonInvocationOutcome::ApplicationProblem { problem } => {
                    operation.application_problem_is_bound(request_id, None, problem)
                }
                tracedecay_daemon_protocol::DaemonInvocationOutcome::RetainedApplicationProblem {
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
        // Registered HTTP handlers run inside the daemon process, so a
        // connect-phase failure cannot occur here; the projection still keeps
        // the connect diagnostic truthful for completeness.
        Err(DaemonInvocationError::Unreachable {
            reason_code,
            detail,
        }) => Err(ApplicationProblem::unavailable(SafeDiagnostic {
            code: reason_code,
            message: detail,
        })),
    }
}

#[cfg(test)]
#[path = "retained_http_identity_tests.rs"]
mod tests;
