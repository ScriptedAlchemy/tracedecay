use crate::common;

use tracedecay_contracts::retrieval::{
    CodeGraphReadFreshnessV1, ServedCodeGraphGenerationV1, SymbolGraphPage, SymbolPrimitiveRecord,
    SymbolRelationRecord,
};
use tracedecay_contracts::{
    APPLICATION_PROBLEM_REVISION, ApplicationEnvelope, ApplicationOutcome, ApplicationProblem,
    ApplicationProblemEnvelope, CoverageCompleteness, CoverageDomainState, EvidenceCoverage,
    EvidenceDomain, EvidencePacket, OperationReceipt, ProblemOwningLayer, ProblemTerminality,
    RetryDirective, RetryScope,
};
use tracedecay_domain::{CodeGenerationId, UtcMicros};

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

/// A code-graph read's packet names its serving generation on the envelope,
/// the one field every surface renders the stale-seat trailer from.
#[test]
fn code_graph_evidence_names_its_served_generation_on_the_envelope() {
    let operation = common::operation();
    let context = common::context(&operation);
    let envelope = |freshness: Option<CodeGraphReadFreshnessV1>| {
        let mut evidence = common::evidence(Vec::<String>::new());
        evidence.temporal.source_generation =
            Some(CodeGenerationId::new("generation.envelope.7").unwrap());
        evidence.temporal.code_graph_freshness = freshness;
        let receipt = OperationReceipt::completed(
            UtcMicros(2),
            UtcMicros(3),
            context.deadline().clone(),
            Default::default(),
        )
        .unwrap();
        ApplicationEnvelope::evidence(
            operation.result_contract().clone(),
            context.request_id().clone(),
            context.scope().clone(),
            EvidencePacket::from_retrieval(evidence, common::authority(&context), receipt).unwrap(),
        )
    };
    let stale = CodeGraphReadFreshnessV1::LastCompleteStale {
        sealed_at: UtcMicros(1),
        rebuild_in_flight: true,
    };
    let served = envelope(Some(stale));
    assert_eq!(
        served.code_graph,
        Some(ServedCodeGraphGenerationV1 {
            generation: "generation.envelope.7".to_owned(),
            freshness: stale,
        })
    );
    let wire = serde_json::to_value(&served).unwrap();
    assert_eq!(wire["code_graph"]["generation"], "generation.envelope.7");
    assert_eq!(
        wire["code_graph"]["freshness"]["state"],
        "last_complete_stale"
    );

    // A packet no code-graph read produced carries no serving generation.
    assert_eq!(envelope(None).code_graph, None);
}

#[test]
fn symbol_graph_pages_touch_each_symbol_file_once_in_page_order() {
    let record = |file: &str| SymbolPrimitiveRecord {
        node_id: format!("node.{file}"),
        name: "answer".to_owned(),
        qualified_name: format!("{file}::answer"),
        kind: "function".to_owned(),
        file: file.to_owned(),
        line: 1,
        end_line: 2,
        signature: None,
        is_async: false,
        score: None,
    };
    let page = SymbolGraphPage::complete(
        CodeGenerationId::new("generation.page.1").unwrap(),
        CodeGraphReadFreshnessV1::Current,
        vec![
            SymbolRelationRecord {
                symbol: record("src/b.rs"),
                edge_kind: "calls".to_owned(),
                dispatch_via_trait: false,
                dispatch_from: None,
                depth: Some(1),
            },
            SymbolRelationRecord {
                symbol: record("src/a.rs"),
                edge_kind: "calls".to_owned(),
                dispatch_via_trait: false,
                dispatch_from: None,
                depth: Some(1),
            },
            SymbolRelationRecord {
                symbol: record("src/b.rs"),
                edge_kind: "calls".to_owned(),
                dispatch_via_trait: false,
                dispatch_from: None,
                depth: Some(2),
            },
        ],
        Some(3),
        None,
    );
    assert_eq!(page.touched_files(), vec!["src/b.rs", "src/a.rs"]);
}

#[test]
fn pre_admission_problem_has_canonical_identity_and_semantics() {
    let operation = common::operation();
    let context = common::context(&operation);
    let problem = ApplicationProblemEnvelope::new(
        operation.result_contract().clone(),
        context.request_id().clone(),
        ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
    )
    .expect("not-found envelope is valid");

    let wire = serde_json::to_value(&problem).unwrap();
    assert_eq!(wire["problem"]["kind"], "not_found_or_not_authorized");
    assert_eq!(wire["problem"]["revision"], APPLICATION_PROBLEM_REVISION);
    assert_eq!(wire["problem"]["owning_layer"], "application");
    assert_eq!(wire["problem"]["terminality"], "pre_admission");
    assert_eq!(wire["problem"]["retryable"], false);
    assert_eq!(wire["problem"]["retry_scope"], serde_json::Value::Null);
    assert_eq!(
        wire["problem"]["retry_after_millis"],
        serde_json::Value::Null
    );
    assert_eq!(wire["problem"]["request_id"], "request.fixture");
    assert_eq!(wire["problem"]["trace_id"], "request.fixture");
    assert_eq!(wire["problem"]["details"], serde_json::json!([]));
    assert_eq!(wire["problem"]["legal_actions"], serde_json::json!([]));
    assert_eq!(wire["problem"]["coverage"], serde_json::Value::Null);
    assert_eq!(
        problem.problem.owning_layer,
        ProblemOwningLayer::Application
    );
    assert_eq!(
        problem.problem.terminality,
        ProblemTerminality::PreAdmission
    );
    assert_eq!(problem.problem.retry_scope, None::<RetryScope>);
    assert!(wire.get("outcome").is_none());
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

#[test]
fn problem_record_preserves_bounded_retry_and_partial_coverage() {
    let operation = common::operation();
    let context = common::context(&operation);
    let coverage = EvidenceCoverage {
        requested_domains: vec![EvidenceDomain::Symbol],
        visited: Some(4),
        eligible: Some(3),
        returned: 2,
        completeness: CoverageCompleteness::Partial,
        domains: vec![CoverageDomainState {
            domain: EvidenceDomain::Symbol,
            completeness: CoverageCompleteness::Partial,
        }],
    };
    let problem = ApplicationProblemEnvelope::new(
        operation.result_contract().clone(),
        context.request_id().clone(),
        ApplicationProblem::unavailable(
            tracedecay_contracts::SafeDiagnostic::new(
                "application.partial",
                "Only partial evidence is available.",
            )
            .unwrap(),
        ),
    )
    .expect("partial-coverage envelope is valid")
    .with_owning_layer(ProblemOwningLayer::Port)
    .with_retry_after_millis(Some(250))
    .unwrap()
    .with_coverage(coverage)
    .unwrap();

    let wire = serde_json::to_value(problem).unwrap();
    assert_eq!(wire["problem"]["owning_layer"], "port");
    assert_eq!(wire["problem"]["retry_after_millis"], 250);
    assert_eq!(wire["problem"]["coverage"]["completeness"], "partial");
    assert_eq!(wire["problem"]["coverage"]["returned"], 2);
}
