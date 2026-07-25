use tracedecay_domain::{ManifestDigest, UtcMicros};
use tracedecay_policy::{
    ConflictArbitrationPolicyEvaluatorV1, CorrelationPolicyEvaluatorV1,
    DiagnosticsCurationPolicyEvaluatorV1, ExperimentRoutingPolicyEvaluatorV1,
    HintPolicyEvaluatorV1, MemoryProposalPolicyEvaluatorV1, PolicyEvidenceAgreementV1,
    PolicyEvidenceCoverageV1, PolicyEvidenceSnapshotV1, PolicyEvidenceStateV1, PolicyIdentifierV1,
    PolicyReplaySubstitutionV1, ReplayModeV1, RetainedPolicyDispositionV1, RetainedPolicyEvaluator,
    RetainedPolicyInputV1, RetainedPolicyRecordedResultV1, RetainedPolicyReplayRequestV1,
    RetainedPolicySnapshotStateV1, replay_retained_policy,
};

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn evidence(byte: char) -> PolicyEvidenceSnapshotV1 {
    PolicyEvidenceSnapshotV1 {
        watermark: digest(byte),
        state: PolicyEvidenceStateV1::Fresh,
        coverage: PolicyEvidenceCoverageV1::Complete,
    }
}

fn input() -> RetainedPolicyInputV1 {
    RetainedPolicyInputV1 {
        requested_route: PolicyIdentifierV1::new("route.primary").unwrap(),
        deterministic_fallback: Some(PolicyIdentifierV1::new("route.baseline").unwrap()),
        enabled: true,
        authorized: true,
        primary_evidence: evidence('a'),
        secondary_evidence: None,
        evidence_agreement: PolicyEvidenceAgreementV1::NotApplicable,
        snapshot_state: RetainedPolicySnapshotStateV1::Complete,
        policy_revision: 7,
        policy_digest: digest('b'),
        configuration_digest: digest('c'),
        evaluated_at: UtcMicros(10),
    }
}

