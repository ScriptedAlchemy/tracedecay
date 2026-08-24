//! External-source authorization kernel.
//!
//! The state transition is intentionally one-way:
//! `input -> decision -> source proof -> sink recheck -> admission proof`.
//! Constructors for proofs are private to this module's transition functions.

mod decision;
mod grant;
mod input;
mod intersection;
mod recheck;
mod state;

pub(crate) use input::policy_digest;

pub use decision::{
    PolicyEvaluatorVersionV1, PolicyReasonCodeV1, SourceAuthorizationDecisionV1,
    SourceAuthorizationEvaluator, SourceAuthorizationEvaluatorV1,
    SourceAuthorizationExpectedDecisionV1, SourceAuthorizationTruthTableV1,
    public_source_result_shape,
};
pub use grant::{CapabilityGrantV1, GrantStateV1};
pub use input::{
    AuthorizationCoverageV1, AuthorizationSnapshotStateV1, BudgetSetV1, DisclosureClassV1,
    ExternalContentStatusV1, GrantIdV1, PolicyIdentifierV1, PrivacyConstraintSetV1,
    PrivacyConstraintV1, RequestedSourceAccessV1, ResolvedOwnerScopeV1, ResourceIdV1, SinkKindV1,
    SinkPolicySnapshotV1, SourceAuthorizationInputV1, SourceBindingIdV1, SourceBindingSnapshotV1,
    SourceBindingV1, SourceDefinitionSnapshotV1, SourceDefinitionV1, SourceIdV1, SourceOwnerV1,
    SourcePolicyMetadataSnapshotV1, SourceSensitivityV1, TypedOperationV1,
};
pub use intersection::EffectiveSourceGrantV1;
pub use recheck::{
    SinkAdmissionProofV1, SinkRecheckDecisionV1, SinkRecheckDispositionV1,
    SourceAuthorizationProofV1, issue_source_authorization_proof, recheck_sink_admission,
};
pub use state::{
    PublicSourceResultShapeV1, SourceAccessDecisionV1, SourceAuthorizationDispositionV1,
};
