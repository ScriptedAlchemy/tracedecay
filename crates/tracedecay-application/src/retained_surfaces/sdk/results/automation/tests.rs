use super::{
    AutomationCommittedReceiptV1, AutomationRunProblemV1, AutomationRunResultV1,
    AutomationSkipReasonV1, AutomationTaskV1, MemoryAutomationCurationRelationV1,
};
use serde_json::{Value, json};
use tracedecay_domain::{
    ActorId, FactId, FactIdentityMaterialV1, FactIdentitySourceV1, FactOwnerV1, ManifestDigest,
    ProjectId, ProvenanceId, RepositoryId, WorktreeId, canonical_sha256,
};
use tracedecay_tool_catalog::EffectClass;
mod admission_binding;
mod outer_partial;
use crate::retained_surfaces::{
    AutomationRunRequestV1, RetainedSurfaceExecutionErrorV1, RetainedSurfaceOperation,
    retained_surface_application_operation, retained_surface_execution_problem,
};
use crate::{
    ApplicationExecutionFailureClassV1, ApplicationProblem, ApplicationProblemEnvelope,
    ApplicationUnavailableClassV1, CancellationStage, EffectReceipt, EffectTermination,
    IdempotencyKey, LegalAction, RequestId, ResolvedScope, RetryDirective, SafeDiagnostic,
};

fn zero_terminal(status: &str) -> Value {
    let terminal = if status == "completed" {
        json!({"status":"completed","summary":{"reviewed_count":0,"accepted_count":0,"rejected_count":0,"skipped_count":0}})
    } else {
        json!({"status":"skipped","reason":"nothing_to_review","summary":{"reviewed_count":0,"accepted_count":0,"rejected_count":0,"skipped_count":1}})
    };
    with_request_digest(
        json!({"run_id":"run.memory.zero","task":"memory_curator","terminal":terminal,"committed_receipts":[]}),
        &automation_request("run.memory.zero", AutomationTaskV1::MemoryCurator),
    )
}

pub(crate) fn automation_request(run_id: &str, task: AutomationTaskV1) -> AutomationRunRequestV1 {
    let reflector = json!({
        "provider":"codex","query":"canonical evidence","scope":"all","session_id":null,
        "include_summaries":true,"evidence_limit":10,"include_recent_sessions":true,
        "recent_sessions_limit":3,"sort":"recency","source":null,"role":null,
        "start_time":null,"end_time":null
    });
    let skill = json!({
        "provider":"codex","query":"canonical skill evidence","evidence_limit":10,
        "include_recent_sessions":true,"recent_sessions_limit":3
    });
    let (kind, options) = match task {
        AutomationTaskV1::MemoryCurator => (
            "memory_curator",
            json!({
                "fact_review_limit":24,"min_confidence_millionths":720000
            }),
        ),
        AutomationTaskV1::SessionReflector => ("session_reflector", reflector),
        AutomationTaskV1::SkillWriter => ("skill_writer", skill),
        AutomationTaskV1::CombinedReview => (
            "combined_review",
            json!({
                "session_reflector":reflector,"skill_writer":skill
            }),
        ),
        AutomationTaskV1::UserJob => ("user_job", json!({"job_id":"nightly"})),
    };
    serde_json::from_value(json!({
        "run_id":run_id,"task":{"kind":kind,"options":options}
    }))
    .expect("automation request fixture")
}

pub(crate) fn with_request_digest(mut value: Value, request: &AutomationRunRequestV1) -> Value {
    value["request_digest"] = json!(request.input_digest().expect("request digest").as_str());
    value
}

#[test]
fn skipped_terminal_rejects_a_committed_receipt() {
    let mut terminal = automatic_fact_terminal();
    terminal["terminal"] = zero_terminal("skipped")["terminal"].clone();
    let terminal = serde_json::from_value::<AutomationRunResultV1>(terminal)
        .expect("typed but inconsistent skipped terminal");
    assert!(!terminal.matches_terminal());
}

#[test]
fn terminal_rejects_removed_open_fields() {
    for (field, value) in [
        ("run", json!({"accepted":0})),
        ("reconciliation", json!("reconciled")),
    ] {
        let mut legacy = zero_terminal("completed");
        legacy[field] = value;
        assert!(serde_json::from_value::<AutomationRunResultV1>(legacy).is_err());
    }
}

