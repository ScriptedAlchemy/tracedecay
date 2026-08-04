use serde_json::json;
use tracedecay_domain::ManifestDigest;
use tracedecay_policy::authorization::{
    AuthorizationCoverageV1, AuthorizationSnapshotStateV1, DisclosureClassV1,
    ExternalContentStatusV1, PolicyIdentifierV1, PolicyReasonCodeV1, PublicSourceResultShapeV1,
    SinkKindV1, SourceAccessDecisionV1, SourceAuthorizationEvaluator,
    SourceAuthorizationEvaluatorV1, SourceAuthorizationTruthTableV1, TypedOperationV1,
    issue_source_authorization_proof, public_source_result_shape,
};
use tracedecay_policy::replay::{
    ReplayModeV1, ReplaySubstitutionV1, SourceAuthorizationRecordedResultV1,
    SourceAuthorizationReplayRequestV1, replay_source_authorization,
};

const SOURCE_AUTHORIZATION_TRUTH_TABLES: &str =
    include_str!("fixtures/source_authorization/core.json");

fn truth_tables() -> Vec<SourceAuthorizationTruthTableV1> {
    serde_json::from_str(SOURCE_AUTHORIZATION_TRUTH_TABLES)
        .expect("checked-in source authorization truth tables deserialize")
}

#[test]
fn canonical_source_authorization_truth_tables_hold() {
    let evaluator = SourceAuthorizationEvaluatorV1::default();

    for row in truth_tables() {
        let decision = evaluator.evaluate(&row.input);

        assert_eq!(
            decision.access, row.expected.access,
            "unexpected access for {}",
            row.name
        );
        assert_eq!(
            decision.authorization_coverage, row.expected.authorization_coverage,
            "unexpected coverage for {}",
            row.name
        );
        assert_eq!(
            decision.disposition, row.expected.disposition,
            "unexpected disposition for {}",
            row.name
        );
        assert_eq!(
            decision.ordered_reason_codes, row.expected.ordered_reason_codes,
            "unexpected reasons for {}",
            row.name
        );
        assert_eq!(
            decision.effective_grant.is_some(),
            row.expected.has_effective_grant,
            "unexpected effective-grant presence for {}",
            row.name
        );
        assert_eq!(
            public_source_result_shape(&decision, row.source_visible),
            row.expected.public_shape,
            "unexpected public shape for {}",
            row.name
        );
    }
}

#[test]
fn identical_inputs_produce_identical_canonical_decisions() {
    let evaluator = SourceAuthorizationEvaluatorV1::default();
    let input = truth_tables()
        .into_iter()
        .find(|row| row.name == "project_authorized_live")
        .expect("allow fixture exists")
        .input;

    assert_eq!(evaluator.evaluate(&input), evaluator.evaluate(&input));
}

#[test]
fn definition_binding_and_owner_snapshots_remain_separate_authorities() {
    let evaluator = SourceAuthorizationEvaluatorV1::default();
    let mut input = truth_tables()
        .into_iter()
        .find(|row| row.name == "project_authorized_live")
        .expect("allow fixture exists")
        .input;

    assert_eq!(
        &input.definition.definition.source_id,
        input.binding.binding.source_id()
    );
    assert_eq!(
        input.binding.binding.owner(),
        input.resolved_owner_scope.owner
    );

    input.definition.definition.source_id =
        PolicyIdentifierV1::new("source.definition.other").unwrap();
    let decision = evaluator.evaluate(&input);

    assert_eq!(decision.access, SourceAccessDecisionV1::Unauthorized);
    assert_eq!(
        decision.ordered_reason_codes,
        [
            PolicyReasonCodeV1::InputComplete,
            PolicyReasonCodeV1::SourceDefinitionBindingMismatch,
        ]
    );
}

