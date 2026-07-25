//! Retained pure policy families that are not capability routing.
//!
//! Each evaluator is a distinct callable authority. They share only the
//! deterministic evidence classification mechanics; names, revisions, and
//! decisions remain family-specific and replayable.

use serde::{Deserialize, Serialize};
use tracedecay_domain::{ManifestDigest, UtcMicros};

use crate::authorization::{PolicyIdentifierV1, PolicyReasonCodeV1, policy_digest};
use crate::replay::ReplayModeV1;

const RETAINED_POLICY_INPUT_DOMAIN: &str = "tracedecay.policy.retained-input.v1";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RetainedPolicyKindV1 {
    HintEligibilityDelivery,
    LocalLiveCorrelation,
    DiagnosticsCuration,
    MemoryProposal,
    ConflictArbitration,
    ExperimentRouting,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PolicyEvidenceStateV1 {
    Fresh,
    Partial,
    Stale,
    Unavailable,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PolicyEvidenceCoverageV1 {
    Complete,
    Partial,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PolicyEvidenceAgreementV1 {
    Agree,
    Disagree,
    Incomparable,
    NotApplicable,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PolicyEvidenceSnapshotV1 {
    pub watermark: ManifestDigest,
    pub state: PolicyEvidenceStateV1,
    pub coverage: PolicyEvidenceCoverageV1,
}

impl PolicyEvidenceSnapshotV1 {
    fn is_valid(&self) -> bool {
        self.watermark.validate().is_ok()
    }

    const fn is_complete_and_fresh(&self) -> bool {
        matches!(self.state, PolicyEvidenceStateV1::Fresh)
            && matches!(self.coverage, PolicyEvidenceCoverageV1::Complete)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RetainedPolicySnapshotStateV1 {
    Complete,
    Incomplete,
}

/// Immutable input shared structurally by the six retained evaluator
/// families. The evaluator kind is intentionally not caller-selectable.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetainedPolicyInputV1 {
    pub requested_route: PolicyIdentifierV1,
    pub deterministic_fallback: Option<PolicyIdentifierV1>,
    pub enabled: bool,
    pub authorized: bool,
    pub primary_evidence: PolicyEvidenceSnapshotV1,
    pub secondary_evidence: Option<PolicyEvidenceSnapshotV1>,
    pub evidence_agreement: PolicyEvidenceAgreementV1,
    pub snapshot_state: RetainedPolicySnapshotStateV1,
    pub policy_revision: u64,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub evaluated_at: UtcMicros,
}

impl RetainedPolicyInputV1 {
    pub fn input_digest(&self) -> ManifestDigest {
        policy_digest(RETAINED_POLICY_INPUT_DOMAIN, self)
    }

    fn is_valid(&self) -> bool {
        self.requested_route.is_valid()
            && self
                .deterministic_fallback
                .as_ref()
                .is_none_or(PolicyIdentifierV1::is_valid)
            && self.primary_evidence.is_valid()
            && self
                .secondary_evidence
                .as_ref()
                .is_none_or(PolicyEvidenceSnapshotV1::is_valid)
            && self.policy_revision > 0
            && self.policy_digest.validate().is_ok()
            && self.configuration_digest.validate().is_ok()
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RetainedPolicyDispositionV1 {
    Allow,
    Deny,
    Abstain,
    NotApplicable,
    Indeterminate,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RetainedPolicyReasonV1 {
    InvalidInput,
    Disabled,
    Unauthorized,
    IncompleteSnapshot,
    MissingSecondaryEvidence,
    EvidencePartial,
    EvidenceStale,
    EvidenceUnavailable,
    EvidenceUnknown,
    EvidenceDisagrees,
    EvidenceIncomparable,
    DeterministicFallback,
    Selected,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetainedPolicyDecisionV1 {
    pub kind: RetainedPolicyKindV1,
    pub evaluator_id: PolicyIdentifierV1,
    pub evaluator_revision: u64,
    pub input_digest: ManifestDigest,
    pub policy_revision: u64,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub disposition: RetainedPolicyDispositionV1,
    pub selected_route: Option<PolicyIdentifierV1>,
    pub ordered_reason_codes: Vec<RetainedPolicyReasonV1>,
    pub primary_evidence: PolicyEvidenceSnapshotV1,
    pub secondary_evidence: Option<PolicyEvidenceSnapshotV1>,
    pub evidence_agreement: PolicyEvidenceAgreementV1,
}

impl RetainedPolicyDecisionV1 {
    fn is_bound_to(&self, input: &RetainedPolicyInputV1) -> bool {
        self.evaluator_id.is_valid()
            && self.evaluator_revision > 0
            && self.input_digest == input.input_digest()
            && self.policy_revision == input.policy_revision
            && self.policy_digest == input.policy_digest
            && self.configuration_digest == input.configuration_digest
            && self.primary_evidence == input.primary_evidence
            && self.secondary_evidence == input.secondary_evidence
            && self.evidence_agreement == input.evidence_agreement
            && !self.ordered_reason_codes.is_empty()
            && match self.disposition {
                RetainedPolicyDispositionV1::Allow => self.selected_route.is_some(),
                RetainedPolicyDispositionV1::Deny
                | RetainedPolicyDispositionV1::Abstain
                | RetainedPolicyDispositionV1::NotApplicable
                | RetainedPolicyDispositionV1::Indeterminate => self.selected_route.is_none(),
            }
    }
}

pub trait RetainedPolicyEvaluator {
    fn kind(&self) -> RetainedPolicyKindV1;
    fn evaluator_id(&self) -> &PolicyIdentifierV1;
    fn evaluator_revision(&self) -> u64;

    fn evaluate(&self, input: &RetainedPolicyInputV1) -> RetainedPolicyDecisionV1 {
        evaluate_retained(self, input)
    }
}

fn evaluate_retained<E: RetainedPolicyEvaluator + ?Sized>(
    evaluator: &E,
    input: &RetainedPolicyInputV1,
) -> RetainedPolicyDecisionV1 {
    let (disposition, selected_route, ordered_reason_codes) = classify(evaluator.kind(), input);
    RetainedPolicyDecisionV1 {
        kind: evaluator.kind(),
        evaluator_id: evaluator.evaluator_id().clone(),
        evaluator_revision: evaluator.evaluator_revision(),
        input_digest: input.input_digest(),
        policy_revision: input.policy_revision,
        policy_digest: input.policy_digest.clone(),
        configuration_digest: input.configuration_digest.clone(),
        disposition,
        selected_route,
        ordered_reason_codes,
        primary_evidence: input.primary_evidence.clone(),
        secondary_evidence: input.secondary_evidence.clone(),
        evidence_agreement: input.evidence_agreement,
    }
}

fn classify(
    kind: RetainedPolicyKindV1,
    input: &RetainedPolicyInputV1,
) -> (
    RetainedPolicyDispositionV1,
    Option<PolicyIdentifierV1>,
    Vec<RetainedPolicyReasonV1>,
) {
    use RetainedPolicyDispositionV1::{Abstain, Allow, Deny, Indeterminate, NotApplicable};
    use RetainedPolicyReasonV1::{
        Disabled, EvidenceDisagrees, EvidenceIncomparable, IncompleteSnapshot, InvalidInput,
        MissingSecondaryEvidence, Selected, Unauthorized,
    };

    if !input.is_valid() {
        return (Indeterminate, None, vec![InvalidInput]);
    }
    if !input.enabled {
        return (NotApplicable, None, vec![Disabled]);
    }
    if !input.authorized {
        return (Deny, None, vec![Unauthorized]);
    }
    if input.snapshot_state == RetainedPolicySnapshotStateV1::Incomplete {
        return (Indeterminate, None, vec![IncompleteSnapshot]);
    }

    if kind == RetainedPolicyKindV1::LocalLiveCorrelation {
        let Some(secondary) = &input.secondary_evidence else {
            return (Indeterminate, None, vec![MissingSecondaryEvidence]);
        };
        match input.evidence_agreement {
            PolicyEvidenceAgreementV1::Disagree => {
                return (Abstain, None, vec![EvidenceDisagrees]);
            }
            PolicyEvidenceAgreementV1::Incomparable => {
                return (Abstain, None, vec![EvidenceIncomparable]);
            }
            PolicyEvidenceAgreementV1::NotApplicable => {
                return (Indeterminate, None, vec![MissingSecondaryEvidence]);
            }
            PolicyEvidenceAgreementV1::Agree => {}
        }
        if !secondary.is_complete_and_fresh() {
            return evidence_fallback(kind, secondary, input);
        }
    } else if kind == RetainedPolicyKindV1::ConflictArbitration {
        match input.evidence_agreement {
            PolicyEvidenceAgreementV1::Disagree => {
                return (Deny, None, vec![EvidenceDisagrees]);
            }
            PolicyEvidenceAgreementV1::Incomparable => {
                return (Abstain, None, vec![EvidenceIncomparable]);
            }
            PolicyEvidenceAgreementV1::Agree | PolicyEvidenceAgreementV1::NotApplicable => {}
        }
    }

    if !input.primary_evidence.is_complete_and_fresh() {
        return evidence_fallback(kind, &input.primary_evidence, input);
    }
    (Allow, Some(input.requested_route.clone()), vec![Selected])
}

fn evidence_fallback(
    kind: RetainedPolicyKindV1,
    evidence: &PolicyEvidenceSnapshotV1,
    input: &RetainedPolicyInputV1,
) -> (
    RetainedPolicyDispositionV1,
    Option<PolicyIdentifierV1>,
    Vec<RetainedPolicyReasonV1>,
) {
    let evidence_reason = match (evidence.state, evidence.coverage) {
        (PolicyEvidenceStateV1::Unavailable, _) => RetainedPolicyReasonV1::EvidenceUnavailable,
        (PolicyEvidenceStateV1::Stale, _) => RetainedPolicyReasonV1::EvidenceStale,
        (PolicyEvidenceStateV1::Unknown, _) | (_, PolicyEvidenceCoverageV1::Unknown) => {
            RetainedPolicyReasonV1::EvidenceUnknown
        }
        (PolicyEvidenceStateV1::Partial, _) | (_, PolicyEvidenceCoverageV1::Partial) => {
            RetainedPolicyReasonV1::EvidencePartial
        }
        (PolicyEvidenceStateV1::Fresh, PolicyEvidenceCoverageV1::Complete) => {
            RetainedPolicyReasonV1::Selected
        }
    };
    if kind == RetainedPolicyKindV1::ExperimentRouting
        && let Some(fallback) = &input.deterministic_fallback
    {
        return (
            RetainedPolicyDispositionV1::Allow,
            Some(fallback.clone()),
            vec![
                evidence_reason,
                RetainedPolicyReasonV1::DeterministicFallback,
            ],
        );
    }
    (
        RetainedPolicyDispositionV1::Abstain,
        None,
        vec![evidence_reason],
    )
}

macro_rules! retained_evaluator {
    ($name:ident, $kind:ident, $id:literal) => {
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct $name {
            evaluator_id: PolicyIdentifierV1,
        }

        impl Default for $name {
            fn default() -> Self {
                Self {
                    evaluator_id: PolicyIdentifierV1::new($id)
                        .expect("static retained policy evaluator id is valid"),
                }
            }
        }

        impl RetainedPolicyEvaluator for $name {
            fn kind(&self) -> RetainedPolicyKindV1 {
                RetainedPolicyKindV1::$kind
            }

            fn evaluator_id(&self) -> &PolicyIdentifierV1 {
                &self.evaluator_id
            }

            fn evaluator_revision(&self) -> u64 {
                1
            }
        }
    };
}

retained_evaluator!(
    HintPolicyEvaluatorV1,
    HintEligibilityDelivery,
    "hint_eligibility_delivery.v1"
);
retained_evaluator!(
    CorrelationPolicyEvaluatorV1,
    LocalLiveCorrelation,
    "local_live_correlation.v1"
);
retained_evaluator!(
    DiagnosticsCurationPolicyEvaluatorV1,
    DiagnosticsCuration,
    "diagnostics_curation.v1"
);
retained_evaluator!(
    MemoryProposalPolicyEvaluatorV1,
    MemoryProposal,
    "memory_proposal.v1"
);
retained_evaluator!(
    ConflictArbitrationPolicyEvaluatorV1,
    ConflictArbitration,
    "conflict_arbitration.v1"
);
retained_evaluator!(
    ExperimentRoutingPolicyEvaluatorV1,
    ExperimentRouting,
    "experiment_routing.v1"
);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetainedPolicyRecordedResultV1 {
    pub input: RetainedPolicyInputV1,
    pub decision: RetainedPolicyDecisionV1,
}

impl RetainedPolicyRecordedResultV1 {
    pub fn new(input: RetainedPolicyInputV1, decision: RetainedPolicyDecisionV1) -> Self {
        Self { input, decision }
    }

    fn is_internally_consistent(&self) -> bool {
        self.input.is_valid() && self.decision.is_bound_to(&self.input)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PolicyReplaySubstitutionV1 {
    EvaluatorVersion,
    RequestedRoute,
    DeterministicFallback,
    Enabled,
    Authorization,
    PrimaryEvidence,
    SecondaryEvidence,
    EvidenceAgreement,
    SnapshotState,
    PolicyRevision,
    PolicyDigest,
    ConfigurationDigest,
    EvaluatedAt,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetainedPolicyReplayRequestV1 {
    pub mode: ReplayModeV1,
    pub recorded: RetainedPolicyRecordedResultV1,
    pub current_input: Option<RetainedPolicyInputV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetainedPolicyReplayResultV1 {
    pub mode: ReplayModeV1,
    pub recorded_input_digest: ManifestDigest,
    pub decision: Option<RetainedPolicyDecisionV1>,
    pub substitutions: Vec<PolicyReplaySubstitutionV1>,
    pub ordered_reason_codes: Vec<PolicyReasonCodeV1>,
}

pub fn replay_retained_policy(
    evaluator: &impl RetainedPolicyEvaluator,
    request: RetainedPolicyReplayRequestV1,
) -> RetainedPolicyReplayResultV1 {
    let recorded_input_digest = request.recorded.input.input_digest();
    if !request.recorded.is_internally_consistent()
        || request.recorded.decision.kind != evaluator.kind()
    {
        return replay_failure(
            request.mode,
            recorded_input_digest,
            PolicyReasonCodeV1::ReplayRecordInvalid,
        );
    }
    match request.mode {
        ReplayModeV1::RecordedResult => RetainedPolicyReplayResultV1 {
            mode: request.mode,
            recorded_input_digest,
            decision: Some(request.recorded.decision),
            substitutions: Vec::new(),
            ordered_reason_codes: Vec::new(),
        },
        ReplayModeV1::ExactDeterministic => {
            if request.recorded.input.snapshot_state != RetainedPolicySnapshotStateV1::Complete {
                return replay_failure(
                    request.mode,
                    recorded_input_digest,
                    PolicyReasonCodeV1::ReplayInputsMissing,
                );
            }
            if request.recorded.decision.evaluator_id != *evaluator.evaluator_id()
                || request.recorded.decision.evaluator_revision != evaluator.evaluator_revision()
            {
                return replay_failure(
                    request.mode,
                    recorded_input_digest,
                    PolicyReasonCodeV1::ReplayEvaluatorVersionMismatch,
                );
            }
            let decision = evaluator.evaluate(&request.recorded.input);
            if decision != request.recorded.decision {
                return replay_failure(
                    request.mode,
                    recorded_input_digest,
                    PolicyReasonCodeV1::ReplayDecisionMismatch,
                );
            }
            RetainedPolicyReplayResultV1 {
                mode: request.mode,
                recorded_input_digest,
                decision: Some(decision),
                substitutions: Vec::new(),
                ordered_reason_codes: Vec::new(),
            }
        }
        ReplayModeV1::CurrentBestEffort => {
            let Some(current_input) = request.current_input else {
                return replay_failure(
                    request.mode,
                    recorded_input_digest,
                    PolicyReasonCodeV1::ReplayInputsMissing,
                );
            };
            let mut substitutions = replay_substitutions(&request.recorded.input, &current_input);
            if request.recorded.decision.evaluator_id != *evaluator.evaluator_id()
                || request.recorded.decision.evaluator_revision != evaluator.evaluator_revision()
            {
                substitutions.insert(0, PolicyReplaySubstitutionV1::EvaluatorVersion);
            }
            RetainedPolicyReplayResultV1 {
                mode: request.mode,
                recorded_input_digest,
                decision: Some(evaluator.evaluate(&current_input)),
                substitutions,
                ordered_reason_codes: Vec::new(),
            }
        }
    }
}

fn replay_failure(
    mode: ReplayModeV1,
    recorded_input_digest: ManifestDigest,
    reason: PolicyReasonCodeV1,
) -> RetainedPolicyReplayResultV1 {
    RetainedPolicyReplayResultV1 {
        mode,
        recorded_input_digest,
        decision: None,
        substitutions: Vec::new(),
        ordered_reason_codes: vec![reason],
    }
}

fn replay_substitutions(
    recorded: &RetainedPolicyInputV1,
    current: &RetainedPolicyInputV1,
) -> Vec<PolicyReplaySubstitutionV1> {
    let mut substitutions = Vec::new();
    macro_rules! changed {
        ($field:ident, $variant:ident) => {
            if recorded.$field != current.$field {
                substitutions.push(PolicyReplaySubstitutionV1::$variant);
            }
        };
    }
    changed!(requested_route, RequestedRoute);
    changed!(deterministic_fallback, DeterministicFallback);
    changed!(enabled, Enabled);
    changed!(authorized, Authorization);
    changed!(primary_evidence, PrimaryEvidence);
    changed!(secondary_evidence, SecondaryEvidence);
    changed!(evidence_agreement, EvidenceAgreement);
    changed!(snapshot_state, SnapshotState);
    changed!(policy_revision, PolicyRevision);
    changed!(policy_digest, PolicyDigest);
    changed!(configuration_digest, ConfigurationDigest);
    changed!(evaluated_at, EvaluatedAt);
    substitutions
}
