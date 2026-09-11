use serde_json::json;
use tracedecay_policy::authorization::{
    AuthorizationCoverageV1, DisclosureClassV1, ExternalContentStatusV1, PolicyIdentifierV1,
    PolicyReasonCodeV1, PublicSourceResultShapeV1, SinkKindV1, SourceAccessDecisionV1,
    SourceAuthorizationEvaluator, SourceAuthorizationEvaluatorV1, SourceAuthorizationTruthTableV1,
    TypedOperationV1, issue_source_authorization_proof, public_source_result_shape,
};
const SOURCE_AUTHORIZATION_TRUTH_TABLES: &str =
    include_str!("../fixtures/source_authorization/core.json");

/// Byte-exact `(input_digest, decision_digest)` per truth-table row. These
/// are the digests the evaluator produced before decision construction was
/// reworked to hash the decision material once; replay, proof issuance, and
/// sink recheck all bind to these bytes, so a change here is a contract break,
/// not a refactor.
const PINNED_DECISION_DIGESTS: &[(&str, &str, &str)] = &[
    (
        "project_authorized_live",
        "sha256:ebd80c5f30861d41091cac8136112220a51b11c65d24fa9d39cf5b15a543a6fe",
        "sha256:cc0659b2bf7f5064a771c0bc98c455308a8669cb58f5807edd732b112083849d",
    ),
    (
        "project_owner_mismatch",
        "sha256:2a31d316680e54279e4e233317c5262c7d424a23626a8cd0d768a7ca122882b2",
        "sha256:0ced4c613a4cf96bce197747246c9a7b50a93d2e90cfa9a8d1f6fc8b928b7223",
    ),
    (
        "mandatory_local_privacy_blocks_host_egress",
        "sha256:96302fa59d141d0a383843c09c76ac0ac36b4d151502795d0deb8690e34aed33",
        "sha256:59d50e411a22c56bab7dc5acfd3cfbb7e6ad6116b984d14b2e45db500451a6cc",
    ),
    (
        "expired_requester_grant",
        "sha256:a72cf235938cd39fbf950eb8a7892435ab13bb0e9e98280b0dcf9fccf3fff555",
        "sha256:00bf07d143ed55b229fd9adf327d31e69ff1dc788540e5037bdc7569aa6763af",
    ),
    (
        "temporarily_unavailable_is_not_deletion",
        "sha256:5201a04cbcd8e18a7f62ef87807b8909d3b2cf3c22f80545f5dd0bd5f035acc6",
        "sha256:ddaa027062de71c36d8c18ef5044f183851609fb90c59a6730c1c7716b2fa047",
    ),
    (
        "policy_excluded_is_not_unauthorized",
        "sha256:633d4a84397a2d1ccd202b6f6189af5aa83b65f55cf6b70c93b6ecc92b74603c",
        "sha256:f460d6e5b9647aecb9d848e03584549fcf3c4223c8addbc1e29849cc0938ae90",
    ),
];

fn truth_tables() -> Vec<SourceAuthorizationTruthTableV1> {
    serde_json::from_str(SOURCE_AUTHORIZATION_TRUTH_TABLES)
        .expect("checked-in source authorization truth tables deserialize")
}

#[test]
fn canonical_source_authorization_truth_tables_hold() {
    let evaluator = SourceAuthorizationEvaluatorV1::default();

    for row in truth_tables() {
        let decision = evaluator.evaluate(&row.input);
        let (_, input_digest, decision_digest) = PINNED_DECISION_DIGESTS
            .iter()
            .find(|(name, _, _)| *name == row.name)
            .unwrap_or_else(|| panic!("truth-table row {} has no pinned digests", row.name));
        assert_eq!(
            decision.input_digest.as_str(),
            *input_digest,
            "input digest drifted for {}",
            row.name
        );
        assert_eq!(
            decision.decision_digest.as_str(),
            *decision_digest,
            "decision digest drifted for {}",
            row.name
        );

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
