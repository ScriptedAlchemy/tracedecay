mod common;

use tracedecay_application::{
    ApplicationEnvelope, ApplicationOutcome, ApplicationProblem, ApplicationProblemEnvelope,
    CoverageCompleteness, CoverageDomainState, EvidenceCoverage, EvidenceDomain, EvidencePacket,
    OperationReceipt, RetryDirective,
};
use tracedecay_domain::UtcMicros;

#[test]
fn completed_empty_evidence_remains_explicit_and_authorized() {
    let operation = common::operation();
    let context = common::context(&operation);
    let receipt = OperationReceipt::completed(
        UtcMicros(2),
        UtcMicros(3),
        context.deadline().clone(),
        Default::default(),
    )
    .unwrap();
    let packet = EvidencePacket::from_retrieval(
        common::evidence(Vec::<String>::new()),
        common::authority(&context),
        receipt,
    )
    .unwrap();
    let envelope = ApplicationEnvelope::evidence(
        operation.result_contract().clone(),
        context.request_id().clone(),
        context.scope().clone(),
        packet,
    );

    assert!(matches!(
        envelope.outcome,
        ApplicationOutcome::Evidence(ref packet) if packet.is_truthful_complete_empty()
    ));

    let wire = serde_json::to_value(&envelope).unwrap();
    assert_eq!(wire["contract"]["schema_revision"], 1);
    assert_eq!(wire["outcome"]["outcome"], "evidence");
    assert_eq!(wire["outcome"]["value"]["payload"], serde_json::json!([]));
}

#[test]
fn pre_admission_problem_has_no_execution_or_evidence_fields() {
    let operation = common::operation();
    let context = common::context(&operation);
    let problem = ApplicationProblemEnvelope::new(
        operation.result_contract().clone(),
        context.request_id().clone(),
        ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
    );

    let wire = serde_json::to_value(problem).unwrap();
    assert_eq!(wire["problem"]["kind"], "not_found_or_not_authorized");
    assert!(wire.get("outcome").is_none());
    assert!(wire.get("coverage").is_none());
    assert!(wire.get("execution").is_none());
}

#[test]
fn coverage_rejects_domain_states_for_unrequested_evidence() {
    let coverage = EvidenceCoverage {
        requested_domains: vec![EvidenceDomain::Symbol],
        visited: Some(1),
        eligible: Some(1),
        returned: 1,
        completeness: CoverageCompleteness::Complete,
        domains: vec![CoverageDomainState {
            domain: EvidenceDomain::Graph,
            completeness: CoverageCompleteness::Complete,
        }],
    };

    assert!(coverage.validate().is_err());
}
