use tracedecay_domain::UtcMicros;
use tracedecay_policy::authorization::SourceAuthorizationInputV1;

use crate::context::RequestContext;
use crate::handlers::ApplicationOperation;
use crate::result::SafeDiagnostic;

/// Operation boundary at which authorization is evaluated or rechecked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AuthorizationPhase {
    Admission,
    PageExpansion,
    Hydration,
    Publication,
    Effect,
}

/// Typed authorization input. Ports receive no transport-origin authority.
#[derive(Clone, Copy, Debug)]
pub struct AuthorizationRequest<'a> {
    pub context: &'a RequestContext,
    pub operation: &'a ApplicationOperation,
    pub phase: AuthorizationPhase,
    pub observed_at: UtcMicros,
}

/// Immutable source-policy facts loaded by the application boundary.
///
/// The source visibility bit is used only to apply the policy crate's public
/// non-disclosure projection. It is never treated as authorization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceAuthorizationSnapshot {
    input: SourceAuthorizationInputV1,
    source_visible: bool,
}

impl SourceAuthorizationSnapshot {
    pub fn new(input: SourceAuthorizationInputV1, source_visible: bool) -> Self {
        Self {
            input,
            source_visible,
        }
    }

    pub fn input(&self) -> &SourceAuthorizationInputV1 {
        &self.input
    }

    pub const fn source_visible(&self) -> bool {
        self.source_visible
    }
}

/// Snapshot-loading result supplied by a policy/configuration authority.
///
/// A port supplies immutable facts only. It never returns a policy decision,
/// receipt, or proof, so application code cannot reconstruct authority from a
/// [`crate::result::PolicyDecisionRef`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthorizationPortOutcome {
    Snapshot(Box<SourceAuthorizationSnapshot>),
    Absent,
    Unavailable(SafeDiagnostic),
    Stale(SafeDiagnostic),
}

/// Narrow port for current policy/configuration snapshots. The approved
/// [`tracedecay_policy::authorization::SourceAuthorizationEvaluator`] evaluates
/// the returned input inside [`super::AuthorizationService`].
pub trait AuthorizationPort {
    fn source_authorization_snapshot(
        &self,
        request: &AuthorizationRequest<'_>,
    ) -> AuthorizationPortOutcome;
}
