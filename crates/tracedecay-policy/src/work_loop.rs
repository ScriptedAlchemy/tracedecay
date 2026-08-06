//! Pure work-loop proposal evaluation over an immutable Work snapshot.
//!
//! The evaluator explains whether a Work proposal fits the supplied evidence
//! and which explicit command is the legal next step. It never mutates the
//! graph, admits execution, accepts a task, or advances either evidence
//! frontier; accepting, rejecting, superseding, replanning, and admission
//! remain separate version-checked application commands.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{ManifestDigest, TaskId, UtcMicros};

use crate::authorization::{PolicyIdentifierV1, policy_digest};

/// Explicit cancellation fact supplied by the caller. Policy never observes a
/// live token.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkProposalCancellationV1 {
    Active,
    Cancelled { requested_at: UtcMicros },
}

/// One immutable evidence frontier. Local code/session evidence and live Git
/// evidence each carry their own frontier; the evaluator never merges,
/// substitutes, or advances one from the other.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkEvidenceFrontierV1 {
    pub watermark: UtcMicros,
    pub digest: ManifestDigest,
}

impl WorkEvidenceFrontierV1 {
    fn is_valid(&self) -> bool {
        self.digest.validate().is_ok()
    }
}

/// Recorded relation between the two supplied frontiers. `Incomparable` means
/// at least one side was absent; it is not collapsed into agreement.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkFrontierComparisonV1 {
    Agree,
    Disagree,
    Incomparable,
}

/// Immutable Work snapshot facts assembled by the application authority.
///
/// Every count and frontier is an explicit input; the evaluator performs no
/// storage read, clock lookup, or readiness derivation of its own.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProposalPolicyInputV1 {
    pub task_id: TaskId,
    pub based_on_version: u64,
    pub dependency_count: u32,
    pub unresolved_dependency_count: u32,
    pub accepted_proposal_present: bool,
    pub execution_admitted: bool,
    pub task_accepted: bool,
    pub runtime_evidence_count: u32,
    pub terminal_runtime_evidence_count: u32,
    pub local_evidence: Option<WorkEvidenceFrontierV1>,
    pub live_git_evidence: Option<WorkEvidenceFrontierV1>,
    pub policy_revision: u64,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub deadline: UtcMicros,
    pub cancellation: WorkProposalCancellationV1,
    pub evaluated_at: UtcMicros,
}

impl WorkProposalPolicyInputV1 {
    fn is_valid(&self) -> bool {
        self.based_on_version > 0
            && self.policy_revision > 0
            && self.policy_digest.validate().is_ok()
            && self.configuration_digest.validate().is_ok()
            && self.unresolved_dependency_count <= self.dependency_count
            && self.terminal_runtime_evidence_count <= self.runtime_evidence_count
            && self
                .local_evidence
                .as_ref()
                .is_none_or(WorkEvidenceFrontierV1::is_valid)
            && self
                .live_git_evidence
                .as_ref()
                .is_none_or(WorkEvidenceFrontierV1::is_valid)
    }
}

/// Exactly one disposition per decision.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkProposalDispositionV1 {
    Allow,
    Deny,
    Abstain,
    Indeterminate,
}

/// The explicit command the decision recommends next. A recommendation never
/// executes; each action names a separate version-checked application command.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkProposalActionV1 {
    ProceedToAcceptance,
    HoldForDependencies,
    AdmitExecution,
    Replan,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkProposalReasonV1 {
    InvalidRequest,
    RequestCancelled,
    DeadlineExceeded,
    FrontierAgreement,
    FrontierDisagreement,
    FrontierIncomparable,
    TaskAccepted,
    TerminalEvidenceObserved,
    ExecutionInFlight,
    ProposalAccepted,
    DependenciesUnresolved,
    Ready,
}

/// One explained, replayable work-loop decision.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProposalDecisionV1 {
    pub evaluator_id: PolicyIdentifierV1,
    pub evaluator_revision: u64,
    pub input_digest: ManifestDigest,
    pub task_id: TaskId,
    pub based_on_version: u64,
    pub policy_revision: u64,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub disposition: WorkProposalDispositionV1,
    pub recommended_action: Option<WorkProposalActionV1>,
    /// True when the recommendation is the declared deterministic baseline
    /// selected because the evidence cannot support a stronger claim.
    pub deterministic_fallback: bool,
    pub ordered_reason_codes: Vec<WorkProposalReasonV1>,
    /// The local code/session frontier, returned exactly as supplied.
    pub local_evidence: Option<WorkEvidenceFrontierV1>,
    /// The live Git frontier, returned exactly as supplied.
    pub live_git_evidence: Option<WorkEvidenceFrontierV1>,
    pub frontier_comparison: WorkFrontierComparisonV1,
}

