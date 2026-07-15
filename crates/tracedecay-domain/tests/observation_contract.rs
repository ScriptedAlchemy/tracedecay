use std::cmp::Ordering;

use serde_json::{Value, json};
use tracedecay_domain::{
    CanonicalClaudeSanitizationReceiptMaterialV1, CanonicalObservationIdV1, ClaudeByteRangeV1,
    ClaudeFileGenerationV1, ClaudeObservationIdentityMaterialV1, ClaudeSourceCursorV1,
    ClaudeSourceIdentityV1, ComponentVersion, DurableClaudeObservationV1, IdempotencyKeyV1,
    ObservationCollisionOutcomeV1, ObservationScopeV1, PayloadReferenceV1, ProjectId,
    RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
    SanitizerDispositionV1, SensitivityV1, SessionId, classify_observation_collision,
};

fn source(session_id: &str) -> ClaudeSourceIdentityV1 {
    ClaudeSourceIdentityV1::new(SessionId::new(session_id).unwrap()).unwrap()
}

fn profile_material() -> ClaudeObservationIdentityMaterialV1 {
    ClaudeObservationIdentityMaterialV1::new(
        source("session.fixture"),
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(7).unwrap(),
        ClaudeByteRangeV1::new(12, 34).unwrap(),
    )
    .unwrap()
}

fn receipt_ref() -> SanitizationReceiptRefV1 {
    SanitizationReceiptRefV1::new(
        SanitizationReceiptId::new("receipt.fixture").unwrap(),
        ComponentVersion::new("sanitizer.fixture.v1").unwrap(),
    )
    .unwrap()
}

fn accepted_receipt(payload: &Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        receipt_ref(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(payload).unwrap()),
    )
    .unwrap()
}

fn durable(
    material: ClaudeObservationIdentityMaterialV1,
    payload: Value,
) -> DurableClaudeObservationV1 {
    DurableClaudeObservationV1::new(
        material,
        accepted_receipt(&payload),
        RetentionClass::new("transcript.fixture").unwrap(),
        payload,
    )
    .unwrap()
}

