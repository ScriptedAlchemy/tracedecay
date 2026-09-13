//! Deterministic, side-effect-free policy evaluators for TraceDecay V2.
//!
//! This crate receives immutable snapshots and produces typed decisions. It
//! never opens storage, reads configuration, invokes a provider, starts an
//! analyzer, executes Git, renders a transport response, or performs a clock
//! lookup. Every time-dependent fact is an explicit input.

#![forbid(unsafe_code)]

mod hotpath_observe;

pub mod analyzer;
pub mod authorization;
pub mod configuration;
pub mod curation;
pub mod diagnostic_curation;
pub mod git;
pub mod hint_delivery;
pub mod retrieval_selection;
pub mod routing;
pub mod work_loop;

pub use curation::{
    CurationApplyAuthorityV1, CurationApplyDecisionV1, CurationApplyDispositionV1,
    CurationApplyPolicyInputV1, CurationApplySubjectV1, CurationValidationDispositionV1,
    evaluate_curation_apply,
};
pub use git::{
    GitConflictRiskV1, GitEffectAuthorizationV1, GitEffectClassificationInputV1,
    GitEffectClassifier, GitEffectClassifierV1, GitEffectDispositionV1, GitIndexEffectV1,
    GitPreviewPreconditionV1, GitRepositoryStateFactV1,
};
pub use routing::{
    CapabilityAvailabilityV1, CapabilityEffectClassV1, CapabilityRoutingDecisionV1, ScopeMatchV1,
    TruthFreshnessRequirementV1, TruthSourceStateV1,
};
pub use work_loop::{
    WORK_CALIBRATION_SUPPORT_FLOOR, WorkBudgetEnvelopeV1, WorkContentLocationClassV1,
    WorkContentLocationLimitV1, WorkEffortClassV1, WorkOrdinalBandV1, WorkPriorOutcomeV1,
    WorkPriorTerminalV1, WorkProposalReasonV1, WorkRouteCandidateV1, WorkRouteOverrideV1,
    WorkRoutePlanV1,
};
