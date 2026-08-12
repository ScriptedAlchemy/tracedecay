use std::sync::Arc;

use tracedecay_domain::{
    ActorId, Confidence, FactEventId, PayloadAccessState, PayloadReferenceV1, ProvenanceId,
    UtcMicros,
};
use tracedecay_store::{
    FactCommitOutcome, FactCommitReceipt, FactWriteControl,
    ProjectMemoryAutomaticFactApplyDispositionV1, ProjectMemoryAutomaticFactApplyResultV1,
    ProjectMemoryAutomaticFactEffectV1, ProjectMemoryAutomaticFactEvidenceV1,
    ProjectMemoryAutomaticFactReceiptV1, ProjectMemoryAutomaticFactStateV1,
    ProjectMemoryFactAddOutcomeV1, ProjectMemoryFactIdV1, ProjectMemoryFactMergeCommandV1,
    ProjectMemoryFactMergeOutcomeV1, ProjectMemoryFactProjectionV1, ProjectMemoryFactStatusV1,
    ProjectMemoryFactUnavailableV1,
};

use super::{FakeAuthority, batch, committed_outcome, fact_add_request, fact_id, id, owner};
use crate::memory::{
    MemoryApplication, MemoryApplicationError, MemoryMutationError, MemoryOperationContext,
    ProjectMemoryFactAddPreflight, ProjectMemoryFactAddRequestOutcome, automatic_fact_add_command,
};

#[test]
fn automation_run_identity_is_typed_not_fact_payload_metadata() {
    let mut request = fact_add_request();
    request.metadata = serde_json::json!({
        "automation_run_id": "caller-controlled-run-id",
        "fixture": "retained",
    });

    let command = automatic_fact_add_command(
        owner(),
        request,
        "run_01J4A7P5MQ1X9DX2P9BQNQW75T",
        "automatic-fact-typed-run-id",
        None,
    )
    .unwrap();

    assert_eq!(
        command.automation_run_id(),
        Some("run_01J4A7P5MQ1X9DX2P9BQNQW75T")
    );
    assert_eq!(
        command.metadata().get("fixture"),
        Some(&serde_json::Value::String("retained".to_owned()))
    );
    assert!(command.metadata().get("automation_run_id").is_none());
}

#[test]
fn fact_add_preserves_the_authoritative_sanitizer_receipt() {
    let mut request = fact_add_request();
    request.source_label = Some("canonical add fixture".to_owned());
    request.metadata = serde_json::json!({
        "api_key": "fixture-secret-value-that-must-be-redacted",
        "fixture": "retained",
    });
    let (_, expected_receipt) = super::super::sanitize::sanitize_add_fact_request(request.clone())
        .unwrap()
        .unwrap()
        .into_parts();

    let command = automatic_fact_add_command(
        owner(),
        request,
        "run_01J4A7P5MQ1X9DX2P9BQNQW75T",
        "automatic-fact-receipt-provenance",
        None,
    )
    .unwrap();

    assert_eq!(command.sanitization_receipt(), &expected_receipt);
    assert_eq!(command.source_label(), Some("canonical add fixture"));
    assert_eq!(
        command.metadata().get("api_key"),
        Some(&serde_json::Value::String(
            "[TraceDecay redacted: sensitive field]".to_owned()
        ))
    );
}

