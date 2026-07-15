use std::cmp::Ordering;
use std::fmt::Write as _;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
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
fn receipt_derivation_is_canonical_and_generation_bound() {
    let identity = profile_material();
    let payload = PayloadReferenceV1::for_payload(&json!({"message": "safe"})).unwrap();
    let material = CanonicalClaudeSanitizationReceiptMaterialV1::for_durable_payload(
        &identity,
        ComponentVersion::new("sanitizer.fixture.v1").unwrap(),
        SanitizerDispositionV1::Accepted,
        &[7; 32],
        &payload,
    )
    .unwrap();
    let receipt = material.derive_receipt_ref().unwrap();

    assert_eq!(
        receipt.receipt_id().as_str(),
        "privacy.claude.v1.2ef774a1d81493c05616a42ac8cf08856f230c7aa4f4e9d8224512d05ded88a8"
    );
    assert_eq!(receipt.sanitizer_version().as_str(), "sanitizer.fixture.v1");
    assert_eq!(SanitizerDispositionV1::Accepted.as_str(), "accepted");
    assert_eq!(SanitizerDispositionV1::Redacted.as_str(), "redacted");
    assert_eq!(SanitizerDispositionV1::Rejected.as_str(), "rejected");
    assert_eq!(SanitizerDispositionV1::Quarantined.as_str(), "quarantined");

    let changed_generation = ClaudeObservationIdentityMaterialV1::new(
        source("session.fixture"),
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(8).unwrap(),
        ClaudeByteRangeV1::new(12, 34).unwrap(),
    )
    .unwrap();
    let changed = CanonicalClaudeSanitizationReceiptMaterialV1::for_durable_payload(
        &changed_generation,
        ComponentVersion::new("sanitizer.fixture.v1").unwrap(),
        SanitizerDispositionV1::Accepted,
        &[7; 32],
        &payload,
    )
    .unwrap()
    .derive_receipt_ref()
    .unwrap();
    assert_ne!(receipt.receipt_id(), changed.receipt_id());

    let changed_sensitivity =
        CanonicalClaudeSanitizationReceiptMaterialV1::for_durable_payload_with_sensitivity(
            &identity,
            ComponentVersion::new("sanitizer.fixture.v1").unwrap(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::Sensitive,
            &[7; 32],
            &payload,
        )
        .unwrap()
        .derive_receipt_ref()
        .unwrap();
    assert_ne!(receipt.receipt_id(), changed_sensitivity.receipt_id());
}

#[test]
#[allow(deprecated)]
fn legacy_receipt_constructor_accepts_arbitrary_evidence_and_keeps_its_id() {
    let identity = profile_material();
    let version = ComponentVersion::new("sanitizer.legacy.v1").unwrap();
    let evidence = b"arbitrary legacy evidence, not a digest";
    let receipt = CanonicalClaudeSanitizationReceiptMaterialV1::new(
        &identity,
        version.clone(),
        SanitizerDispositionV1::Rejected,
        evidence,
    )
    .unwrap()
    .derive_receipt_ref()
    .unwrap();

    let observation_id = CanonicalObservationIdV1::derive(&identity).unwrap();
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.privacy.claude.receipt.v1\0");
    hasher.update(version.as_str().as_bytes());
    hasher.update(observation_id.as_str().as_bytes());
    hasher.update(b"rejected");
    hasher.update(evidence);
    let mut digest = String::with_capacity(64);
    for byte in hasher.finalize() {
        write!(&mut digest, "{byte:02x}").unwrap();
    }
    let expected = format!("privacy.claude.v1.{digest}");
    assert_eq!(receipt.receipt_id().as_str(), expected);
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
        generation,
        10,
    );
    let later = byte_cursor(
        "session.fixture",
        ObservationScopeV1::Profile,
        generation,
        20,
    );

    assert_eq!(first.checked_cmp(&later).unwrap(), Ordering::Less);
    assert!(
        first
            .checked_cmp(&byte_cursor(
                "session.other",
                ObservationScopeV1::Profile,
                generation,
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
                generation,
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
fn source_cursor_resume_checkpoints_round_trip_without_breaking_legacy_json() {
    let legacy = ClaudeSourceCursorV1::new(
        source("session.fixture"),
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(2).unwrap(),
        20,
    )
    .unwrap();
    let legacy_json = serde_json::to_value(&legacy).unwrap();
    assert!(legacy_json.get("file_identity").is_none());
    assert!(legacy_json.get("resume_fingerprint").is_none());
    let legacy_round_trip: ClaudeSourceCursorV1 = serde_json::from_value(legacy_json).unwrap();
    assert_eq!(legacy_round_trip.file_identity(), None);
    assert_eq!(legacy_round_trip.resume_fingerprint(), None);

    let checkpoint = legacy.with_resume_checkpoint(41, 73);
    let checkpoint_json = serde_json::to_value(&checkpoint).unwrap();
    assert_eq!(checkpoint_json["file_identity"], 41);
    assert_eq!(checkpoint_json["resume_fingerprint"], 73);
    let round_trip: ClaudeSourceCursorV1 = serde_json::from_value(checkpoint_json).unwrap();
    assert_eq!(round_trip, checkpoint);
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
