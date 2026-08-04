use serde_json::json;
use tracedecay_policy::authorization::{
    AuthorizationCoverageV1, DisclosureClassV1, ExternalContentStatusV1, PolicyIdentifierV1,
    PolicyReasonCodeV1, PublicSourceResultShapeV1, SinkKindV1, SourceAccessDecisionV1,
    SourceAuthorizationEvaluator, SourceAuthorizationEvaluatorV1, SourceAuthorizationTruthTableV1,
    TypedOperationV1, issue_source_authorization_proof, public_source_result_shape,
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
