use tracedecay_application::{ApplicationProblem, ResolvedScope};

use super::{
    DAEMON_INVOCATION_PROTOCOL, DAEMON_INVOCATION_REVISION, DaemonInvocationOutcome,
    DaemonInvocationProblem, DaemonInvocationResponse,
};

impl DaemonInvocationResponse {
    pub(crate) fn problem(request_id: impl Into<String>, problem: DaemonInvocationProblem) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            outcome: DaemonInvocationOutcome::Problem { problem },
        }
    }

    pub(crate) fn application_problem(
        request_id: impl Into<String>,
        problem: ApplicationProblem,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            outcome: DaemonInvocationOutcome::ApplicationProblem { problem },
        }
    }

    pub(crate) fn retained_application_problem(
        request_id: impl Into<String>,
        scope: ResolvedScope,
        problem: ApplicationProblem,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            outcome: DaemonInvocationOutcome::RetainedApplicationProblem { scope, problem },
        }
    }
}
