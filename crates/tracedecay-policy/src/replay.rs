//! Replay modes for immutable source-authorization decisions.
//!
//! Exact replay never silently substitutes implementation or inputs.
//! Recorded replay returns the recorded decision without evaluating. Current
//! best-effort replay evaluates current facts and names every replacement.

use serde::{Deserialize, Serialize};
use tracedecay_domain::ManifestDigest;

use crate::authorization::{
    PolicyEvaluatorVersionV1, PolicyReasonCodeV1, SourceAuthorizationDecisionV1,
    SourceAuthorizationEvaluator, SourceAuthorizationInputV1,
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ReplayModeV1 {
    ExactDeterministic,
    RecordedResult,
    CurrentBestEffort,
}

/// Immutable decision record required by every replay mode.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceAuthorizationRecordedResultV1 {
    pub evaluator_version: PolicyEvaluatorVersionV1,
    pub input: SourceAuthorizationInputV1,
    pub decision: SourceAuthorizationDecisionV1,
}

impl SourceAuthorizationRecordedResultV1 {
    pub fn new(
        evaluator_version: PolicyEvaluatorVersionV1,
        input: SourceAuthorizationInputV1,
        decision: SourceAuthorizationDecisionV1,
    ) -> Self {
        Self {
            evaluator_version,
            input,
            decision,
        }
    }

    fn is_internally_consistent(&self) -> bool {
        self.evaluator_version.is_valid()
            && self.decision.evaluator_version == self.evaluator_version
            && self.decision.input_digest == self.input.input_digest()
            && self.decision.policy_revision == self.input.policy_revision
            && self.decision.policy_digest == self.input.policy_digest
            && self.decision.configuration_digest == self.input.configuration_digest
            && self.decision.content_status == self.input.content_status
            && self.decision.evidence_references
                == self
                    .input
                    .evidence_references
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
            && self.decision.has_valid_digest()
    }
}

