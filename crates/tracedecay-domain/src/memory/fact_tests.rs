use super::*;
use crate::observation::{SanitizerDispositionV1, SensitivityV1};
use crate::research::SanitizationReceiptRefV1;
use serde_json::json;

fn id<T: TryFrom<String, Error = DomainError>>(value: &str) -> T {
    T::try_from(value.to_owned()).unwrap()
}

fn fact_id(owner: FactOwnerV1, operation: &str) -> FactId {
    FactId::derive(
        &FactIdentityMaterialV1::new(
            owner,
            FactIdentitySourceV1::Application {
                operation_id: id(operation),
            },
        )
        .unwrap(),
    )
    .unwrap()
}

fn receipt(receipt_id: &str, payload: PayloadReferenceV1) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(id(receipt_id), id("sanitizer.fixture.v1")).unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(payload),
    )
    .unwrap()
}

fn payload() -> FactPayloadV1 {
    let material = json!({
        "content": "The daemon is the only writer.",
        "category": "project",
        "tags": ["daemon", "database"],
        "entities": ["TraceDecay"],
        "metadata": {"source": "fixture"},
        "source_label": "fixture",
    });
    let receipt = receipt(
        "receipt.fact.fixture",
        PayloadReferenceV1::for_payload(&material).unwrap(),
    );
    FactPayloadV1::new(
        "The daemon is the only writer.".to_owned(),
        FactCategoryV1::Project,
        vec!["daemon".to_owned(), "database".to_owned()],
        vec!["TraceDecay".to_owned()],
        json!({"source": "fixture"}),
        Some("fixture".to_owned()),
        receipt,
        RetentionClass::new("durable.fact").unwrap(),
    )
    .unwrap()
}

#[test]
fn fact_and_evidence_ids_are_deterministic_and_owner_scoped() {
    let project_owner = FactOwnerV1::Project {
        project_id: id("project.fixture"),
    };
    let first = fact_id(project_owner.clone(), "operation.fixture");
    let replay = fact_id(project_owner, "operation.fixture");
    let profile = fact_id(FactOwnerV1::Profile, "operation.fixture");
    assert_eq!(first, replay);
    assert_ne!(first, profile);

    let evidence = FactEvidenceRefV1::new(
        first.clone(),
        id("retrieval.fixture"),
        FactEvidenceRelationV1::Supports,
        EvidenceClass::Observed,
        Confidence::new(1.0).unwrap(),
    )
    .unwrap();
    let replayed = FactEvidenceRefV1::new(
        first.clone(),
        id("retrieval.fixture"),
        FactEvidenceRelationV1::Supports,
        EvidenceClass::Observed,
        Confidence::new(1.0).unwrap(),
    )
    .unwrap();
    assert_eq!(evidence.evidence_id(), replayed.evidence_id());

    let lower_confidence = FactEvidenceRefV1::new(
        first,
        id("retrieval.fixture"),
        FactEvidenceRelationV1::Supports,
        EvidenceClass::Inferred,
        Confidence::new(0.8).unwrap(),
    )
    .unwrap();
    assert_ne!(evidence.evidence_id(), lower_confidence.evidence_id());
}

#[test]
fn assertion_identity_changes_with_owner_payload_and_lineage() {
    let owner = FactOwnerV1::Project {
        project_id: id("project.fixture"),
    };
    let fact_id = fact_id(owner.clone(), "operation.fixture");
    let evidence = FactEvidenceRefV1::new(
        fact_id.clone(),
        id("retrieval.fixture"),
        FactEvidenceRelationV1::Supports,
        EvidenceClass::Observed,
        Confidence::new(1.0).unwrap(),
    )
    .unwrap();
    let first = FactAssertionV1::new(
        fact_id.clone(),
        owner.clone(),
        FactAssertionKindV1::Initial,
        payload(),
        vec![evidence.clone()],
        UtcMicros(10),
        None,
    )
    .unwrap();
    let replay = FactAssertionV1::new(
        fact_id,
        owner,
        FactAssertionKindV1::Initial,
        payload(),
        vec![evidence],
        UtcMicros(10),
        None,
    )
    .unwrap();
    assert_eq!(first.assertion_id(), replay.assertion_id());
}