#[test]
fn observation_ids_are_stable_and_payload_objects_are_canonical() {
    let material = profile_material();
    let observation_id = CanonicalObservationIdV1::derive(&material).unwrap();
    let idempotency_key = IdempotencyKeyV1::derive(&material).unwrap();

    assert_eq!(
        observation_id.as_str(),
        "sha256:92fe6f78f68eb34153f865b770a7fed01b01425730796ac67bbc4973aad527a3"
    );
    assert_eq!(
        idempotency_key.as_str(),
        "sha256:92fe6f78f68eb34153f865b770a7fed01b01425730796ac67bbc4973aad527a3"
    );
    assert_eq!(observation_id, idempotency_key);

    let first: Value = serde_json::from_str(r#"{"z":2,"nested":{"b":2,"a":1},"a":1}"#).unwrap();
    let reordered: Value = serde_json::from_str(r#"{"a":1,"nested":{"a":1,"b":2},"z":2}"#).unwrap();
    let first_ref = PayloadReferenceV1::for_payload(&first).unwrap();
    let reordered_ref = PayloadReferenceV1::for_payload(&reordered).unwrap();

    assert_eq!(first_ref.digest(), reordered_ref.digest());
    assert_eq!(first_ref.byte_len(), reordered_ref.byte_len());
    assert_eq!(
        durable(material.clone(), first).canonical_payload_bytes(),
        durable(material, reordered).canonical_payload_bytes()
    );
}

#[test]
fn receipt_derivation_is_canonical_and_preserves_existing_ids() {
    let material = CanonicalClaudeSanitizationReceiptMaterialV1::new(
        &profile_material(),
        ComponentVersion::new("sanitizer.fixture.v1").unwrap(),
        SanitizerDispositionV1::Accepted,
        b"evidence.fixture",
    )
    .unwrap();
    let receipt = material.derive_receipt_ref().unwrap();

    assert_eq!(
        receipt.receipt_id().as_str(),
        "privacy.claude.v1.cd2fffcfc651eafe4c7f923686dd238c7791b9355600cf82b7b0707a049d7a0d"
    );
    assert_eq!(receipt.sanitizer_version().as_str(), "sanitizer.fixture.v1");
    assert_eq!(SanitizerDispositionV1::Accepted.as_str(), "accepted");
    assert_eq!(SanitizerDispositionV1::Redacted.as_str(), "redacted");
    assert_eq!(SanitizerDispositionV1::Rejected.as_str(), "rejected");
    assert_eq!(SanitizerDispositionV1::Quarantined.as_str(), "quarantined");
}

#[test]
fn idempotency_wire_field_is_a_canonical_identity_alias() {
    let observation = durable(profile_material(), json!({"message": "safe"}));
    let wire = serde_json::to_value(&observation).unwrap();

    assert_eq!(observation.idempotency_key(), observation.observation_id());
    assert_eq!(wire["idempotency_key"], wire["observation_id"]);

    let mut legacy_wire = wire.clone();
    legacy_wire["idempotency_key"] = Value::String(
        "sha256:13b3a18339fe0dbf5a1ccc894e24cf1626ca88babef32869bf7dc85f6a626abb".to_owned(),
    );
    let decoded: DurableClaudeObservationV1 = serde_json::from_value(legacy_wire).unwrap();
    assert_eq!(decoded.idempotency_key(), decoded.observation_id());

    let mut invalid_wire = wire;
    invalid_wire["idempotency_key"] = Value::String(format!("sha256:{}", "0".repeat(64)));
    assert!(serde_json::from_value::<DurableClaudeObservationV1>(invalid_wire).is_err());
}

#[test]
fn scope_participates_in_identity_and_invalid_positions_are_rejected() {
    let profile = profile_material();
    let project = ClaudeObservationIdentityMaterialV1::new(
        source("session.fixture"),
        ObservationScopeV1::Project {
            project_id: ProjectId::new("project.fixture").unwrap(),
        },
        ClaudeFileGenerationV1::new(7).unwrap(),
        ClaudeByteRangeV1::new(12, 34).unwrap(),
    )
    .unwrap();

    assert_ne!(
        CanonicalObservationIdV1::derive(&profile).unwrap(),
        CanonicalObservationIdV1::derive(&project).unwrap()
    );
    assert_ne!(
        IdempotencyKeyV1::derive(&profile).unwrap(),
        IdempotencyKeyV1::derive(&project).unwrap()
    );
    assert!(ClaudeFileGenerationV1::new(0).is_err());
    assert!(ClaudeByteRangeV1::new(5, 5).is_err());
    assert!(ClaudeByteRangeV1::new(6, 5).is_err());
}

#[test]
fn source_cursors_enforce_their_comparison_domain() {
    let generation = ClaudeFileGenerationV1::new(2).unwrap();
    let byte_cursor = |session: &str, scope, generation, offset| {
        ClaudeSourceCursorV1::new(source(session), scope, generation, offset).unwrap()
    };
    let first = byte_cursor(
        "session.fixture",
        ObservationScopeV1::Profile,
        generation.clone(),
        10,
    );
    let later = byte_cursor(
        "session.fixture",
        ObservationScopeV1::Profile,
        generation.clone(),
        20,
    );

    assert_eq!(first.checked_cmp(&later).unwrap(), Ordering::Less);
    assert!(
        first
            .checked_cmp(&byte_cursor(
                "session.other",
                ObservationScopeV1::Profile,
                generation.clone(),
                20,
            ))
            .is_err()
    );
    assert!(
        first
            .checked_cmp(&byte_cursor(
                "session.fixture",
                ObservationScopeV1::Project {
                    project_id: ProjectId::new("project.fixture").unwrap(),
                },
                generation.clone(),
                20,
            ))
            .is_err()
    );
    assert!(
        first
            .checked_cmp(&byte_cursor(
                "session.fixture",
                ObservationScopeV1::Profile,
                ClaudeFileGenerationV1::new(3).unwrap(),
                20,
            ))
            .is_err()
    );
}

#[test]
fn receipts_and_durable_observations_enforce_sanitization_binding() {
    let payload = json!({"message": "safe"});
    let payload_ref = PayloadReferenceV1::for_payload(&payload).unwrap();

    assert!(
        SanitizationReceiptV1::new(
            receipt_ref(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::Unclassified,
            Some(payload_ref.clone()),
        )
        .is_err()
    );
    assert!(
        SanitizationReceiptV1::new(
            receipt_ref(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::Secret,
            Some(payload_ref.clone()),
        )
        .is_err()
    );

    for disposition in [
        SanitizerDispositionV1::Rejected,
        SanitizerDispositionV1::Quarantined,
    ] {
        assert!(
            SanitizationReceiptV1::new(
                receipt_ref(),
                disposition,
                SensitivityV1::Sensitive,
                Some(payload_ref.clone()),
            )
            .is_err()
        );

        let receipt =
            SanitizationReceiptV1::new(receipt_ref(), disposition, SensitivityV1::Sensitive, None)
                .unwrap();
        assert!(
            DurableClaudeObservationV1::new(
                profile_material(),
                receipt,
                RetentionClass::new("transcript.fixture").unwrap(),
                payload.clone(),
            )
            .is_err()
        );
    }

    for mismatched in [
        json!({"message": "nope"}),
        json!({"message": "longer value"}),
    ] {
        assert!(
            DurableClaudeObservationV1::new(
                profile_material(),
                accepted_receipt(&payload),
                RetentionClass::new("transcript.fixture").unwrap(),
                mismatched,
            )
            .is_err()
        );
    }
}

#[test]
fn durable_round_trip_preserves_unknown_provider_evidence_and_canonical_bytes() {
    let payload = json!({
        "kind": "assistant",
        "provider_evidence": {
            "future_field": [1, {"opaque": true}],
            "claude_extension": {"nested": "preserved"}
        },
        "text": "sanitized"
    });
    let payload_reference = PayloadReferenceV1::for_payload(&payload).unwrap();
    let observation = durable(profile_material(), payload.clone());
    let canonical = observation.canonical_payload_bytes().unwrap();
    let encoded = serde_json::to_vec(&observation).unwrap();
    let decoded: DurableClaudeObservationV1 = serde_json::from_slice(&encoded).unwrap();

    assert_eq!(decoded.identity(), observation.identity());
    assert_eq!(decoded.receipt(), observation.receipt());
    assert_eq!(decoded.retention_class(), observation.retention_class());
    assert_eq!(decoded.payload(), &payload);
    assert_eq!(decoded.canonical_payload_bytes().unwrap(), canonical);
    assert_eq!(
        PayloadReferenceV1::for_payload(decoded.payload())
            .unwrap()
            .digest(),
        payload_reference.digest()
    );
    assert_eq!(
        decoded.payload()["provider_evidence"],
        payload["provider_evidence"]
    );
}

#[test]
fn collision_classification_distinguishes_duplicates_collisions_and_new_identity() {
    let material = profile_material();
    let first_payload: Value = serde_json::from_str(r#"{"b":2,"a":1}"#).unwrap();
    let reordered_payload: Value = serde_json::from_str(r#"{"a":1,"b":2}"#).unwrap();
    let existing = durable(material.clone(), first_payload);
    let exact_retry = durable(material.clone(), reordered_payload);
    let collision = durable(material, json!({"a": 1, "b": 3}));
    let distinct = durable(
        ClaudeObservationIdentityMaterialV1::new(
            source("session.fixture"),
            ObservationScopeV1::Profile,
            ClaudeFileGenerationV1::new(7).unwrap(),
            ClaudeByteRangeV1::new(34, 56).unwrap(),
        )
        .unwrap(),
        json!({"a": 1, "b": 2}),
    );

    assert_eq!(
        classify_observation_collision(&existing, &exact_retry),
        ObservationCollisionOutcomeV1::ExactDuplicate
    );
    assert_eq!(
        classify_observation_collision(&existing, &collision),
        ObservationCollisionOutcomeV1::IdentityCollision
    );
    assert_eq!(
        classify_observation_collision(&existing, &distinct),
        ObservationCollisionOutcomeV1::Distinct
    );
}