#[test]
fn partial_snapshot_coverage_never_claims_authoritative_deletion() {
    let evaluator = SourceAuthorizationEvaluatorV1::default();
    let mut input = truth_tables()
        .into_iter()
        .find(|row| row.name == "project_authorized_live")
        .expect("allow fixture exists")
        .input;
    input.content_status = ExternalContentStatusV1::Partial;
    input.requested_coverage = AuthorizationCoverageV1::Partial;

    let decision = evaluator.evaluate(&input);

    assert_eq!(decision.access, SourceAccessDecisionV1::Authorized);
    assert_eq!(
        decision.authorization_coverage,
        AuthorizationCoverageV1::Partial
    );
    assert_eq!(
        public_source_result_shape(&decision, true),
        PublicSourceResultShapeV1::Partial
    );
    assert!(
        decision
            .ordered_reason_codes
            .contains(&PolicyReasonCodeV1::ContentPartial)
    );
    assert!(
        !decision
            .ordered_reason_codes
            .contains(&PolicyReasonCodeV1::ContentAuthoritativeDeleted)
    );
}

#[test]
fn narrowing_a_grant_cannot_widen_an_authorization_decision() {
    let evaluator = SourceAuthorizationEvaluatorV1::default();
    let allowed = truth_tables()
        .into_iter()
        .find(|row| row.name == "project_authorized_live")
        .expect("allow fixture exists");
    let baseline = evaluator.evaluate(&allowed.input);
    assert_eq!(baseline.access, SourceAccessDecisionV1::Authorized);

    let mut narrowed = allowed.input;
    narrowed.requester_grant.disclosure_ceiling = DisclosureClassV1::Summary;
    let narrowed_decision = evaluator.evaluate(&narrowed);

    assert_ne!(narrowed_decision.access, SourceAccessDecisionV1::Authorized);
    assert!(narrowed_decision.effective_grant.is_none());
}

#[test]
fn effective_grant_is_narrowed_to_the_exact_requested_authority() {
    let evaluator = SourceAuthorizationEvaluatorV1::default();
    let allowed = truth_tables()
        .into_iter()
        .find(|row| row.name == "project_authorized_live")
        .expect("allow fixture exists");
    let decision = evaluator.evaluate(&allowed.input);
    let effective = decision.effective_grant.expect("effective grant");

    assert_eq!(
        effective.disclosure_ceiling,
        allowed.input.requested_access.disclosure
    );
    assert_eq!(effective.budgets, allowed.input.requested_access.budget);
}

#[test]
fn sink_policy_must_describe_the_requested_sink() {
    let evaluator = SourceAuthorizationEvaluatorV1::default();
    let mut input = truth_tables()
        .into_iter()
        .find(|row| row.name == "project_authorized_live")
        .expect("allow fixture exists")
        .input;
    input.sink_policy.sink = SinkKindV1::HostDelivery;

    let decision = evaluator.evaluate(&input);

    assert_eq!(decision.access, SourceAccessDecisionV1::Unauthorized);
    assert_eq!(
        decision.ordered_reason_codes,
        vec![
            PolicyReasonCodeV1::InputComplete,
            PolicyReasonCodeV1::SinkPolicySinkMismatch,
        ]
    );
}

#[test]
fn mutated_decision_cannot_issue_an_opaque_source_proof() {
    let evaluator = SourceAuthorizationEvaluatorV1::default();
    let input = truth_tables()
        .into_iter()
        .find(|row| row.name == "project_authorized_live")
        .expect("allow fixture exists")
        .input;
    let mut decision = evaluator.evaluate(&input);
    decision
        .effective_grant
        .as_mut()
        .expect("effective grant")
        .budgets = input.requester_grant.budgets.clone();

    assert!(issue_source_authorization_proof(&evaluator, &input, &decision).is_none());
}