#[tokio::test]
async fn secret_like_add_is_a_truthful_no_write_outcome() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    let mut request = fact_add_request();
    request.content = "api_key=sk-canonical-fixture-secret-1234567890abcdef".to_owned();
    let preflight = application
        .preflight_project_memory_fact_add(request, None)
        .unwrap();
    assert!(matches!(
        &preflight,
        ProjectMemoryFactAddPreflight::RejectedSecretLike { .. }
    ));
    assert!(preflight.command().is_none());
    assert!(
        !serde_json::to_string(preflight.effect_material())
            .unwrap()
            .contains("sk-canonical-fixture-secret")
    );
    let write_control = FactWriteControl::new(
        Arc::new(|| panic!("secret rejection must not inspect interruption")),
        Arc::new(|| panic!("secret rejection must not begin a commit")),
    );

    let outcome = application
        .add_preflighted_project_memory_fact(preflight, &write_control)
        .await
        .unwrap();

    assert_eq!(
        outcome,
        ProjectMemoryFactAddRequestOutcome::RejectedSecretLike
    );
    assert!(
        application
            .authority
            .authority_calls
            .lock()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn add_preflight_uses_one_canonical_identity_after_sanitization_and_defaults() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    let actor = ActorId::new("actor.memory.preflight").unwrap();
    let mut unordered = fact_add_request();
    unordered.tags = vec!["second".to_owned(), "first".to_owned()];
    unordered.entities = vec!["zeta".to_owned(), "alpha".to_owned()];
    unordered.trust = None;
    unordered.metadata = serde_json::json!({
        "automation_run_id": "caller-controlled",
        "fixture": "canonical",
    });
    let mut canonical = unordered.clone();
    canonical.tags.sort_unstable();
    canonical.entities.sort_unstable();
    canonical.trust = Some(Confidence::new(0.5).unwrap());
    canonical
        .metadata
        .as_object_mut()
        .unwrap()
        .remove("automation_run_id");

    let first = application
        .preflight_project_memory_fact_add(unordered, Some(actor.clone()))
        .unwrap();
    let replay = application
        .preflight_project_memory_fact_add(canonical.clone(), Some(actor.clone()))
        .unwrap();
    let other_actor = application
        .preflight_project_memory_fact_add(
            canonical,
            Some(ActorId::new("actor.memory.preflight.other").unwrap()),
        )
        .unwrap();

    assert_eq!(first.effect_material(), replay.effect_material());
    assert_eq!(first.operation_id(), replay.operation_id());
    assert_eq!(
        first.command().unwrap().input_digest(),
        first.effect_material().canonical_digest()
    );
    assert_eq!(first.command().unwrap().actor(), Some(&actor));
    assert_ne!(first.operation_id(), other_actor.operation_id());

    let derived = MemoryOperationContext::from_logical_effect(
        &owner(),
        "add",
        first.effect_material(),
        Some(actor),
    )
    .unwrap();
    assert_eq!(derived.operation_id(), first.operation_id());
}

#[tokio::test]
async fn automatic_fact_receipt_binds_the_exact_submitted_request_and_evidence() {
    let owner = owner();
    let apply_id: ProvenanceId = id("automatic-fact.apply.exact-binding");
    let submitted_request = automatic_fact_add_command(
        owner.clone(),
        fact_add_request(),
        "automatic-fact.run.submitted",
        "automatic-fact.command.submitted",
        None,
    )
    .unwrap();
    let wrong_request = automatic_fact_add_command(
        owner.clone(),
        fact_add_request(),
        "automatic-fact.run.wrong",
        "automatic-fact.command.wrong",
        None,
    )
    .unwrap();
    let submitted_evidence = ProjectMemoryAutomaticFactEvidenceV1::new(
        Some("automatic-fact.evidence.submitted".to_owned()),
        None,
        None,
    )
    .unwrap();
    let wrong_evidence = ProjectMemoryAutomaticFactEvidenceV1::new(
        Some("automatic-fact.evidence.wrong".to_owned()),
        None,
        None,
    )
    .unwrap();

    for (receipt_request, receipt_evidence) in [
        (wrong_request, submitted_evidence.clone()),
        (submitted_request.clone(), wrong_evidence),
    ] {
        let receipt = ProjectMemoryAutomaticFactReceiptV1::new(
            apply_id.clone(),
            owner.clone(),
            ProjectMemoryAutomaticFactStateV1::Quarantined,
            receipt_request,
            receipt_evidence,
            ProjectMemoryAutomaticFactEffectV1::Quarantined {
                reason: "fixture quarantine".to_owned(),
            },
            UtcMicros(1),
        )
        .unwrap();
        let expected = ProjectMemoryAutomaticFactApplyResultV1::new(
            receipt,
            ProjectMemoryAutomaticFactApplyDispositionV1::Quarantined,
        )
        .unwrap();
        let authority = FakeAuthority::default();
        *authority.automatic_fact_apply_result.lock().unwrap() = Some(expected.clone());
        let application = MemoryApplication::new(owner.clone(), authority).unwrap();

        let error = application
            .apply_project_memory_automatic_fact(
                apply_id.clone(),
                submitted_request.clone(),
                submitted_evidence.clone(),
                &super::write_control(),
            )
            .await
            .unwrap_err();

        let MemoryMutationError::InvalidAuthorityResult {
            authority_result, ..
        } = error
        else {
            panic!("wrong automatic fact receipt must retain the authority result");
        };
        assert_eq!(authority_result, expected);
    }
}