pub trait WorkProposalEvaluator {
    fn evaluate(&self, input: &WorkProposalPolicyInputV1) -> WorkProposalDecisionV1;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkProposalEvaluatorV1 {
    evaluator_id: PolicyIdentifierV1,
}

impl Default for WorkProposalEvaluatorV1 {
    fn default() -> Self {
        Self {
            evaluator_id: PolicyIdentifierV1::new("work_proposal.v1")
                .expect("static evaluator identifier is valid"),
        }
    }
}

impl WorkProposalEvaluatorV1 {
    /// Revision of this reviewed implementation, recorded with every decision
    /// so replay can refuse a substituted evaluator. It is a property of the
    /// code, not of an instance.
    const EVALUATOR_REVISION: u64 = 1;

    fn decision(
        &self,
        input: &WorkProposalPolicyInputV1,
        disposition: WorkProposalDispositionV1,
        recommended_action: Option<WorkProposalActionV1>,
        deterministic_fallback: bool,
        ordered_reason_codes: Vec<WorkProposalReasonV1>,
        frontier_comparison: WorkFrontierComparisonV1,
    ) -> WorkProposalDecisionV1 {
        WorkProposalDecisionV1 {
            evaluator_id: self.evaluator_id.clone(),
            evaluator_revision: Self::EVALUATOR_REVISION,
            input_digest: policy_digest("tracedecay.policy.work-proposal-input.v1", input),
            task_id: input.task_id.clone(),
            based_on_version: input.based_on_version,
            policy_revision: input.policy_revision,
            policy_digest: input.policy_digest.clone(),
            configuration_digest: input.configuration_digest.clone(),
            disposition,
            recommended_action,
            deterministic_fallback,
            ordered_reason_codes,
            local_evidence: input.local_evidence.clone(),
            live_git_evidence: input.live_git_evidence.clone(),
            frontier_comparison,
        }
    }
}

fn compare_frontiers(input: &WorkProposalPolicyInputV1) -> WorkFrontierComparisonV1 {
    match (&input.local_evidence, &input.live_git_evidence) {
        (Some(local), Some(live)) => {
            if local.digest == live.digest {
                WorkFrontierComparisonV1::Agree
            } else {
                WorkFrontierComparisonV1::Disagree
            }
        }
        _ => WorkFrontierComparisonV1::Incomparable,
    }
}

const fn comparison_reason(comparison: WorkFrontierComparisonV1) -> WorkProposalReasonV1 {
    match comparison {
        WorkFrontierComparisonV1::Agree => WorkProposalReasonV1::FrontierAgreement,
        WorkFrontierComparisonV1::Disagree => WorkProposalReasonV1::FrontierDisagreement,
        WorkFrontierComparisonV1::Incomparable => WorkProposalReasonV1::FrontierIncomparable,
    }
}

impl WorkProposalEvaluator for WorkProposalEvaluatorV1 {
    fn evaluate(&self, input: &WorkProposalPolicyInputV1) -> WorkProposalDecisionV1 {
        if !input.is_valid() {
            return self.decision(
                input,
                WorkProposalDispositionV1::Indeterminate,
                None,
                false,
                vec![WorkProposalReasonV1::InvalidRequest],
                WorkFrontierComparisonV1::Incomparable,
            );
        }
        let comparison = compare_frontiers(input);
        if matches!(
            input.cancellation,
            WorkProposalCancellationV1::Cancelled { .. }
        ) {
            return self.decision(
                input,
                WorkProposalDispositionV1::Indeterminate,
                None,
                false,
                vec![WorkProposalReasonV1::RequestCancelled],
                comparison,
            );
        }
        if input.evaluated_at >= input.deadline {
            return self.decision(
                input,
                WorkProposalDispositionV1::Indeterminate,
                None,
                false,
                vec![WorkProposalReasonV1::DeadlineExceeded],
                comparison,
            );
        }
        let mut reasons = vec![comparison_reason(comparison)];
        if comparison == WorkFrontierComparisonV1::Disagree {
            // Disagreeing frontiers cannot support a recommendation. Both
            // frontiers are preserved verbatim; neither substitutes for the
            // other, and no baseline is invented from a merged view.
            return self.decision(
                input,
                WorkProposalDispositionV1::Abstain,
                None,
                false,
                reasons,
                comparison,
            );
        }
        if input.task_accepted {
            // The task is complete: further proposals against it are refused,
            // not merely out of scope, so acceptance cannot be re-litigated.
            reasons.push(WorkProposalReasonV1::TaskAccepted);
            return self.decision(
                input,
                WorkProposalDispositionV1::Deny,
                None,
                false,
                reasons,
                comparison,
            );
        }
        if input.execution_admitted {
            if input.terminal_runtime_evidence_count > 0 {
                reasons.push(WorkProposalReasonV1::TerminalEvidenceObserved);
                return self.decision(
                    input,
                    WorkProposalDispositionV1::Allow,
                    Some(WorkProposalActionV1::Replan),
                    false,
                    reasons,
                    comparison,
                );
            }
            reasons.push(WorkProposalReasonV1::ExecutionInFlight);
            return self.decision(
                input,
                WorkProposalDispositionV1::Abstain,
                None,
                false,
                reasons,
                comparison,
            );
        }
        if input.accepted_proposal_present {
            reasons.push(WorkProposalReasonV1::ProposalAccepted);
            return self.decision(
                input,
                WorkProposalDispositionV1::Allow,
                Some(WorkProposalActionV1::AdmitExecution),
                false,
                reasons,
                comparison,
            );
        }
        if input.unresolved_dependency_count > 0 {
            reasons.push(WorkProposalReasonV1::DependenciesUnresolved);
            return self.decision(
                input,
                WorkProposalDispositionV1::Allow,
                Some(WorkProposalActionV1::HoldForDependencies),
                true,
                reasons,
                comparison,
            );
        }
        reasons.push(WorkProposalReasonV1::Ready);
        self.decision(
            input,
            WorkProposalDispositionV1::Allow,
            Some(WorkProposalActionV1::ProceedToAcceptance),
            false,
            reasons,
            comparison,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn frontier(watermark: i64, byte: char) -> WorkEvidenceFrontierV1 {
        WorkEvidenceFrontierV1 {
            watermark: UtcMicros(watermark),
            digest: digest(byte),
        }
    }

    fn input() -> WorkProposalPolicyInputV1 {
        WorkProposalPolicyInputV1 {
            task_id: TaskId::try_from("task.policy.fixture".to_owned()).unwrap(),
            based_on_version: 1,
            dependency_count: 0,
            unresolved_dependency_count: 0,
            accepted_proposal_present: false,
            execution_admitted: false,
            task_accepted: false,
            runtime_evidence_count: 0,
            terminal_runtime_evidence_count: 0,
            local_evidence: Some(frontier(10, 'a')),
            live_git_evidence: None,
            policy_revision: 1,
            policy_digest: digest('b'),
            configuration_digest: digest('c'),
            deadline: UtcMicros(1_000),
            cancellation: WorkProposalCancellationV1::Active,
            evaluated_at: UtcMicros(100),
        }
    }

    #[test]
    fn identical_inputs_produce_identical_decisions() {
        let evaluator = WorkProposalEvaluatorV1::default();
        let request = input();
        assert_eq!(evaluator.evaluate(&request), evaluator.evaluate(&request));
    }

    #[test]
    fn ready_work_is_recommended_for_acceptance() {
        let decision = WorkProposalEvaluatorV1::default().evaluate(&input());
        assert_eq!(decision.disposition, WorkProposalDispositionV1::Allow);
        assert_eq!(
            decision.recommended_action,
            Some(WorkProposalActionV1::ProceedToAcceptance)
        );
        assert!(!decision.deterministic_fallback);
        assert_eq!(
            decision.frontier_comparison,
            WorkFrontierComparisonV1::Incomparable
        );
        assert_eq!(decision.local_evidence, input().local_evidence);
    }

    #[test]
    fn unresolved_dependencies_select_the_deterministic_hold_baseline() {
        let mut request = input();
        request.dependency_count = 2;
        request.unresolved_dependency_count = 1;
        let decision = WorkProposalEvaluatorV1::default().evaluate(&request);
        assert_eq!(decision.disposition, WorkProposalDispositionV1::Allow);
        assert_eq!(
            decision.recommended_action,
            Some(WorkProposalActionV1::HoldForDependencies)
        );
        assert!(decision.deterministic_fallback);
    }

    #[test]
    fn an_accepted_proposal_recommends_explicit_admission() {
        let mut request = input();
        request.accepted_proposal_present = true;
        let decision = WorkProposalEvaluatorV1::default().evaluate(&request);
        assert_eq!(
            decision.recommended_action,
            Some(WorkProposalActionV1::AdmitExecution)
        );
    }

    #[test]
    fn terminal_runtime_evidence_after_admission_recommends_a_replan() {
        let mut request = input();
        request.accepted_proposal_present = true;
        request.execution_admitted = true;
        request.runtime_evidence_count = 2;
        request.terminal_runtime_evidence_count = 1;
        let decision = WorkProposalEvaluatorV1::default().evaluate(&request);
        assert_eq!(decision.disposition, WorkProposalDispositionV1::Allow);
        assert_eq!(
            decision.recommended_action,
            Some(WorkProposalActionV1::Replan)
        );
        assert!(
            decision
                .ordered_reason_codes
                .contains(&WorkProposalReasonV1::TerminalEvidenceObserved)
        );
    }

    #[test]
    fn in_flight_execution_without_terminal_evidence_abstains() {
        let mut request = input();
        request.accepted_proposal_present = true;
        request.execution_admitted = true;
        let decision = WorkProposalEvaluatorV1::default().evaluate(&request);
        assert_eq!(decision.disposition, WorkProposalDispositionV1::Abstain);
        assert_eq!(decision.recommended_action, None);
    }

    #[test]
    fn an_accepted_task_denies_further_proposals() {
        let mut request = input();
        request.task_accepted = true;
        let decision = WorkProposalEvaluatorV1::default().evaluate(&request);
        assert_eq!(decision.disposition, WorkProposalDispositionV1::Deny);
        assert_eq!(decision.recommended_action, None);
        assert!(
            decision
                .ordered_reason_codes
                .contains(&WorkProposalReasonV1::TaskAccepted)
        );
    }

    #[test]
    fn agreeing_frontiers_are_returned_unchanged_and_recorded_as_agreement() {
        let mut request = input();
        request.local_evidence = Some(frontier(10, 'a'));
        request.live_git_evidence = Some(frontier(20, 'a'));
        let decision = WorkProposalEvaluatorV1::default().evaluate(&request);
        assert_eq!(
            decision.frontier_comparison,
            WorkFrontierComparisonV1::Agree
        );
        assert_eq!(decision.local_evidence, request.local_evidence);
        assert_eq!(decision.live_git_evidence, request.live_git_evidence);
        assert_eq!(decision.disposition, WorkProposalDispositionV1::Allow);
    }

    #[test]
    fn disagreeing_frontiers_abstain_without_substitution() {
        let mut request = input();
        request.local_evidence = Some(frontier(10, 'a'));
        request.live_git_evidence = Some(frontier(10, 'f'));
        let decision = WorkProposalEvaluatorV1::default().evaluate(&request);
        assert_eq!(decision.disposition, WorkProposalDispositionV1::Abstain);
        assert_eq!(decision.recommended_action, None);
        assert_eq!(
            decision.frontier_comparison,
            WorkFrontierComparisonV1::Disagree
        );
        assert_eq!(decision.local_evidence, request.local_evidence);
        assert_eq!(decision.live_git_evidence, request.live_git_evidence);
    }

    #[test]
    fn cancellation_and_deadline_are_indeterminate() {
        let mut cancelled = input();
        cancelled.cancellation = WorkProposalCancellationV1::Cancelled {
            requested_at: UtcMicros(50),
        };
        assert_eq!(
            WorkProposalEvaluatorV1::default()
                .evaluate(&cancelled)
                .disposition,
            WorkProposalDispositionV1::Indeterminate
        );

        let mut elapsed = input();
        elapsed.evaluated_at = elapsed.deadline;
        assert_eq!(
            WorkProposalEvaluatorV1::default()
                .evaluate(&elapsed)
                .disposition,
            WorkProposalDispositionV1::Indeterminate
        );
    }

    #[test]
    fn inconsistent_counts_are_an_invalid_request() {
        let mut request = input();
        request.terminal_runtime_evidence_count = 3;
        request.runtime_evidence_count = 1;
        let decision = WorkProposalEvaluatorV1::default().evaluate(&request);
        assert_eq!(
            decision.disposition,
            WorkProposalDispositionV1::Indeterminate
        );
        assert_eq!(
            decision.ordered_reason_codes,
            vec![WorkProposalReasonV1::InvalidRequest]
        );
    }
}