#[test]
fn automatic_fact_receipt_binds_command_target_task_and_summary() {
    let receipt = automatic_fact_terminal();
    let result = serde_json::from_value::<AutomationRunResultV1>(receipt.clone())
        .expect("exact automatic fact terminal");
    assert!(result.matches_terminal());
    for pointer in [
        "/committed_receipts/0/receipt/automation_run_id",
        "/committed_receipts/0/receipt/effect/target/fact_id",
    ] {
        let mut mismatched = receipt.clone();
        *mismatched.pointer_mut(pointer).expect("identity pointer") = json!("fact.profile.wrong");
        let mismatched =
            serde_json::from_value::<AutomationRunResultV1>(mismatched).expect("typed mismatch");
        assert!(!mismatched.matches_terminal());
    }
    let mut wrong_task = receipt.clone();
    wrong_task["task"] = json!("memory_curator");
    assert!(
        !serde_json::from_value::<AutomationRunResultV1>(wrong_task)
            .expect("typed cross-task receipt")
            .matches_terminal()
    );
    let mut wrong_count = receipt;
    wrong_count["terminal"]["summary"]["accepted_count"] = json!(0);
    wrong_count["terminal"]["summary"]["rejected_count"] = json!(1);
    assert!(
        !serde_json::from_value::<AutomationRunResultV1>(wrong_count)
            .expect("typed count mismatch")
            .matches_terminal()
    );
}

#[test]
fn duplicate_automatic_receipt_is_not_a_second_effect() {
    let mut terminal = automatic_fact_terminal();
    let duplicate = terminal["committed_receipts"][0].clone();
    terminal["committed_receipts"]
        .as_array_mut()
        .expect("receipt list")
        .push(duplicate);
    terminal["terminal"]["summary"]["reviewed_count"] = json!(2);
    terminal["terminal"]["summary"]["accepted_count"] = json!(2);
    assert!(
        !serde_json::from_value::<AutomationRunResultV1>(terminal)
            .expect("typed duplicate receipt")
            .matches_terminal()
    );
}

#[test]
fn curation_receipt_binds_outer_run_and_inner_commits() {
    let terminal = curation_terminal();
    assert!(
        serde_json::from_value::<AutomationRunResultV1>(terminal.clone())
            .expect("canonical curation terminal")
            .matches_terminal()
    );
    let mut wrong_run = terminal.clone();
    wrong_run["committed_receipts"][0]["receipt"]["receipt"]["automation_run_id"] =
        json!("run.memory.wrong");
    assert!(
        !serde_json::from_value::<AutomationRunResultV1>(wrong_run)
            .expect("typed wrong run")
            .matches_terminal()
    );
    let mut wrong_fact = terminal;
    wrong_fact["committed_receipts"][0]["receipt"]["receipt"]["changed_fact_ids"] =
        json!([project_fact_id("curation"), project_fact_id("other")]);
    let mut wrong_result =
        serde_json::from_value::<AutomationRunResultV1>(wrong_fact).expect("typed extra fact");
    let AutomationCommittedReceiptV1::Curation(receipt) = &mut wrong_result.committed_receipts[0]
    else {
        panic!("curation receipt fixture")
    };
    receipt.canonical_digest = receipt.canonical_digest().expect("digest");
    assert!(!wrong_result.matches_terminal());
}

#[test]
fn linked_curation_receipt_requires_the_exact_ordered_endpoint_union() {
    let terminal = linked_curation_terminal("supports");
    let source = terminal["committed_receipts"][0]["receipt"]["receipt"]
        ["operation_effects"][0]["source_fact_id"]
        .clone();
    let target = terminal["committed_receipts"][0]["receipt"]["receipt"]
        ["operation_effects"][0]["target_fact_id"]
        .clone();
    assert!(
        serde_json::from_value::<AutomationRunResultV1>(terminal.clone())
            .expect("canonical linked curation terminal")
            .matches_terminal()
    );
    for changed_fact_ids in [
        json!([source.clone()]),
        json!([source.clone(), project_fact_id("substituted")]),
        json!([target, source]),
    ] {
        let mut changed = terminal.clone();
        changed["committed_receipts"][0]["receipt"]["receipt"]["changed_fact_ids"] =
            changed_fact_ids;
        let changed = with_current_curation_digest(changed);
        assert!(
            !serde_json::from_value::<AutomationRunResultV1>(changed)
                .expect("typed changed endpoint union")
                .matches_terminal()
        );
    }
}

#[test]
fn curation_receipt_rejects_a_duplicate_normalize_effect_with_fresh_events() {
    let mut terminal = curation_terminal();
    let mut duplicate =
        terminal["committed_receipts"][0]["receipt"]["receipt"]["operation_effects"][0].clone();
    duplicate["commit"]["committed_event_ids"] = json!([
        "event.curation.duplicate.assertion",
        "event.curation.duplicate.fact"
    ]);
    duplicate["commit"]["last_event_id"] = json!("event.curation.duplicate.fact");
    terminal["committed_receipts"][0]["receipt"]["receipt"]["operation_effects"]
        .as_array_mut()
        .expect("operation effects")
        .push(duplicate);
    terminal["committed_receipts"][0]["receipt"]["receipt"]["normalized_tags"] = json!(2);
    terminal["committed_receipts"][0]["receipt"]["receipt"]["accepted_operations"] = json!(2);
    terminal["terminal"]["summary"]["reviewed_count"] = json!(2);
    terminal["terminal"]["summary"]["accepted_count"] = json!(2);
    let terminal = with_current_curation_digest(terminal);
    assert!(
        !serde_json::from_value::<AutomationRunResultV1>(terminal)
            .expect("typed duplicate commit")
            .matches_terminal()
    );
}