#[test]
fn deleted_content_requires_historical_read_authority_before_sink_admission() {
    let evaluator = SourceAuthorizationEvaluatorV1::default();
    let mut input = truth_tables()
        .into_iter()
        .find(|row| row.name == "project_authorized_live")
        .expect("allow fixture exists")
        .input;
    input.content_status = ExternalContentStatusV1::AuthoritativeDeleted;

    let deleted = evaluator.evaluate(&input);
    assert_eq!(deleted.access, SourceAccessDecisionV1::Authorized);
    assert!(issue_source_authorization_proof(&evaluator, &input, &deleted).is_none());

    input.requested_access.operation = TypedOperationV1::HistoricalRead;
    input
        .source_grant
        .operations
        .insert(TypedOperationV1::HistoricalRead);
    input
        .requester_grant
        .operations
        .insert(TypedOperationV1::HistoricalRead);
    input
        .source_policy
        .eligible_operations
        .insert(TypedOperationV1::HistoricalRead);
    let historical = evaluator.evaluate(&input);

    assert!(issue_source_authorization_proof(&evaluator, &input, &historical).is_some());
}

#[test]
fn unauthorized_public_result_is_indistinguishable_from_not_found() {
    let evaluator = SourceAuthorizationEvaluatorV1::default();
    let denied = truth_tables()
        .into_iter()
        .find(|row| row.name == "project_owner_mismatch")
        .expect("owner-mismatch fixture exists");
    let decision = evaluator.evaluate(&denied.input);
    let public_shape = public_source_result_shape(&decision, denied.source_visible);

    assert_eq!(
        public_shape,
        PublicSourceResultShapeV1::NotFoundOrNotAuthorized
    );
    assert_eq!(
        serde_json::to_value(public_shape).expect("public shape serializes"),
        json!("not_found_or_not_authorized")
    );
}

#[test]
fn replay_modes_preserve_recorded_or_name_current_substitutions() {
    let evaluator = SourceAuthorizationEvaluatorV1::default();
    let row = truth_tables()
        .into_iter()
        .find(|row| row.name == "project_authorized_live")
        .expect("allow fixture exists");
    let recorded_decision = evaluator.evaluate(&row.input);
    let recorded = SourceAuthorizationRecordedResultV1::new(
        evaluator.version().clone(),
        row.input.clone(),
        recorded_decision.clone(),
    );

    let exact = replay_source_authorization(
        &evaluator,
        SourceAuthorizationReplayRequestV1 {
            mode: ReplayModeV1::ExactDeterministic,
            recorded: recorded.clone(),
            current_input: None,
        },
    );
    assert_eq!(exact.decision, Some(recorded_decision.clone()));
    assert!(exact.substitutions.is_empty());

    let recorded_result = replay_source_authorization(
        &evaluator,
        SourceAuthorizationReplayRequestV1 {
            mode: ReplayModeV1::RecordedResult,
            recorded: recorded.clone(),
            current_input: None,
        },
    );
    assert_eq!(recorded_result.decision, Some(recorded_decision));
    assert!(recorded_result.substitutions.is_empty());

    let mut current_input = row.input;
    current_input.configuration_digest =
        ManifestDigest::new(format!("sha256:{}", "f".repeat(64))).expect("fixture digest");
    let current = replay_source_authorization(
        &evaluator,
        SourceAuthorizationReplayRequestV1 {
            mode: ReplayModeV1::CurrentBestEffort,
            recorded,
            current_input: Some(current_input),
        },
    );
    assert!(
        current
            .substitutions
            .contains(&ReplaySubstitutionV1::ConfigurationDigest)
    );
}

