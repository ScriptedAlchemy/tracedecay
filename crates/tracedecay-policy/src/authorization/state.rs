use serde::{Deserialize, Serialize};

/// One exhaustive policy disposition. This conveys the evaluator result; it
/// does not itself authorize an application effect.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SourceAuthorizationDispositionV1 {
    Allow,
    Deny,
    Abstain,
    NotApplicable,
    Indeterminate,
}

/// Access and content remain independent axes. In particular,
/// `AuthoritativeDeleted` is never synthesized from an authorization failure.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SourceAccessDecisionV1 {
    Authorized,
    PolicyExcluded,
    Unauthorized,
}

/// The only public shape for resource-addressed hidden, absent, wrong-owner,
/// or unauthorized results. It intentionally has no payload fields.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PublicSourceResultShapeV1 {
    NotFoundOrNotAuthorized,
    PolicyExcluded,
    Live,
    Partial,
    TemporarilyUnavailable,
    AuthoritativeDeleted,
}