#[test]
fn curation_receipt_rejects_a_duplicate_link_effect_with_a_fresh_event() {
    let mut terminal = linked_curation_terminal("supports");
    let mut duplicate =
        terminal["committed_receipts"][0]["receipt"]["receipt"]["operation_effects"][0].clone();
    duplicate["commit"]["committed_event_ids"] = json!(["event.curation.link.duplicate"]);
    duplicate["commit"]["last_event_id"] = json!("event.curation.link.duplicate");
    terminal["committed_receipts"][0]["receipt"]["receipt"]["operation_effects"]
        .as_array_mut()
        .expect("operation effects")
        .push(duplicate);
    terminal["committed_receipts"][0]["receipt"]["receipt"]["facts_linked"] = json!(2);
    terminal["committed_receipts"][0]["receipt"]["receipt"]["accepted_operations"] = json!(2);
    terminal["terminal"]["summary"]["reviewed_count"] = json!(2);
    terminal["terminal"]["summary"]["accepted_count"] = json!(2);
    let terminal = with_current_curation_digest(terminal);
    assert!(
        !serde_json::from_value::<AutomationRunResultV1>(terminal)
            .expect("typed duplicate link effect")
            .matches_terminal()
    );
}

#[test]
fn curation_receipt_requires_last_event_id_to_be_the_ordered_tail() {
    let mut terminal = curation_terminal();
    terminal["committed_receipts"][0]["receipt"]["receipt"]["operation_effects"][0]["commit"]["committed_event_ids"] =
        json!(["event.curation", "event.curation.actual-tail"]);
    let terminal = with_current_curation_digest(terminal);

    assert!(
        !serde_json::from_value::<AutomationRunResultV1>(terminal)
            .expect("typed non-tail last event")
            .matches_terminal()
    );
}

#[test]
fn curation_effects_require_their_exact_event_cardinality() {
    let mut normalize = curation_terminal();
    normalize["committed_receipts"][0]["receipt"]["receipt"]["operation_effects"][0]["commit"]["committed_event_ids"] =
        json!(["event.curation.assertion"]);
    normalize = with_current_curation_digest(normalize);
    assert!(
        !serde_json::from_value::<AutomationRunResultV1>(normalize)
            .expect("typed one-event normalization")
            .matches_terminal()
    );

    let mut link = linked_curation_terminal("supports");
    link["committed_receipts"][0]["receipt"]["receipt"]["operation_effects"][0]["commit"]["committed_event_ids"] =
        json!(["event.curation.link.first", "event.curation.link"]);
    link = with_current_curation_digest(link);
    assert!(
        !serde_json::from_value::<AutomationRunResultV1>(link)
            .expect("typed two-event link")
            .matches_terminal()
    );
}

#[test]
fn curation_effects_are_bounded_and_share_one_commit_disposition() {
    let mut mixed_disposition = curation_terminal();
    let mut replay =
        mixed_disposition["committed_receipts"][0]["receipt"]["receipt"]["operation_effects"][0]
            .clone();
    replay["commit"]["disposition"] = json!("idempotent_replay");
    replay["commit"]["fact_id"] = json!(project_fact_id("replay"));
    replay["fact_id"] = json!(project_fact_id("replay"));
    replay["commit"]["committed_event_ids"] = json!([
        "event.curation.replay.fact",
        "event.curation.replay.assertion"
    ]);
    replay["commit"]["last_event_id"] = json!("event.curation.replay.assertion");
    mixed_disposition["committed_receipts"][0]["receipt"]["receipt"]["operation_effects"]
        .as_array_mut()
        .expect("operation effects")
        .push(replay);
    mixed_disposition["committed_receipts"][0]["receipt"]["receipt"]["changed_fact_ids"] =
        json!([project_fact_id("curation"), project_fact_id("replay")]);
    mixed_disposition["committed_receipts"][0]["receipt"]["receipt"]["normalized_tags"] = json!(2);
    mixed_disposition["committed_receipts"][0]["receipt"]["receipt"]["accepted_operations"] =
        json!(2);
    mixed_disposition["terminal"]["summary"]["reviewed_count"] = json!(2);
    mixed_disposition["terminal"]["summary"]["accepted_count"] = json!(2);
    let mixed_disposition = with_current_curation_digest(mixed_disposition);
    assert!(
        !serde_json::from_value::<AutomationRunResultV1>(mixed_disposition)
            .expect("typed mixed commit dispositions")
            .matches_terminal()
    );

    let mut oversized = curation_terminal();
    let template =
        oversized["committed_receipts"][0]["receipt"]["receipt"]["operation_effects"][0].clone();
    let mut effects = Vec::with_capacity(257);
    let mut changed = Vec::with_capacity(257);
    for index in 0..257 {
        let fact_id = project_fact_id(&format!("bounded-{index}"));
        let mut effect = template.clone();
        effect["fact_id"] = json!(fact_id.clone());
        effect["commit"]["fact_id"] = json!(fact_id.clone());
        effect["commit"]["committed_event_ids"] = json!([
            format!("event.curation.bounded.{index}.fact"),
            format!("event.curation.bounded.{index}.assertion")
        ]);
        effect["commit"]["last_event_id"] =
            json!(format!("event.curation.bounded.{index}.assertion"));
        effects.push(effect);
        changed.push(fact_id);
    }
    oversized["committed_receipts"][0]["receipt"]["receipt"]["operation_effects"] = json!(effects);
    oversized["committed_receipts"][0]["receipt"]["receipt"]["changed_fact_ids"] = json!(changed);
    oversized["committed_receipts"][0]["receipt"]["receipt"]["normalized_tags"] = json!(257);
    oversized["committed_receipts"][0]["receipt"]["receipt"]["accepted_operations"] = json!(257);
    oversized["terminal"]["summary"]["reviewed_count"] = json!(257);
    oversized["terminal"]["summary"]["accepted_count"] = json!(257);
    let oversized = with_current_curation_digest(oversized);
    assert!(
        !serde_json::from_value::<AutomationRunResultV1>(oversized)
            .expect("typed oversized curation receipt")
            .matches_terminal()
    );
}

