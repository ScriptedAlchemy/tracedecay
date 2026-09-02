use tracedecay_policy::authorization::PublicSourceResultShapeV1;

use crate::result::{ApplicationProblem, RetryDirective, SafeDiagnostic};

/// Internal causes intentionally collapsed before any application response is
/// constructed for a resource-addressed operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConcealedResourceCause {
    Absent,
    OutsideScope,
    PolicyHidden,
}

/// Central non-disclosure hooks for resource lookup, cursor resume, and anchor
/// expansion. All exposed paths preserve the same public problem shape.
#[derive(Clone, Copy, Debug, Default)]
pub struct NonDisclosureHooks;

impl NonDisclosureHooks {
    pub fn resource_problem(
        &self,
        _cause: ConcealedResourceCause,
        retry: RetryDirective,
    ) -> ApplicationProblem {
        ApplicationProblem::not_found_or_not_authorized(retry)
    }

    pub fn cursor_problem(&self, retry: RetryDirective) -> ApplicationProblem {
        ApplicationProblem::not_found_or_not_authorized(retry)
    }

    pub fn anchor_problem(&self, retry: RetryDirective) -> ApplicationProblem {
        ApplicationProblem::not_found_or_not_authorized(retry)
    }

    /// Convert a policy public shape into the application problem permitted at
    /// an authorization boundary. `Live` and `Partial` only reach this hook
    /// when a proof could not be verified, so they remain concealed too.
    pub fn source_problem(&self, shape: PublicSourceResultShapeV1) -> ApplicationProblem {
        match shape {
            PublicSourceResultShapeV1::NotFoundOrNotAuthorized
            | PublicSourceResultShapeV1::Live
            | PublicSourceResultShapeV1::Partial
            | PublicSourceResultShapeV1::AuthoritativeDeleted => {
                self.resource_problem(ConcealedResourceCause::PolicyHidden, RetryDirective::Never)
            }
            PublicSourceResultShapeV1::PolicyExcluded => ApplicationProblem::Unsupported {
                diagnostic: SafeDiagnostic::new(
                    "application.authorization.policy-excluded",
                    "The requested operation is not available.",
                )
                .expect("static safe diagnostic is valid"),
                retry: RetryDirective::Never,
                legal_actions: Vec::new(),
            },
            PublicSourceResultShapeV1::TemporarilyUnavailable => ApplicationProblem::unavailable(
                SafeDiagnostic::new(
                    "application.authorization.source-unavailable",
                    "The requested resource is temporarily unavailable.",
                )
                .expect("static safe diagnostic is valid"),
            ),
        }
    }

    pub fn stale_policy_problem(&self) -> ApplicationProblem {
        ApplicationProblem::stale(
            SafeDiagnostic::new(
                "application.authorization.policy-stale",
                "Authorization information must be refreshed.",
            )
            .expect("static safe diagnostic is valid"),
        )
    }

    pub fn proof_problem(&self) -> ApplicationProblem {
        ApplicationProblem::unavailable(
            SafeDiagnostic::new(
                "application.authorization.proof-invalid",
                "The authorization proof could not be verified.",
            )
            .expect("static safe diagnostic is valid"),
        )
    }
}