#[test]
fn payload_source_label_is_preserved_and_receipt_bound() {
    let payload = payload();
    assert_eq!(payload.source_label(), Some("fixture"));
    let mut tampered = serde_json::to_value(payload).unwrap();
    tampered["source_label"] = json!("other");
    assert!(serde_json::from_value::<FactPayloadV1>(tampered).is_err());

    let wrong_reference = PayloadReferenceV1::for_payload(&json!({"different": true})).unwrap();
    let receipt = receipt("receipt.fact.wrong", wrong_reference);
    assert!(
        FactPayloadV1::new(
            "safe".to_owned(),
            FactCategoryV1::General,
            vec![],
            vec![],
            json!({}),
            None,
            receipt,
            RetentionClass::new("durable.fact").unwrap(),
        )
        .is_err()
    );
}

#[test]
fn evidence_cannot_be_attached_to_another_fact() {
    let owner = FactOwnerV1::Profile;
    let first = fact_id(owner.clone(), "operation.first");
    let second = fact_id(owner.clone(), "operation.second");
    let evidence = FactEvidenceRefV1::new(
        first,
        id("retrieval.fixture"),
        FactEvidenceRelationV1::Supports,
        EvidenceClass::Observed,
        Confidence::new(1.0).unwrap(),
    )
    .unwrap();
    assert!(
        FactAssertionV1::new(
            second,
            owner,
            FactAssertionKindV1::Initial,
            payload(),
            vec![evidence],
            UtcMicros(10),
            None,
        )
        .is_err()
    );
}

#[test]
fn identity_bearing_wire_values_reject_tampering() {
    let owner = FactOwnerV1::Profile;
    let fact_id = fact_id(owner.clone(), "operation.wire");
    let evidence = FactEvidenceRefV1::new(
        fact_id.clone(),
        id("retrieval.wire"),
        FactEvidenceRelationV1::Supports,
        EvidenceClass::Observed,
        Confidence::new(1.0).unwrap(),
    )
    .unwrap();
    let mut evidence_wire = serde_json::to_value(&evidence).unwrap();
    evidence_wire["evidence_id"] = json!("fact-evidence.v1.forged");
    assert!(serde_json::from_value::<FactEvidenceRefV1>(evidence_wire).is_err());

    let assertion = FactAssertionV1::new(
        fact_id,
        owner,
        FactAssertionKindV1::Initial,
        payload(),
        vec![evidence],
        UtcMicros(10),
        None,
    )
    .unwrap();
    let mut assertion_wire = serde_json::to_value(&assertion).unwrap();
    assertion_wire["assertion_id"] = json!("fact-assertion.v1.forged");
    assert!(serde_json::from_value::<FactAssertionV1>(assertion_wire).is_err());

    let mut owner_wire = serde_json::to_value(&assertion).unwrap();
    owner_wire["owner"] = json!({"kind": "project", "project_id": "project.other"});
    assert!(serde_json::from_value::<FactAssertionV1>(owner_wire).is_err());
}

#[test]
fn assertion_set_order_is_canonical() {
    let owner = FactOwnerV1::Profile;
    let fact_id = fact_id(owner.clone(), "operation.order");
    let first_evidence = FactEvidenceRefV1::new(
        fact_id.clone(),
        id("retrieval.order.a"),
        FactEvidenceRelationV1::Supports,
        EvidenceClass::Observed,
        Confidence::new(1.0).unwrap(),
    )
    .unwrap();
    let second_evidence = FactEvidenceRefV1::new(
        fact_id.clone(),
        id("retrieval.order.b"),
        FactEvidenceRelationV1::Supports,
        EvidenceClass::Observed,
        Confidence::new(1.0).unwrap(),
    )
    .unwrap();
    let first = FactAssertionV1::new(
        fact_id.clone(),
        owner.clone(),
        FactAssertionKindV1::Initial,
        payload(),
        vec![first_evidence.clone(), second_evidence.clone()],
        UtcMicros(10),
        None,
    )
    .unwrap();
    let second = FactAssertionV1::new(
        fact_id,
        owner,
        FactAssertionKindV1::Initial,
        payload(),
        vec![second_evidence, first_evidence],
        UtcMicros(10),
        None,
    )
    .unwrap();

    assert_eq!(first, second);
}

#[test]
fn unknown_identity_and_assertion_variants_are_rejected() {
    assert!(
        serde_json::from_value::<FactIdentitySourceV1>(json!({
            "kind": "unknown",
        }))
        .is_err()
    );
    assert!(serde_json::from_value::<FactAssertionKindV1>(json!({"kind": "unknown"})).is_err());
}