#[test]
fn retained_policy_families_are_distinct_callable_evaluators() {
    let input = input();
    let decisions = [
        HintPolicyEvaluatorV1::default().evaluate(&input),
        DiagnosticsCurationPolicyEvaluatorV1::default().evaluate(&input),
        MemoryProposalPolicyEvaluatorV1::default().evaluate(&input),
        ConflictArbitrationPolicyEvaluatorV1::default().evaluate(&input),
        ExperimentRoutingPolicyEvaluatorV1::default().evaluate(&input),
    ];

    assert!(decisions.iter().all(|decision| {
        decision.disposition == RetainedPolicyDispositionV1::Allow
            && decision.selected_route.as_ref() == Some(&input.requested_route)
            && decision.primary_evidence == input.primary_evidence
            && decision.policy_digest == input.policy_digest
            && decision.configuration_digest == input.configuration_digest
    }));
    let evaluator_ids = decisions
        .iter()
        .map(|decision| decision.evaluator_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(evaluator_ids.len(), decisions.len());
}

#[test]
fn correlation_preserves_independent_frontiers_and_abstains_on_disagreement() {
    let mut input = input();
    input.secondary_evidence = Some(evidence('d'));
    input.evidence_agreement = PolicyEvidenceAgreementV1::Disagree;

    let decision = CorrelationPolicyEvaluatorV1::default().evaluate(&input);

    assert_eq!(decision.disposition, RetainedPolicyDispositionV1::Abstain);
    assert_eq!(decision.primary_evidence, input.primary_evidence);
    assert_eq!(decision.secondary_evidence, input.secondary_evidence);
    assert_eq!(
        decision.evidence_agreement,
        PolicyEvidenceAgreementV1::Disagree
    );
    assert!(decision.selected_route.is_none());
}

#[test]
fn experiment_routing_uses_only_the_declared_deterministic_fallback() {
    let mut input = input();
    input.primary_evidence.state = PolicyEvidenceStateV1::Stale;
    input.primary_evidence.coverage = PolicyEvidenceCoverageV1::Partial;

    let decision = ExperimentRoutingPolicyEvaluatorV1::default().evaluate(&input);

    assert_eq!(decision.disposition, RetainedPolicyDispositionV1::Allow);
    assert_eq!(decision.selected_route, input.deterministic_fallback);

    input.deterministic_fallback = None;
    let abstained = ExperimentRoutingPolicyEvaluatorV1::default().evaluate(&input);
    assert_eq!(abstained.disposition, RetainedPolicyDispositionV1::Abstain);
    assert!(abstained.selected_route.is_none());
}

#[test]
fn retained_policy_replay_distinguishes_exact_recorded_and_current_best_effort() {
    let evaluator = HintPolicyEvaluatorV1::default();
    let recorded_input = input();
    let recorded_decision = evaluator.evaluate(&recorded_input);
    let recorded =
        RetainedPolicyRecordedResultV1::new(recorded_input.clone(), recorded_decision.clone());

    let exact = replay_retained_policy(
        &evaluator,
        RetainedPolicyReplayRequestV1 {
            mode: ReplayModeV1::ExactDeterministic,
            recorded: recorded.clone(),
            current_input: None,
        },
    );
    assert_eq!(exact.decision, Some(recorded_decision.clone()));
    assert!(exact.substitutions.is_empty());

    let recorded_result = replay_retained_policy(
        &evaluator,
        RetainedPolicyReplayRequestV1 {
            mode: ReplayModeV1::RecordedResult,
            recorded: recorded.clone(),
            current_input: None,
        },
    );
    assert_eq!(recorded_result.decision, Some(recorded_decision));
    assert!(recorded_result.substitutions.is_empty());

    let mut current = recorded_input;
    current.configuration_digest = digest('e');
    current.primary_evidence = evidence('f');
    let current_result = replay_retained_policy(
        &evaluator,
        RetainedPolicyReplayRequestV1 {
            mode: ReplayModeV1::CurrentBestEffort,
            recorded,
            current_input: Some(current),
        },
    );
    assert!(
        current_result
            .substitutions
            .contains(&PolicyReplaySubstitutionV1::ConfigurationDigest)
    );
    assert!(
        current_result
            .substitutions
            .contains(&PolicyReplaySubstitutionV1::PrimaryEvidence)
    );
}

#[test]
fn exact_replay_rejects_incomplete_immutable_inputs() {
    let evaluator = MemoryProposalPolicyEvaluatorV1::default();
    let mut recorded_input = input();
    recorded_input.snapshot_state = RetainedPolicySnapshotStateV1::Incomplete;
    let recorded_decision = evaluator.evaluate(&recorded_input);

    let replay = replay_retained_policy(
        &evaluator,
        RetainedPolicyReplayRequestV1 {
            mode: ReplayModeV1::ExactDeterministic,
            recorded: RetainedPolicyRecordedResultV1::new(recorded_input, recorded_decision),
            current_input: None,
        },
    );

    assert!(replay.decision.is_none());
    assert_eq!(
        replay.ordered_reason_codes,
        vec![tracedecay_policy::PolicyReasonCodeV1::ReplayInputsMissing]
    );
}

#[test]
fn current_best_effort_names_evaluator_version_substitution() {
    let evaluator = HintPolicyEvaluatorV1::default();
    let recorded_input = input();
    let mut recorded_decision = evaluator.evaluate(&recorded_input);
    recorded_decision.evaluator_id = PolicyIdentifierV1::new("hint_legacy.v9").unwrap();
    recorded_decision.evaluator_revision = 9;

    let replay = replay_retained_policy(
        &evaluator,
        RetainedPolicyReplayRequestV1 {
            mode: ReplayModeV1::CurrentBestEffort,
            recorded: RetainedPolicyRecordedResultV1::new(
                recorded_input.clone(),
                recorded_decision,
            ),
            current_input: Some(recorded_input),
        },
    );

    assert!(
        replay
            .substitutions
            .contains(&PolicyReplaySubstitutionV1::EvaluatorVersion)
    );
}