#[test]
fn linked_curation_receipt_is_closed_and_semantically_bounded() {
    let terminal = linked_curation_terminal("supports");
    let relation = &terminal["committed_receipts"][0]["receipt"]["receipt"]["operation_effects"][0]
        ["relation"];
    assert!(relation.get("metadata").is_none());
    assert!(relation["provenance"].get("metadata").is_none());

    let mut raw_metadata = terminal.clone();
    raw_metadata["committed_receipts"][0]["receipt"]["receipt"]["operation_effects"][0]["relation"]
        ["provenance"]["metadata"] = json!({"forbidden":"raw"});
    assert!(serde_json::from_value::<AutomationRunResultV1>(raw_metadata).is_err());

    for (pointer, value) in [
        (
            "/committed_receipts/0/receipt/receipt/operation_effects/0/relation/evidence_fact_ids",
            json!([]),
        ),
        (
            "/committed_receipts/0/receipt/receipt/operation_effects/0/relation/confidence_millionths",
            json!(1_000_001),
        ),
        (
            "/committed_receipts/0/receipt/receipt/operation_effects/0/relation/provenance/source_label",
            json!(" automation:memory-curator"),
        ),
        (
            "/committed_receipts/0/receipt/receipt/operation_effects/0/relation/provenance/sanitization_receipt/payload/byte_len",
            json!(0),
        ),
    ] {
        let mut invalid = terminal.clone();
        *invalid.pointer_mut(pointer).expect("relation field") = value;
        invalid = with_current_curation_digest(invalid);
        assert!(
            !serde_json::from_value::<AutomationRunResultV1>(invalid)
                .expect("typed invalid relation receipt")
                .matches_terminal()
        );
    }
}

#[test]
fn curation_relation_schema_exposes_only_sanitizer_bound_provenance() {
    let schema = serde_json::to_value(schemars::schema_for!(MemoryAutomationCurationRelationV1))
        .expect("curation relation schema");
    let properties = schema["properties"]
        .as_object()
        .expect("curation relation properties");
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        properties.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "confidence_millionths",
            "evidence_fact_ids",
            "kind",
            "provenance"
        ]
    );
    let provenance = &schema["$defs"]["MemoryAutomationCurationRelationProvenanceV1"];
    let provenance_properties = provenance["properties"]
        .as_object()
        .expect("curation provenance properties");
    assert_eq!(provenance["additionalProperties"], false);
    assert_eq!(
        provenance_properties
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["sanitization_receipt", "source_label"]
    );
}

#[test]
fn linked_curation_receipt_preserves_every_canonical_relation_kind() {
    for relation in ["supports", "contradicts", "supersedes", "derived_from"] {
        assert!(
            serde_json::from_value::<AutomationRunResultV1>(linked_curation_terminal(relation))
                .expect("canonical relation kind")
                .matches_terminal()
        );
    }
}

#[test]
fn linked_curation_receipt_allows_distinct_targets_from_one_source() {
    let mut terminal = linked_curation_terminal("supports");
    let mut second =
        terminal["committed_receipts"][0]["receipt"]["receipt"]["operation_effects"][0].clone();
    let source = second["source_fact_id"].clone();
    let first_target = second["target_fact_id"].clone();
    let second_target = project_fact_id("second-target");
    second["target_fact_id"] = json!(second_target);
    second["relation"]["kind"] = json!("derived_from");
    second["commit"]["committed_event_ids"] = json!(["event.curation.second-link"]);
    second["commit"]["last_event_id"] = json!("event.curation.second-link");
    second["commit"]["active_assertion_id"] = json!("assertion.curation.second-link");
    terminal["committed_receipts"][0]["receipt"]["receipt"]["operation_effects"]
        .as_array_mut()
        .expect("operation effects")
        .push(second);
    terminal["committed_receipts"][0]["receipt"]["receipt"]["changed_fact_ids"] =
        json!([source, first_target, second_target]);
    terminal["committed_receipts"][0]["receipt"]["receipt"]["facts_linked"] = json!(2);
    terminal["committed_receipts"][0]["receipt"]["receipt"]["accepted_operations"] = json!(2);
    terminal["terminal"]["summary"]["reviewed_count"] = json!(2);
    terminal["terminal"]["summary"]["accepted_count"] = json!(2);

    assert!(
        serde_json::from_value::<AutomationRunResultV1>(with_current_curation_digest(terminal))
            .expect("two distinct links from one source")
            .matches_terminal()
    );
}