/// Every current-best-effort substitution is explicit and stable.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ReplaySubstitutionV1 {
    EvaluatorVersion,
    DefinitionSnapshot,
    BindingSnapshot,
    SourceGrant,
    RequesterGrant,
    ResolvedOwnerScope,
    RequestedAccess,
    SourcePolicy,
    SinkPolicy,
    ContentStatus,
    AuthorizationCoverage,
    SnapshotState,
    Requester,
    PolicyRevision,
    PolicyDigest,
    ConfigurationDigest,
    EvidenceReferences,
    EvaluatedAt,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceAuthorizationReplayRequestV1 {
    pub mode: ReplayModeV1,
    pub recorded: SourceAuthorizationRecordedResultV1,
    pub current_input: Option<SourceAuthorizationInputV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceAuthorizationReplayResultV1 {
    pub mode: ReplayModeV1,
    pub recorded_input_digest: ManifestDigest,
    pub decision: Option<SourceAuthorizationDecisionV1>,
    pub substitutions: Vec<ReplaySubstitutionV1>,
    pub ordered_reason_codes: Vec<PolicyReasonCodeV1>,
}

pub fn replay_source_authorization(
    evaluator: &impl SourceAuthorizationEvaluator,
    request: SourceAuthorizationReplayRequestV1,
) -> SourceAuthorizationReplayResultV1 {
    let recorded_input_digest = request.recorded.input.input_digest();
    if !request.recorded.is_internally_consistent() {
        return SourceAuthorizationReplayResultV1 {
            mode: request.mode,
            recorded_input_digest,
            decision: None,
            substitutions: Vec::new(),
            ordered_reason_codes: vec![PolicyReasonCodeV1::ReplayRecordInvalid],
        };
    }
    match request.mode {
        ReplayModeV1::RecordedResult => SourceAuthorizationReplayResultV1 {
            mode: request.mode,
            recorded_input_digest,
            decision: Some(request.recorded.decision),
            substitutions: Vec::new(),
            ordered_reason_codes: Vec::new(),
        },
        ReplayModeV1::ExactDeterministic => {
            if request.recorded.input.snapshot_state
                != crate::authorization::AuthorizationSnapshotStateV1::Complete
            {
                return SourceAuthorizationReplayResultV1 {
                    mode: request.mode,
                    recorded_input_digest,
                    decision: None,
                    substitutions: Vec::new(),
                    ordered_reason_codes: vec![PolicyReasonCodeV1::ReplayInputsMissing],
                };
            }
            if evaluator.evaluator_version() != &request.recorded.evaluator_version {
                return SourceAuthorizationReplayResultV1 {
                    mode: request.mode,
                    recorded_input_digest,
                    decision: None,
                    substitutions: Vec::new(),
                    ordered_reason_codes: vec![PolicyReasonCodeV1::ReplayEvaluatorVersionMismatch],
                };
            }
            let decision = evaluator.evaluate(&request.recorded.input);
            if decision != request.recorded.decision {
                return SourceAuthorizationReplayResultV1 {
                    mode: request.mode,
                    recorded_input_digest,
                    decision: None,
                    substitutions: Vec::new(),
                    ordered_reason_codes: vec![PolicyReasonCodeV1::ReplayDecisionMismatch],
                };
            }
            SourceAuthorizationReplayResultV1 {
                mode: request.mode,
                recorded_input_digest,
                decision: Some(decision),
                substitutions: Vec::new(),
                ordered_reason_codes: Vec::new(),
            }
        }
        ReplayModeV1::CurrentBestEffort => {
            let Some(current_input) = request.current_input else {
                return SourceAuthorizationReplayResultV1 {
                    mode: request.mode,
                    recorded_input_digest,
                    decision: None,
                    substitutions: Vec::new(),
                    ordered_reason_codes: vec![PolicyReasonCodeV1::ReplayInputsMissing],
                };
            };
            let mut substitutions =
                source_authorization_substitutions(&request.recorded.input, &current_input);
            if evaluator.evaluator_version() != &request.recorded.evaluator_version {
                substitutions.insert(0, ReplaySubstitutionV1::EvaluatorVersion);
            }
            SourceAuthorizationReplayResultV1 {
                mode: request.mode,
                recorded_input_digest,
                decision: Some(evaluator.evaluate(&current_input)),
                substitutions,
                ordered_reason_codes: Vec::new(),
            }
        }
    }
}

fn source_authorization_substitutions(
    recorded: &SourceAuthorizationInputV1,
    current: &SourceAuthorizationInputV1,
) -> Vec<ReplaySubstitutionV1> {
    let mut substitutions = Vec::new();
    if recorded.definition != current.definition {
        substitutions.push(ReplaySubstitutionV1::DefinitionSnapshot);
    }
    if recorded.binding != current.binding {
        substitutions.push(ReplaySubstitutionV1::BindingSnapshot);
    }
    if recorded.source_grant != current.source_grant {
        substitutions.push(ReplaySubstitutionV1::SourceGrant);
    }
    if recorded.requester_grant != current.requester_grant {
        substitutions.push(ReplaySubstitutionV1::RequesterGrant);
    }
    if recorded.resolved_owner_scope != current.resolved_owner_scope {
        substitutions.push(ReplaySubstitutionV1::ResolvedOwnerScope);
    }
    if recorded.requested_access != current.requested_access {
        substitutions.push(ReplaySubstitutionV1::RequestedAccess);
    }
    if recorded.source_policy != current.source_policy {
        substitutions.push(ReplaySubstitutionV1::SourcePolicy);
    }
    if recorded.sink_policy != current.sink_policy {
        substitutions.push(ReplaySubstitutionV1::SinkPolicy);
    }
    if recorded.content_status != current.content_status {
        substitutions.push(ReplaySubstitutionV1::ContentStatus);
    }
    if recorded.requested_coverage != current.requested_coverage {
        substitutions.push(ReplaySubstitutionV1::AuthorizationCoverage);
    }
    if recorded.snapshot_state != current.snapshot_state {
        substitutions.push(ReplaySubstitutionV1::SnapshotState);
    }
    if recorded.requester != current.requester {
        substitutions.push(ReplaySubstitutionV1::Requester);
    }
    if recorded.policy_revision != current.policy_revision {
        substitutions.push(ReplaySubstitutionV1::PolicyRevision);
    }
    if recorded.policy_digest != current.policy_digest {
        substitutions.push(ReplaySubstitutionV1::PolicyDigest);
    }
    if recorded.configuration_digest != current.configuration_digest {
        substitutions.push(ReplaySubstitutionV1::ConfigurationDigest);
    }
    if recorded.evidence_references != current.evidence_references {
        substitutions.push(ReplaySubstitutionV1::EvidenceReferences);
    }
    if recorded.evaluated_at != current.evaluated_at {
        substitutions.push(ReplaySubstitutionV1::EvaluatedAt);
    }
    substitutions
}