#[test]
fn exact_replay_refuses_missing_inputs_and_detects_recorded_drift() {
    let evaluator = SourceAuthorizationEvaluatorV1::default();
    let row = truth_tables()
        .into_iter()
        .find(|row| row.name == "project_authorized_live")
        .expect("allow fixture exists");
    let decision = evaluator.evaluate(&row.input);

    let mut missing_input = row.input.clone();
    missing_input.snapshot_state = AuthorizationSnapshotStateV1::Missing;
    let missing_decision = evaluator.evaluate(&missing_input);
    let missing = replay_source_authorization(
        &evaluator,
        SourceAuthorizationReplayRequestV1 {
            mode: ReplayModeV1::ExactDeterministic,
            recorded: SourceAuthorizationRecordedResultV1::new(
                evaluator.version().clone(),
                missing_input,
                missing_decision,
            ),
            current_input: None,
        },
    );
    assert_eq!(
        missing.ordered_reason_codes,
        vec![PolicyReasonCodeV1::ReplayInputsMissing]
    );
    assert!(missing.decision.is_none());

    struct SubstitutedEvaluator {
        base: SourceAuthorizationEvaluatorV1,
        version: tracedecay_policy::authorization::PolicyEvaluatorVersionV1,
    }

    impl SourceAuthorizationEvaluator for SubstitutedEvaluator {
        fn evaluator_version(&self) -> &tracedecay_policy::authorization::PolicyEvaluatorVersionV1 {
            &self.version
        }

        fn evaluate(
            &self,
            input: &tracedecay_policy::authorization::SourceAuthorizationInputV1,
        ) -> tracedecay_policy::authorization::SourceAuthorizationDecisionV1 {
            self.base.evaluate(input)
        }
    }

    let mut substituted_version = evaluator.version().clone();
    substituted_version.evaluator_revision += 1;
    let version_mismatch = replay_source_authorization(
        &SubstitutedEvaluator {
            base: SourceAuthorizationEvaluatorV1::default(),
            version: substituted_version,
        },
        SourceAuthorizationReplayRequestV1 {
            mode: ReplayModeV1::ExactDeterministic,
            recorded: SourceAuthorizationRecordedResultV1::new(
                evaluator.version().clone(),
                row.input.clone(),
                decision.clone(),
            ),
            current_input: None,
        },
    );
    assert_eq!(
        version_mismatch.ordered_reason_codes,
        vec![PolicyReasonCodeV1::ReplayEvaluatorVersionMismatch]
    );
    assert!(version_mismatch.decision.is_none());

    struct DivergentEvaluator(SourceAuthorizationEvaluatorV1);

    impl SourceAuthorizationEvaluator for DivergentEvaluator {
        fn evaluator_version(&self) -> &tracedecay_policy::authorization::PolicyEvaluatorVersionV1 {
            self.0.version()
        }

        fn evaluate(
            &self,
            input: &tracedecay_policy::authorization::SourceAuthorizationInputV1,
        ) -> tracedecay_policy::authorization::SourceAuthorizationDecisionV1 {
            let mut decision = self.0.evaluate(input);
            decision
                .ordered_reason_codes
                .push(PolicyReasonCodeV1::InputInvalid);
            decision
        }
    }

    let drifted = replay_source_authorization(
        &DivergentEvaluator(SourceAuthorizationEvaluatorV1::default()),
        SourceAuthorizationReplayRequestV1 {
            mode: ReplayModeV1::ExactDeterministic,
            recorded: SourceAuthorizationRecordedResultV1::new(
                evaluator.version().clone(),
                row.input,
                decision,
            ),
            current_input: None,
        },
    );
    assert_eq!(
        drifted.ordered_reason_codes,
        vec![PolicyReasonCodeV1::ReplayDecisionMismatch]
    );
    assert!(drifted.decision.is_none());
}

#[test]
fn recorded_replay_refuses_a_tampered_decision_record() {
    let evaluator = SourceAuthorizationEvaluatorV1::default();
    let row = truth_tables()
        .into_iter()
        .find(|row| row.name == "project_authorized_live")
        .expect("allow fixture exists");
    let mut decision = evaluator.evaluate(&row.input);
    decision
        .ordered_reason_codes
        .push(PolicyReasonCodeV1::InputInvalid);

    let replay = replay_source_authorization(
        &evaluator,
        SourceAuthorizationReplayRequestV1 {
            mode: ReplayModeV1::RecordedResult,
            recorded: SourceAuthorizationRecordedResultV1::new(
                evaluator.version().clone(),
                row.input,
                decision,
            ),
            current_input: None,
        },
    );

    assert_eq!(
        replay.ordered_reason_codes,
        vec![PolicyReasonCodeV1::ReplayRecordInvalid]
    );
    assert!(replay.decision.is_none());
}