#[test]
fn automatic_fact_receipt_rejects_noncanonical_or_changed_identity() {
    let mut uppercase = automatic_fact_terminal();
    uppercase["committed_receipts"][0]["receipt"]["request"]["input_digest"] =
        json!("A".repeat(64));
    assert!(serde_json::from_value::<AutomationRunResultV1>(uppercase).is_err());
    let mut missing = automatic_fact_terminal();
    missing["committed_receipts"][0]["receipt"]["request"]
        .as_object_mut()
        .expect("request")
        .remove("sanitization_receipt");
    assert!(serde_json::from_value::<AutomationRunResultV1>(missing).is_err());
    let mut changed = automatic_fact_terminal();
    changed["committed_receipts"][0]["receipt"]["evidence"]["item"]["reason"] =
        json!("changed after settlement");
    assert!(
        !serde_json::from_value::<AutomationRunResultV1>(changed)
            .expect("typed digest mismatch")
            .matches_terminal()
    );
}

#[test]
fn absent_automatic_fact_evidence_uses_the_canonical_omitted_shape() {
    let mut terminal = automatic_fact_terminal();
    terminal["committed_receipts"][0]["receipt"]["evidence"] = json!({});
    let mut result = serde_json::from_value::<AutomationRunResultV1>(terminal)
        .expect("closed absent-evidence terminal");
    let AutomationCommittedReceiptV1::AutomaticFact(receipt) = &mut result.committed_receipts[0]
    else {
        panic!("automatic receipt fixture")
    };
    receipt.canonical_digest = receipt
        .computed_canonical_digest()
        .expect("canonical digest");
    assert!(result.matches_terminal());
    let wire = serde_json::to_value(result).expect("receipt wire");
    assert_eq!(
        wire["committed_receipts"][0]["receipt"]["evidence"],
        json!({})
    );
}

#[test]
fn partial_problem_preserves_exact_inner_receipts_and_rejects_flattening() {
    let result = serde_json::from_value::<AutomationRunResultV1>(automatic_fact_terminal())
        .expect("automatic terminal");
    let problem = automatic_partial_problem(result.committed_receipts);
    let wire = serde_json::to_value(&problem).expect("problem wire");
    let decoded = serde_json::from_value::<AutomationRunProblemV1>(wire.clone())
        .expect("exact partial terminal");
    assert!(decoded.matches_terminal(&decoded.problem.request_id));
    let request = automation_request("run.memory.fact", AutomationTaskV1::SessionReflector);
    assert!(decoded.matches_admission(&request, &decoded.problem.request_id,));
    assert!(!decoded.matches_admission(
        &automation_request("run.memory.other", AutomationTaskV1::SessionReflector),
        &decoded.problem.request_id,
    ));

    let mut flattened = wire.clone();
    flattened["committed_receipts"] = json!([]);
    assert!(serde_json::from_value::<AutomationRunProblemV1>(flattened).is_err());

    let mut wrong_operation = wire.clone();
    wrong_operation["problem"]["contract"]["schema_id"] =
        json!("schema.application.retained.wrong.result");
    assert!(serde_json::from_value::<AutomationRunProblemV1>(wrong_operation).is_err());

    let mut changed = wire;
    changed["committed_receipts"][0]["receipt"]["evidence"]["item"]["reason"] =
        json!("changed after the outer terminal committed");
    assert!(serde_json::from_value::<AutomationRunProblemV1>(changed).is_err());
}

#[test]
fn partial_problem_rejects_duplicate_automatic_effect_identity() {
    let result = serde_json::from_value::<AutomationRunResultV1>(automatic_fact_terminal())
        .expect("automatic terminal");
    let receipt = result.committed_receipts[0].clone();
    assert!(automatic_partial_problem_result(vec![receipt.clone(), receipt]).is_err());
}

#[test]
fn partial_curator_problem_rejects_two_distinct_valid_receipts() {
    let first = serde_json::from_value::<AutomationRunResultV1>(curation_terminal())
        .expect("first curation terminal");
    let mut second = first.clone();
    let AutomationCommittedReceiptV1::Curation(receipt) = &mut second.committed_receipts[0] else {
        panic!("curation receipt fixture")
    };
    receipt.receipt.operation_id =
        ProvenanceId::new("operation.curation.second".to_owned()).expect("operation id");
    receipt.receipt.input_digest = "b".repeat(64);
    receipt.canonical_digest = receipt.canonical_digest().expect("canonical digest");
    assert!(second.matches_terminal());

    assert!(
        partial_problem_result(
            "run.memory.curation",
            AutomationTaskV1::MemoryCurator,
            vec![
                first.committed_receipts[0].clone(),
                second.committed_receipts[0].clone(),
            ],
        )
        .is_err()
    );
}