#[tokio::test]
async fn merge_rejects_a_mismatched_authority_digest_without_losing_the_outcome() {
    let owner = owner();
    let winner = ProjectMemoryFactIdV1::new(
        owner.clone(),
        fact_id(owner.clone(), "operation.memory.merge-winner"),
    )
    .unwrap();
    let loser = ProjectMemoryFactIdV1::new(
        owner.clone(),
        fact_id(owner.clone(), "operation.memory.merge-loser"),
    )
    .unwrap();
    let command = ProjectMemoryFactMergeCommandV1::new(
        owner.clone(),
        id("operation.memory.merge"),
        winner.clone(),
        vec![loser.clone()],
        None,
        None,
    )
    .unwrap();
    let expected_digest = command.input_digest().unwrap();
    let first_event: FactEventId = id("event.memory.merge-loser.first");
    let last_event: FactEventId = id("event.memory.merge-loser.last");
    let commit = FactCommitReceipt::new(
        loser.fact_id().clone(),
        owner.clone(),
        vec![first_event, last_event.clone()],
        last_event,
        None,
    )
    .unwrap();
    let outcome = ProjectMemoryFactMergeOutcomeV1::new(
        owner.clone(),
        command.operation_id().clone(),
        "0".repeat(64),
        winner,
        false,
        vec![loser],
        vec![commit],
    )
    .unwrap();
    assert_ne!(outcome.input_digest(), expected_digest);
    let authority = FakeAuthority::default();
    *authority.merge_outcome.lock().unwrap() = Some(outcome.clone());
    let application = MemoryApplication::new(owner, authority).unwrap();

    let error = application
        .dashboard_merge_facts(command, &super::write_control())
        .await
        .unwrap_err();

    let MemoryMutationError::InvalidAuthorityResult {
        authority_result, ..
    } = error
    else {
        panic!("merge digest mismatch must retain the committed authority outcome");
    };
    assert_eq!(authority_result, outcome);
}

#[test]
fn semantic_similarity_outcomes_are_receipt_bearing_commits() {
    let owner = owner();
    let write = batch(owner.clone(), "operation.memory.conflict");
    let receipt = match committed_outcome(&write) {
        FactCommitOutcome::Committed(receipt) => receipt,
        _ => unreachable!("fixture commits exactly once"),
    };
    let projection = ProjectMemoryFactProjectionV1::Unavailable(
        ProjectMemoryFactUnavailableV1::new(
            ProjectMemoryFactStatusV1::new(
                owner.clone(),
                write.fact_id().clone(),
                PayloadAccessState::Deleted,
                UtcMicros(1),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let closest = ProjectMemoryFactIdV1::new(
        owner.clone(),
        fact_id(owner.clone(), "operation.memory.closest"),
    )
    .unwrap();
    let normalized = ProjectMemoryFactAddOutcomeV1::normalized_duplicate(
        projection.clone(),
        ProjectMemoryFactIdV1::new(owner.clone(), write.fact_id().clone()).unwrap(),
    )
    .unwrap();
    let near_duplicate = ProjectMemoryFactAddOutcomeV1::semantic_near_duplicate(
        projection.clone(),
        closest.clone(),
        900_000,
        receipt.clone(),
        false,
    )
    .unwrap();
    let conflict = ProjectMemoryFactAddOutcomeV1::possible_conflict(
        projection, closest, 750_000, receipt, false,
    )
    .unwrap();

    assert!(normalized.commit_receipt().is_none());
    assert!(!normalized.commit_replayed());
    assert!(
        super::super::project_memory::validate_project_memory_add_outcome(&owner, &normalized)
            .is_ok()
    );
    for outcome in [&near_duplicate, &conflict] {
        assert!(outcome.commit_receipt().is_some());
        assert!(
            super::super::project_memory::validate_project_memory_add_outcome(&owner, outcome)
                .is_ok()
        );
    }
}

#[test]
fn relation_provenance_keeps_metadata_bound_to_its_receipt() {
    let provenance = super::super::sanitize::sanitize_curation_provenance(
        "memory-curator".to_owned(),
        serde_json::json!({
            "token": "secret-fixture-value",
            "reason": "fixture",
        }),
    )
    .unwrap();

    assert_eq!(
        provenance.sanitization_receipt().payload(),
        Some(
            &PayloadReferenceV1::for_payload(&serde_json::json!({
                "source_label": provenance.source_label(),
                "metadata": provenance.metadata(),
            }))
            .unwrap()
        )
    );
    assert_eq!(
        provenance.metadata().get("token"),
        Some(&serde_json::Value::String(
            "[TraceDecay redacted: sensitive field]".to_owned()
        ))
    );
}

#[test]
fn relation_evidence_sorts_unique_input_and_rejects_duplicates() {
    let first = fact_id(owner(), "operation.relation.evidence.first");
    let second = fact_id(owner(), "operation.relation.evidence.second");
    let mut evidence = vec![second, first];
    super::super::dashboard::canonicalize_relation_evidence(&owner(), &mut evidence).unwrap();
    assert!(evidence.windows(2).all(|pair| pair[0] < pair[1]));
    let fact_id = fact_id(owner(), "operation.relation.evidence.duplicate");
    let mut evidence = vec![fact_id.clone(), fact_id];

    let error = super::super::dashboard::canonicalize_relation_evidence(&owner(), &mut evidence)
        .unwrap_err();

    assert!(matches!(error, MemoryApplicationError::InvalidInput { .. }));
}
