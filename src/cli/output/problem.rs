//! Non-disclosing problem helpers shared by CLI presenters.

use tracedecay_application::{ApplicationProblem, ApplicationProblemKind};

#[allow(dead_code)] // Plan 21 CLI problem output — staged
pub fn concealed_not_found_or_not_authorized() -> ApplicationProblem {
    tracedecay::daemon_client::concealed_not_found_or_not_authorized()
}

/// Stable semantic exit class. Numeric process-code policy is selected by the
/// CLI entry point; it must not collapse these application states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Plan 21 CLI exit-class — staged
pub enum CliExitClass {
    InvalidRequest,
    NotFoundOrNotAuthorized,
    Conflict,
    Stale,
    Unsupported,
    Unavailable,
    Saturated,
    Cancelled,
    TimedOut,
}

/// Exhaustive application-problem to CLI exit-class mapping.
#[allow(dead_code)] // Plan 21 CLI exit-class — staged
pub fn exit_class(problem: &ApplicationProblem) -> CliExitClass {
    match problem.kind() {
        ApplicationProblemKind::InvalidRequest => CliExitClass::InvalidRequest,
        ApplicationProblemKind::NotFoundOrNotAuthorized => CliExitClass::NotFoundOrNotAuthorized,
        ApplicationProblemKind::Conflict => CliExitClass::Conflict,
        ApplicationProblemKind::Stale => CliExitClass::Stale,
        ApplicationProblemKind::Unsupported => CliExitClass::Unsupported,
        ApplicationProblemKind::Unavailable => CliExitClass::Unavailable,
        ApplicationProblemKind::Saturated => CliExitClass::Saturated,
        ApplicationProblemKind::Cancelled => CliExitClass::Cancelled,
        ApplicationProblemKind::TimedOut => CliExitClass::TimedOut,
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_application::{ApplicationProblem, ApplicationProblemKind, RetryDirective};

    use super::{CliExitClass, concealed_not_found_or_not_authorized, exit_class};

    #[test]
    fn concealed_rejection_has_no_diagnostic_or_legal_action() {
        let problem = concealed_not_found_or_not_authorized();

        assert_eq!(
            problem.kind(),
            ApplicationProblemKind::NotFoundOrNotAuthorized
        );
        assert_eq!(exit_class(&problem), CliExitClass::NotFoundOrNotAuthorized);
        assert!(matches!(
            problem,
            ApplicationProblem::NotFoundOrNotAuthorized {
                retry: RetryDirective::Never,
                legal_actions,
            } if legal_actions.is_empty()
        ));
    }
}