#[test]
fn zero_effect_problem_is_bound_to_exact_run_and_task() {
    let request_id = RequestId::new("request.automation.reset-bound").expect("request id");
    let operation =
        retained_surface_application_operation(RetainedSurfaceOperation::FactStoreCurate)
            .expect("automation operation");
    let problem = ApplicationProblemEnvelope::new(
        operation.result_contract().clone(),
        request_id.clone(),
        ApplicationProblem::reset_required(
            SafeDiagnostic::new(
                "application.automation-run.reset-bound",
                "The exact admitted memory run requires reconciliation",
            )
            .expect("diagnostic"),
        ),
    )
    .expect("problem envelope");
    let request = automation_request("run.memory.reset-bound", AutomationTaskV1::MemoryCurator);
    let terminal =
        AutomationRunProblemV1::new(&request, memory_scope(), problem, Vec::new(), &request_id)
            .expect("zero-effect problem");
    assert!(terminal.matches_admission(&request, &request_id));
    assert!(!terminal.matches_admission(
        &automation_request("run.memory.other", AutomationTaskV1::MemoryCurator),
        &request_id,
    ));
    assert!(!terminal.matches_admission(
        &automation_request("run.memory.reset-bound", AutomationTaskV1::SessionReflector),
        &request_id,
    ));
}

#[test]
fn zero_effect_problem_requires_an_admitted_stage_or_execution_class() {
    let operation =
        retained_surface_application_operation(RetainedSurfaceOperation::FactStoreCurate)
            .expect("automation operation");
    let request = automation_request("run.memory.failure-bound", AutomationTaskV1::MemoryCurator);
    let request_id = RequestId::new("request.memory.failure-bound").expect("request id");
    let terminal = |problem| {
        let envelope = ApplicationProblemEnvelope::new(
            operation.result_contract().clone(),
            request_id.clone(),
            problem,
        )
        .expect("problem envelope");
        AutomationRunProblemV1::new(&request, memory_scope(), envelope, Vec::new(), &request_id)
    };

    assert!(terminal(ApplicationProblem::cancelled_before_admission()).is_err());
    assert!(terminal(ApplicationProblem::timed_out_before_admission()).is_err());
    assert!(
        terminal(
            ApplicationProblem::cancelled(CancellationStage::BeforeEffect)
                .expect("admitted cancellation")
        )
        .is_ok()
    );
    assert!(
        terminal(
            ApplicationProblem::timed_out(CancellationStage::EffectInFlight)
                .expect("admitted timeout")
        )
        .is_ok()
    );
    assert!(
        terminal(
            ApplicationProblem::admitted_unavailable(
                ApplicationUnavailableClassV1::BackendDisconnected,
                SafeDiagnostic::new(
                    "application.automation-run.backend-disconnected",
                    "The admitted automation backend disconnected",
                )
                .expect("diagnostic"),
            )
            .expect("admitted unavailable")
        )
        .is_ok()
    );
    assert!(
        terminal(
            ApplicationProblem::execution_failed(
                ApplicationExecutionFailureClassV1::MalformedOutput,
                SafeDiagnostic::new(
                    "application.automation-run.malformed-output",
                    "The admitted automation backend returned malformed output",
                )
                .expect("diagnostic"),
            )
            .expect("execution failure")
        )
        .is_ok()
    );
    assert!(
        ApplicationProblem::admitted_unavailable(
            ApplicationUnavailableClassV1::Authority,
            SafeDiagnostic::new(
                "application.automation-run.authority-unavailable",
                "The automation authority is unavailable",
            )
            .expect("diagnostic"),
        )
        .is_err()
    );
}

#[test]
fn non_partial_problem_rejects_committed_memory_receipts() {
    let result = serde_json::from_value::<AutomationRunResultV1>(automatic_fact_terminal())
        .expect("automatic terminal");
    let request_id = RequestId::new("request.automation.reset").expect("request id");
    let scope = memory_scope();
    let operation =
        retained_surface_application_operation(RetainedSurfaceOperation::FactStoreCurate)
            .expect("automation operation");
    let problem = ApplicationProblem::ResetRequired {
        diagnostic: SafeDiagnostic::new(
            "application.automation-run.reset-required",
            "The exact admitted run requires reconciliation before it can resume",
        )
        .expect("diagnostic"),
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::Reset],
    };
    let problem = ApplicationProblemEnvelope::new(
        operation.result_contract().clone(),
        request_id.clone(),
        problem,
    )
    .expect("problem envelope");
    assert!(
        AutomationRunProblemV1::new(
            &automation_request("run.memory.fact", AutomationTaskV1::SessionReflector),
            scope,
            problem,
            result.committed_receipts,
            &request_id,
        )
        .is_err()
    );
}

