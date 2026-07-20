use tracedecay_domain::ManifestDigest;
use tracedecay_policy::authorization::{
    ExternalContentStatusV1, GrantStateV1, PolicyReasonCodeV1, SinkRecheckDispositionV1,
    SourceAuthorizationEvaluator, SourceAuthorizationEvaluatorV1, SourceAuthorizationTruthTableV1,
    issue_source_authorization_proof, recheck_sink_admission,
};

const SOURCE_AUTHORIZATION_TRUTH_TABLES: &str =
    include_str!("fixtures/source_authorization/core.json");

fn authorized_input() -> tracedecay_policy::authorization::SourceAuthorizationInputV1 {
    serde_json::from_str::<Vec<SourceAuthorizationTruthTableV1>>(SOURCE_AUTHORIZATION_TRUTH_TABLES)
        .expect("checked-in source authorization truth tables deserialize")
        .into_iter()
        .find(|row| row.name == "project_authorized_live")
        .expect("allow fixture exists")
        .input
}

#[test]
fn sink_recheck_issues_a_fresh_proof_only_for_unchanged_authority() {
    let evaluator = SourceAuthorizationEvaluatorV1::default();
    let input = authorized_input();
    let decision = evaluator.evaluate(&input);
    let proof = issue_source_authorization_proof(&evaluator, &input, &decision)
        .expect("an allow decision produces an internal source proof");

    let mut current = input;
    current.evaluated_at.0 += 1;
    let recheck = recheck_sink_admission(&evaluator, &proof, &current);

    assert_eq!(recheck.disposition, SinkRecheckDispositionV1::Admit);
    assert!(recheck.admission_proof().is_some());
}

#[test]
fn sink_policy_drift_invalidates_the_old_proof() {
    let evaluator = SourceAuthorizationEvaluatorV1::default();
    let input = authorized_input();
    let decision = evaluator.evaluate(&input);
    let proof = issue_source_authorization_proof(&evaluator, &input, &decision)
        .expect("an allow decision produces an internal source proof");

    let mut current = input;
    current.sink_policy.policy_revision += 1;
    current.sink_policy.policy_digest =
        ManifestDigest::new(format!("sha256:{}", "9".repeat(64))).expect("fixture digest");
    let recheck = recheck_sink_admission(&evaluator, &proof, &current);

    assert_eq!(recheck.disposition, SinkRecheckDispositionV1::Deny);
    assert_eq!(
        recheck.ordered_reason_codes,
        vec![PolicyReasonCodeV1::SinkPolicyDrift]
    );
    assert!(recheck.admission_proof().is_none());
}

#[test]
fn revoked_grant_between_evaluation_and_sink_recheck_is_denied() {
    let evaluator = SourceAuthorizationEvaluatorV1::default();
    let input = authorized_input();
    let decision = evaluator.evaluate(&input);
    let proof = issue_source_authorization_proof(&evaluator, &input, &decision)
        .expect("an allow decision produces an internal source proof");

    let mut current = input;
    current.requester_grant.state = GrantStateV1::Revoked;
    current.evaluated_at.0 += 1;
    let recheck = recheck_sink_admission(&evaluator, &proof, &current);

    assert_eq!(recheck.disposition, SinkRecheckDispositionV1::Deny);
    assert_eq!(
        recheck.ordered_reason_codes,
        vec![
            PolicyReasonCodeV1::InputComplete,
            PolicyReasonCodeV1::SourceGrantActive,
            PolicyReasonCodeV1::RequesterGrantRevoked,
        ]
    );
    assert!(recheck.admission_proof().is_none());
}

#[test]
fn authoritative_deletion_requires_historical_authority_at_sink_recheck() {
    let evaluator = SourceAuthorizationEvaluatorV1::default();
    let input = authorized_input();
    let decision = evaluator.evaluate(&input);
    let proof = issue_source_authorization_proof(&evaluator, &input, &decision)
        .expect("an allow decision produces an internal source proof");

    let mut current = input;
    current.content_status = ExternalContentStatusV1::AuthoritativeDeleted;
    current.evaluated_at.0 += 1;
    let recheck = recheck_sink_admission(&evaluator, &proof, &current);

    assert_eq!(recheck.disposition, SinkRecheckDispositionV1::Deny);
    assert_eq!(
        recheck.ordered_reason_codes,
        vec![PolicyReasonCodeV1::AuthorizationInputDrift]
    );
    assert!(recheck.admission_proof().is_none());
}