fn automatic_fact_terminal() -> Value {
    let value = with_request_digest(
        json!({
            "run_id":"run.memory.fact","task":"session_reflector",
            "terminal":{"status":"completed","summary":{"reviewed_count":1,"accepted_count":1,"rejected_count":0,"skipped_count":0}},
            "committed_receipts":[{"kind":"automatic_fact","receipt":{
                "apply_id":"apply.memory.fact","owner":{"kind":"profile"},"state":"applied","disposition":"applied","automation_run_id":"run.memory.fact",
                "request":{"operation_id":"operation.memory.fact","input_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","actor":"actor.memory",
                    "sanitization_receipt":{"receipt":{"receipt_id":"receipt.sanitization.memory","sanitizer_version":"sanitizer.memory.v1"},"disposition":"accepted","sensitivity":"non_sensitive","payload":{"digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","byte_len":40}},
                    "content":"Remember the exact canonical fact","category":"general","source_label":"automation:session-reflector","tags":["canonical"],"entities":[],"default_trust_millionths":750000,"metadata":{}},
                "evidence":{"evidence_hash":"evidence-memory-fact","item":{"content":"Remember the exact canonical fact","category":"general","tags":["canonical"],"entities":[],"trust":0.75,"source_span":{"session_id":"session.memory.fact","message_id":"message.memory.fact"},"reason":"The bounded session evidence supports this fact"},"validation":{"status":"accepted","dedupe":{"nearest":null,"near_duplicate_threshold":0.9},"conflict":{"source":"apply_time_add_fact_diff","note":"Apply-time add authority resolves any final conflict"}}},
                "effect":{"state":"applied","fact_id":"fact.profile.memory-fact","target":{"owner":{"kind":"profile"},"fact_id":"fact.profile.memory-fact"},"assertion_id":"assertion.memory.fact","event_id":"event.memory.fact"},
                "recorded_at_micros":1700000000000000i64,"canonical_digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}]
        }),
        &automation_request("run.memory.fact", AutomationTaskV1::SessionReflector),
    );
    let mut result = serde_json::from_value::<AutomationRunResultV1>(value).expect("fixture");
    let AutomationCommittedReceiptV1::AutomaticFact(receipt) = &mut result.committed_receipts[0]
    else {
        panic!("automatic fact receipt fixture")
    };
    receipt.canonical_digest = receipt.computed_canonical_digest().expect("digest");
    serde_json::to_value(result).expect("wire")
}

fn curation_terminal() -> Value {
    let fact_id = project_fact_id("curation");
    let value = with_request_digest(
        json!({
            "run_id":"run.memory.curation","task":"memory_curator",
            "terminal":{"status":"completed","summary":{"reviewed_count":1,"accepted_count":1,"rejected_count":0,"skipped_count":0}},
            "committed_receipts":[{"kind":"curation","receipt":{"receipt":{
                "owner":{"kind":"project","project_id":"project.curation"},"operation_id":"operation.curation","input_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "automation_run_id":"run.memory.curation",
                "operation_effects":[{"kind":"normalize_tags","fact_id":fact_id,"commit":{"disposition":"committed","fact_id":fact_id,"owner":{"kind":"project","project_id":"project.curation"},"committed_event_ids":["event.curation.fact","event.curation.assertion"],"last_event_id":"event.curation.assertion","active_assertion_id":"assertion.curation"}}],
                "replay_fact_id":fact_id,"replay_event_id":"event.curation.assertion","changed_fact_ids":[fact_id],
                "accepted_operations":1,"facts_added":0,"facts_updated":0,"facts_merged":0,"facts_removed":0,"normalized_tags":1,"facts_linked":0},
                "canonical_digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}]
        }),
        &automation_request("run.memory.curation", AutomationTaskV1::MemoryCurator),
    );
    let mut result = serde_json::from_value::<AutomationRunResultV1>(value).expect("fixture");
    let AutomationCommittedReceiptV1::Curation(receipt) = &mut result.committed_receipts[0] else {
        panic!("curation receipt fixture")
    };
    receipt.canonical_digest = receipt.canonical_digest().expect("digest");
    serde_json::to_value(result).expect("wire")
}

fn linked_curation_terminal(relation: &str) -> Value {
    let source_fact_id = project_fact_id("source");
    let target_fact_id = project_fact_id("target");
    let evidence_fact_id = project_fact_id("evidence");
    with_current_curation_digest(with_request_digest(
        json!({
            "run_id":"run.memory.curation","task":"memory_curator",
            "terminal":{"status":"completed","summary":{"reviewed_count":1,"accepted_count":1,"rejected_count":0,"skipped_count":0}},
            "committed_receipts":[{"kind":"curation","receipt":{"receipt":{
                "owner":{"kind":"project","project_id":"project.curation"},"operation_id":"operation.curation","input_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "automation_run_id":"run.memory.curation",
                "operation_effects":[{"kind":"link_facts","source_fact_id":source_fact_id,"target_fact_id":target_fact_id,"relation":{
                    "kind":relation,"evidence_fact_ids":[evidence_fact_id],"confidence_millionths":800000,
                    "provenance":{"source_label":"automation:memory-curator","sanitization_receipt":{
                        "receipt":{"receipt_id":"receipt.curation.relation","sanitizer_version":"sanitizer.memory.v1"},
                        "disposition":"accepted","sensitivity":"non_sensitive",
                        "payload":{"digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","byte_len":128}
                    }}
                },"disposition":"linked","commit":{"disposition":"committed","fact_id":source_fact_id,"owner":{"kind":"project","project_id":"project.curation"},"committed_event_ids":["event.curation.link"],"last_event_id":"event.curation.link","active_assertion_id":"assertion.curation.link"}}],
                "replay_fact_id":source_fact_id,"replay_event_id":"event.curation.link","changed_fact_ids":[source_fact_id,target_fact_id],
                "accepted_operations":1,"facts_added":0,"facts_updated":0,"facts_merged":0,"facts_removed":0,"normalized_tags":0,"facts_linked":1},
                "canonical_digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}]
        }),
        &automation_request("run.memory.curation", AutomationTaskV1::MemoryCurator),
    ))
}

fn with_current_curation_digest(value: Value) -> Value {
    let mut result =
        serde_json::from_value::<AutomationRunResultV1>(value).expect("curation terminal fixture");
    let AutomationCommittedReceiptV1::Curation(receipt) = &mut result.committed_receipts[0] else {
        panic!("curation receipt fixture")
    };
    receipt.canonical_digest = receipt.canonical_digest().expect("digest");
    serde_json::to_value(result).expect("wire")
}

fn project_fact_id(label: &str) -> String {
    let owner = FactOwnerV1::Project {
        project_id: ProjectId::new("project.curation".to_owned()).expect("project id"),
    };
    let source = FactIdentitySourceV1::Application {
        operation_id: ProvenanceId::new(format!("operation.curation.{label}"))
            .expect("operation id"),
    };
    FactId::derive(
        &FactIdentityMaterialV1::new(owner, source).expect("canonical identity material"),
    )
    .expect("canonical fact id")
    .as_str()
    .to_owned()
}

fn automatic_partial_problem(
    committed_receipts: Vec<AutomationCommittedReceiptV1>,
) -> AutomationRunProblemV1 {
    automatic_partial_problem_result(committed_receipts).expect("canonical problem terminal")
}

fn automatic_partial_problem_result(
    committed_receipts: Vec<AutomationCommittedReceiptV1>,
) -> Result<AutomationRunProblemV1, crate::ApplicationContractError> {
    partial_problem_result(
        "run.memory.fact",
        AutomationTaskV1::SessionReflector,
        committed_receipts,
    )
}

fn partial_problem_result(
    run_id: &str,
    task: AutomationTaskV1,
    committed_receipts: Vec<AutomationCommittedReceiptV1>,
) -> Result<AutomationRunProblemV1, crate::ApplicationContractError> {
    let request_id = RequestId::new("request.automation.partial").expect("request id");
    let scope = memory_scope();
    let committed_state = canonical_sha256(&(
        "tracedecay.automation-run.partial-state.v1",
        run_id,
        &committed_receipts,
    ))
    .expect("committed state");
    let digest = |seed: char| {
        ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64)))
            .expect("fixture digest")
    };
    let operation =
        retained_surface_application_operation(RetainedSurfaceOperation::FactStoreCurate)
            .expect("automation operation");
    let receipt = EffectReceipt {
        operation: operation.use_case_id().clone(),
        request_id: request_id.clone(),
        actor: ActorId::new("actor.automation").expect("actor"),
        scope: scope.clone(),
        effect_class: EffectClass::Administrative,
        idempotency_key: IdempotencyKey::new("idempotency.automation.partial")
            .expect("idempotency key"),
        input_digest: digest('1'),
        expected_state: digest('2'),
        policy_digest: digest('3'),
        configuration_digest: digest('4'),
        catalog_digest: digest('5'),
        privacy_digest: digest('6'),
        outcome: EffectTermination::Partial,
        committed_state: Some(committed_state),
        external_proof: None,
    };
    let problem =
        retained_surface_execution_problem(RetainedSurfaceExecutionErrorV1::PartialEffect {
            reason_code: "application.automation-run.partial-effect".to_owned(),
            committed_receipt: Box::new(receipt),
            detail: "A canonical memory effect committed before the run stopped".to_owned(),
        });
    let problem = ApplicationProblemEnvelope::new(
        operation.result_contract().clone(),
        request_id.clone(),
        problem,
    )
    .expect("problem envelope");
    AutomationRunProblemV1::new(
        &automation_request(run_id, task),
        scope,
        problem,
        committed_receipts,
        &request_id,
    )
}

fn memory_scope() -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new("project.automation").expect("project id"),
        RepositoryId::new("repository.automation").expect("repository id"),
        WorktreeId::new("worktree.automation").expect("worktree id"),
        None,
    )
    .expect("scope")
}
