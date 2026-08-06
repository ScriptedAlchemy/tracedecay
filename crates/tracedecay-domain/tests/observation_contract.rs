use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CanonicalClaudeSanitizationReceiptMaterialV1, CanonicalMessageRoleV1,
    CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1, CanonicalObservationFactV1,
    CanonicalObservationIdV1, CanonicalObservationRelationsV1, CanonicalReasoningVisibilityV1,
    CanonicalWorkflowSemanticKindV1, ClaudeByteRangeV1, ClaudeFileGenerationV1,
    ClaudeObservationIdentityMaterialV1, ClaudeSourceCursorV1, ClaudeSourceIdentityV1,
    ComponentVersion, DurableClaudeObservationV1, IdempotencyKeyV1,
    MAX_CANONICAL_OBSERVATION_FACTS_V1, MAX_OBSERVATION_RECORD_BYTES,
    MAX_OBSERVATION_STRUCTURE_DEPTH, MAX_OBSERVATION_STRUCTURE_VALUES,
    ObservationCollisionOutcomeV1, ObservationContractError, ObservationId,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceIdentityV1, ObservationSourceRangeV1, PayloadReferenceV1, ProjectId,
    ProviderId, ProviderUsageContractDimensionV1, RetentionClass, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    SessionId, classify_observation_collision,
};

fn source(session_id: &str) -> ClaudeSourceIdentityV1 {
    ClaudeSourceIdentityV1::new(SessionId::new(session_id).unwrap()).unwrap()
}

fn provider_source(provider: &str, session_id: &str) -> ObservationSourceIdentityV1 {
    ObservationSourceIdentityV1::for_provider(
        ProviderId::new(provider).unwrap(),
        SessionId::new(session_id).unwrap(),
    )
    .unwrap()
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

fn envelope_with_content(
    content: Value,
) -> Result<CanonicalObservationEnvelopeV1, ObservationContractError> {
    CanonicalObservationEnvelopeV1::new(
        ProviderId::new("fixture-provider").unwrap(),
        "message",
        ObservationId::new("message.fixture").unwrap(),
        CanonicalObservationRelationsV1::new(SessionId::new("session.fixture").unwrap()),
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content,
            model: None,
            timestamp: None,
        }],
        CanonicalObservationEvidenceV1::new(
            ObservationOrderingDomainV1::FileBytes,
            ClaudeByteRangeV1::new(1, 2).unwrap(),
        ),
    )
}

fn json_structure_metrics(value: &Value) -> (usize, usize) {
    let mut values = 0usize;
    let mut max_depth = 0usize;
    let mut stack = vec![(value, 1usize)];
    while let Some((current, depth)) = stack.pop() {
        values += 1;
        max_depth = max_depth.max(depth);
        match current {
            Value::Array(items) => stack.extend(items.iter().map(|item| (item, depth + 1))),
            Value::Object(fields) => {
                stack.extend(fields.values().map(|item| (item, depth + 1)));
            }
            _ => {}
        }
    }
    (values, max_depth)
}

fn nested_arrays(depth: usize) -> Value {
    (0..depth).fold(Value::Null, |value, _| Value::Array(vec![value]))
}

#[test]
fn uncorrelated_usage_retains_native_evidence_and_requires_missing_dimensions() {
    let fact = |missing_dimensions| CanonicalObservationFactV1::UncorrelatedUsage {
        input_tokens: Some(11),
        output_tokens: Some(7),
        cache_read_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
        total_tokens: Some(18),
        native_kind: "_usage".to_owned(),
        native_field: "usage".to_owned(),
        missing_dimensions,
    };
    let valid = fact(BTreeSet::from([
        ProviderUsageContractDimensionV1::Model,
        ProviderUsageContractDimensionV1::Scope,
    ]));
    let wire = serde_json::to_value(&valid).unwrap();
    assert_eq!(wire["total_tokens"], 18);
    assert_eq!(wire["native_kind"], "_usage");
    assert_eq!(wire["native_field"], "usage");
    assert_eq!(wire["missing_dimensions"], json!(["model", "scope"]));

    let error = CanonicalObservationEnvelopeV1::new(
        ProviderId::new("fixture-provider").unwrap(),
        "usage",
        ObservationId::new("usage.fixture").unwrap(),
        CanonicalObservationRelationsV1::new(SessionId::new("session.fixture").unwrap()),
        vec![fact(BTreeSet::new())],
        CanonicalObservationEvidenceV1::new(
            ObservationOrderingDomainV1::SnapshotOrder,
            ObservationSourceRangeV1::new(1, 2).unwrap(),
        ),
    )
    .unwrap_err();
    assert_eq!(error, ObservationContractError::InvalidCanonicalPayload);
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
fn claude_identity_wire_and_hash_remain_v1_compatible() {
    let material = profile_material();
    let wire = serde_json::to_value(&material).unwrap();

    assert!(wire.get("ordering_domain").is_none());
    assert!(wire.get("native_record_id").is_none());
    assert_eq!(
        CanonicalObservationIdV1::derive(&material)
            .unwrap()
            .as_str(),
        "sha256:92fe6f78f68eb34153f865b770a7fed01b01425730796ac67bbc4973aad527a3"
    );
}

#[test]
fn native_record_identity_is_independent_of_generation_and_ordering_position() {
    let source = provider_source("hermes", "session.fixture");
    let native_record_id = ObservationId::new("message.fixture").unwrap();
    let identity = |generation, start, end| {
        ClaudeObservationIdentityMaterialV1::for_native_record(
            source.clone(),
            ObservationScopeV1::Profile,
            ClaudeFileGenerationV1::new(generation).unwrap(),
            ClaudeByteRangeV1::new(start, end).unwrap(),
            ObservationOrderingDomainV1::SqliteRowId,
            native_record_id.clone(),
        )
        .unwrap()
    };

    let first = identity(1, 10, 11);
    let relocated = identity(2, 40, 41);
    assert_eq!(
        CanonicalObservationIdV1::derive(&first).unwrap(),
        CanonicalObservationIdV1::derive(&relocated).unwrap()
    );

    let wire = serde_json::to_value(&first).unwrap();
    assert_eq!(wire["ordering_domain"], "sqlite_row_id");
    assert_eq!(wire["native_record_id"], "message.fixture");

    let payload = PayloadReferenceV1::for_payload(&json!({"message": "safe"})).unwrap();
    let receipt = CanonicalClaudeSanitizationReceiptMaterialV1::for_durable_payload(
        &first,
        ComponentVersion::new("privacy.observation-record.v1").unwrap(),
        SanitizerDispositionV1::Accepted,
        &[9; 32],
        &payload,
    )
    .unwrap()
    .derive_receipt_ref()
    .unwrap();
    assert!(
        receipt
            .receipt_id()
            .as_str()
            .starts_with("privacy.observation.v1.")
    );
}

#[test]
fn claude_native_identity_survives_transcript_relocation() {
    let native_record_id = ObservationId::new("message.fixture").unwrap();
    let identity = |source_key: &str, generation, start, end| {
        ClaudeObservationIdentityMaterialV1::for_native_record(
            ObservationSourceIdentityV1::for_source(
                SessionId::new("session.fixture").unwrap(),
                SessionId::new(source_key).unwrap(),
            )
            .unwrap(),
            ObservationScopeV1::Profile,
            ClaudeFileGenerationV1::new(generation).unwrap(),
            ClaudeByteRangeV1::new(start, end).unwrap(),
            ObservationOrderingDomainV1::FileBytes,
            native_record_id.clone(),
        )
        .unwrap()
    };

    let original = identity("source.original", 1, 10, 11);
    let relocated = identity("source.relocated", 2, 40, 41);
    assert_eq!(
        CanonicalObservationIdV1::derive(&original).unwrap(),
        CanonicalObservationIdV1::derive(&relocated).unwrap()
    );

    let other_session = ClaudeObservationIdentityMaterialV1::for_native_record(
        ObservationSourceIdentityV1::for_source(
            SessionId::new("session.other").unwrap(),
            SessionId::new("source.relocated").unwrap(),
        )
        .unwrap(),
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(2).unwrap(),
        ClaudeByteRangeV1::new(40, 41).unwrap(),
        ObservationOrderingDomainV1::FileBytes,
        native_record_id,
    )
    .unwrap();
    assert_ne!(
        CanonicalObservationIdV1::derive(&original).unwrap(),
        CanonicalObservationIdV1::derive(&other_session).unwrap()
    );

    let other_record = ClaudeObservationIdentityMaterialV1::for_native_record(
        original.source().clone(),
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(2).unwrap(),
        ClaudeByteRangeV1::new(40, 41).unwrap(),
        ObservationOrderingDomainV1::FileBytes,
        ObservationId::new("message.other").unwrap(),
    )
    .unwrap();
    assert_ne!(
        CanonicalObservationIdV1::derive(&original).unwrap(),
        CanonicalObservationIdV1::derive(&other_record).unwrap()
    );
}

#[test]
fn canonical_envelope_preserves_typed_facts_without_inventing_relations() {
    let range = ClaudeByteRangeV1::new(4, 5).unwrap();
    let envelope = CanonicalObservationEnvelopeV1::new(
        ProviderId::new("hermes").unwrap(),
        "message",
        ObservationId::new("message.fixture").unwrap(),
        CanonicalObservationRelationsV1::new(SessionId::new("session.fixture").unwrap())
            .with_message_id(ObservationId::new("message.fixture").unwrap()),
        vec![
            CanonicalObservationFactV1::Message {
                role: CanonicalMessageRoleV1::Assistant,
                content: json!({"text": "safe"}),
                model: Some("model.fixture".to_owned()),
                timestamp: Some(42),
            },
            CanonicalObservationFactV1::Reasoning {
                visibility: CanonicalReasoningVisibilityV1::Unavailable,
                content: None,
            },
        ],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SqliteRowId, range)
            .with_native_sequence(5),
    )
    .unwrap();

    envelope.validate().unwrap();
    assert_eq!(envelope.provider().as_str(), "hermes");
    assert_eq!(envelope.evidence().range(), range);
    assert_eq!(
        envelope.relations().session_id().as_str(),
        "session.fixture"
    );
    assert_eq!(envelope.facts().len(), 2);
    assert!(
        serde_json::to_value(&envelope).unwrap()["relations"]
            .get("thread_id")
            .is_none()
    );
}

#[test]
fn canonical_session_fact_keeps_project_identity_separate_from_native_location() {
    let envelope = CanonicalObservationEnvelopeV1::new(
        ProviderId::new("hermes").unwrap(),
        "message",
        ObservationId::new("message.session-location").unwrap(),
        CanonicalObservationRelationsV1::new(SessionId::new("session.location").unwrap())
            .with_message_id(ObservationId::new("message.session-location").unwrap()),
        vec![
            CanonicalObservationFactV1::Session {
                project_path: Some("/workspace/project".to_owned()),
                location_path: Some("/workspace/project/.worktrees/feature".to_owned()),
                transcript_path: Some("/transcripts/session.jsonl".to_owned()),
                title: None,
                started_at: Some(10),
                ended_at: Some(20),
                source: Some("provider_store".to_owned()),
                native_source: None,
                profile: None,
                location_provenance: Some("profile_pin".to_owned()),
            },
            CanonicalObservationFactV1::Message {
                role: CanonicalMessageRoleV1::Assistant,
                content: json!({"text": "safe"}),
                model: None,
                timestamp: Some(20),
            },
        ],
        CanonicalObservationEvidenceV1::new(
            ObservationOrderingDomainV1::SqliteRowId,
            ClaudeByteRangeV1::new(1, 2).unwrap(),
        ),
    )
    .unwrap();

    let wire = serde_json::to_value(&envelope).unwrap();
    assert_eq!(wire["facts"][0]["project_path"], "/workspace/project");
    assert_eq!(
        wire["facts"][0]["location_path"],
        "/workspace/project/.worktrees/feature"
    );
    assert_eq!(
        wire["facts"][0]["transcript_path"],
        "/transcripts/session.jsonl"
    );
    let decoded: CanonicalObservationEnvelopeV1 = serde_json::from_value(wire).unwrap();
    decoded.validate().unwrap();
    assert_eq!(decoded, envelope);
}

#[test]
fn canonical_envelope_accepts_byte_depth_and_value_boundaries() {
    let empty = envelope_with_content(Value::String(String::new())).unwrap();
    let empty_bytes = serde_json::to_vec(&empty).unwrap().len();
    let byte_boundary = envelope_with_content(Value::String(
        "x".repeat(MAX_OBSERVATION_RECORD_BYTES - empty_bytes),
    ))
    .unwrap();
    assert_eq!(
        serde_json::to_vec(&byte_boundary).unwrap().len(),
        MAX_OBSERVATION_RECORD_BYTES
    );

    let depth_boundary =
        envelope_with_content(nested_arrays(MAX_OBSERVATION_STRUCTURE_DEPTH - 4)).unwrap();
    assert_eq!(
        json_structure_metrics(&serde_json::to_value(depth_boundary).unwrap()).1,
        MAX_OBSERVATION_STRUCTURE_DEPTH
    );

    let base = envelope_with_content(Value::Null).unwrap();
    let base_values = json_structure_metrics(&serde_json::to_value(base).unwrap()).0;
    let value_boundary = envelope_with_content(Value::Array(vec![
        Value::Null;
        MAX_OBSERVATION_STRUCTURE_VALUES
            - base_values
    ]))
    .unwrap();
    assert_eq!(
        json_structure_metrics(&serde_json::to_value(value_boundary).unwrap()).0,
        MAX_OBSERVATION_STRUCTURE_VALUES
    );
}

#[test]
fn canonical_envelope_rejects_every_limit_overflow() {
    let empty = envelope_with_content(Value::String(String::new())).unwrap();
    let empty_bytes = serde_json::to_vec(&empty).unwrap().len();
    let byte_error = envelope_with_content(Value::String(
        "x".repeat(MAX_OBSERVATION_RECORD_BYTES - empty_bytes + 1),
    ))
    .unwrap_err();
    assert_eq!(
        byte_error,
        ObservationContractError::CanonicalEnvelopeTooLarge
    );

    let depth_error =
        envelope_with_content(nested_arrays(MAX_OBSERVATION_STRUCTURE_DEPTH - 3)).unwrap_err();
    assert_eq!(
        depth_error,
        ObservationContractError::CanonicalEnvelopeTooDeep
    );

    let base = envelope_with_content(Value::Null).unwrap();
    let base_values = json_structure_metrics(&serde_json::to_value(base).unwrap()).0;
    let values_error = envelope_with_content(Value::Array(vec![
        Value::Null;
        MAX_OBSERVATION_STRUCTURE_VALUES
            - base_values
            + 1
    ]))
    .unwrap_err();
    assert_eq!(
        values_error,
        ObservationContractError::CanonicalEnvelopeTooManyValues
    );

    let fact = CanonicalObservationFactV1::UncorrelatedUsage {
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
        total_tokens: None,
        native_kind: "fixture_usage".to_string(),
        native_field: "fixture.usage".to_string(),
        missing_dimensions: std::collections::BTreeSet::from([
            tracedecay_domain::ProviderUsageContractDimensionV1::Model,
        ]),
    };
    let facts_error = CanonicalObservationEnvelopeV1::new(
        ProviderId::new("fixture-provider").unwrap(),
        "usage",
        ObservationId::new("usage.fixture").unwrap(),
        CanonicalObservationRelationsV1::new(SessionId::new("session.fixture").unwrap()),
        vec![fact; MAX_CANONICAL_OBSERVATION_FACTS_V1 + 1],
        CanonicalObservationEvidenceV1::new(
            ObservationOrderingDomainV1::FileBytes,
            ClaudeByteRangeV1::new(1, 2).unwrap(),
        ),
    )
    .unwrap_err();
    assert_eq!(facts_error, ObservationContractError::CanonicalFactsTooMany);
}

#[test]
fn workflow_lifecycle_facts_preserve_native_optional_evidence_and_legacy_wire() {
    let range = ClaudeByteRangeV1::new(8, 9).unwrap();
    let envelope = CanonicalObservationEnvelopeV1::new(
        ProviderId::new("fixture-provider").unwrap(),
        "workflow_event",
        ObservationId::new("workflow.fixture").unwrap(),
        CanonicalObservationRelationsV1::new(SessionId::new("session.workflow-fixture").unwrap()),
        vec![
            CanonicalObservationFactV1::WorkflowLifecycle {
                semantic_kind: CanonicalWorkflowSemanticKindV1::TodoList,
                provider_reference: Some("native-list.7".to_owned()),
                item_id: None,
                parent_reference: Some("native-plan.3".to_owned()),
                list_reference: None,
                state: Some("active".to_owned()),
                status: None,
                item_order: None,
                revision: Some("rev-a".to_owned()),
                event_sequence: Some(41),
                content: Some(json!({"title": "release checklist"})),
            },
            CanonicalObservationFactV1::WorkflowLifecycle {
                semantic_kind: CanonicalWorkflowSemanticKindV1::TodoItem,
                provider_reference: Some("native-item.9".to_owned()),
                item_id: Some("stable-item.9".to_owned()),
                parent_reference: None,
                list_reference: Some("native-list.7".to_owned()),
                state: None,
                status: Some("in_progress".to_owned()),
                item_order: Some(2),
                revision: None,
                event_sequence: None,
                content: Some(json!({"text": "publish artifacts"})),
            },
        ],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::DaemonSequence, range)
            .with_native_sequence(52),
    )
    .unwrap();

    let wire = serde_json::to_value(&envelope).unwrap();
    assert_eq!(wire["facts"][0]["semantic_kind"], "todo_list");
    assert_eq!(wire["facts"][1]["item_order"], 2);
    assert!(wire["facts"][0].get("item_id").is_none());
    assert!(wire["facts"][0].get("status").is_none());
    assert!(wire["facts"][1].get("state").is_none());
    assert!(wire["facts"][1].get("revision").is_none());
    let decoded: CanonicalObservationEnvelopeV1 = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(decoded, envelope);

    let legacy = json!({
        "version": 1,
        "provider": "fixture-provider",
        "native_record_kind": "workflow",
        "stable_record_id": "legacy.workflow.1",
        "relations": {"session_id": "session.workflow-fixture"},
        "facts": [{
            "kind": "workflow",
            "evidence_kind": "task",
            "reference": "legacy.task.1",
            "content": {"text": "legacy task"}
        }],
        "evidence": {
            "ordering_domain": "daemon_sequence",
            "range": {"start": 1, "end": 2}
        }
    });
    let legacy_decoded: CanonicalObservationEnvelopeV1 =
        serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(serde_json::to_value(legacy_decoded).unwrap(), legacy);
}

#[test]
fn canonical_envelope_rejects_visible_reasoning_without_content() {
    let range = ClaudeByteRangeV1::new(1, 2).unwrap();
    let error = CanonicalObservationEnvelopeV1::new(
        ProviderId::new("codex").unwrap(),
        "reasoning",
        ObservationId::new("reasoning.fixture").unwrap(),
        CanonicalObservationRelationsV1::new(SessionId::new("session.fixture").unwrap()),
        vec![CanonicalObservationFactV1::Reasoning {
            visibility: CanonicalReasoningVisibilityV1::Visible,
            content: None,
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::FileBytes, range),
    )
    .unwrap_err();

    assert_eq!(error, ObservationContractError::InvalidReasoningVisibility);
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
    let row_cursor = ObservationSourceCursorV1::for_ordering(
        source("session.fixture"),
        ObservationScopeV1::Profile,
        generation,
        ObservationOrderingDomainV1::SqliteRowId,
        20,
    )
    .unwrap();
    assert_eq!(
        first.checked_cmp(&row_cursor),
        Err(ObservationContractError::CursorOrderingDomainMismatch)
    );
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

#[test]
fn workflow_lifecycle_payload_dedupe_and_conflict_remain_deterministic() {
    let material = profile_material();
    let payload = |content: Value, status: &str| {
        serde_json::to_value(
            CanonicalObservationEnvelopeV1::new(
                ProviderId::new("claude").unwrap(),
                "workflow",
                ObservationId::new("workflow.lifecycle.1").unwrap(),
                CanonicalObservationRelationsV1::new(SessionId::new("session.fixture").unwrap()),
                vec![CanonicalObservationFactV1::WorkflowLifecycle {
                    semantic_kind: CanonicalWorkflowSemanticKindV1::Task,
                    provider_reference: Some("task.native.1".to_owned()),
                    item_id: Some("task.stable.1".to_owned()),
                    parent_reference: None,
                    list_reference: None,
                    state: None,
                    status: Some(status.to_owned()),
                    item_order: None,
                    revision: Some("1".to_owned()),
                    event_sequence: Some(7),
                    content: Some(content),
                }],
                CanonicalObservationEvidenceV1::new(
                    ObservationOrderingDomainV1::FileBytes,
                    ClaudeByteRangeV1::new(12, 34).unwrap(),
                ),
            )
            .unwrap(),
        )
        .unwrap()
    };
    let first = durable(
        material.clone(),
        payload(
            json!({"text": "ship", "details": {"a": 1, "b": 2}}),
            "pending",
        ),
    );
    let reordered = durable(
        material.clone(),
        payload(
            json!({"details": {"b": 2, "a": 1}, "text": "ship"}),
            "pending",
        ),
    );
    let conflicting = durable(
        material,
        payload(
            json!({"details": {"a": 1, "b": 2}, "text": "ship"}),
            "completed",
        ),
    );

    assert_eq!(
        classify_observation_collision(&first, &reordered),
        ObservationCollisionOutcomeV1::ExactDuplicate
    );
    assert_eq!(
        classify_observation_collision(&first, &conflicting),
        ObservationCollisionOutcomeV1::IdentityCollision
    );
}

/// Rows committed before native record ids joined default-provider derivation
/// must still decode. Changing a derivation without accepting the previous one
/// makes every such row permanently undecodable, and nothing downstream can
/// quarantine an undecodable observation.
#[test]
fn durable_observations_written_before_native_identity_still_decode() {
    let material = ClaudeObservationIdentityMaterialV1::for_native_record(
        ObservationSourceIdentityV1::for_source(
            SessionId::new("session.fixture").unwrap(),
            SessionId::new("source.fixture").unwrap(),
        )
        .unwrap(),
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(3).unwrap(),
        ClaudeByteRangeV1::new(10, 11).unwrap(),
        ObservationOrderingDomainV1::FileBytes,
        ObservationId::new("message.fixture").unwrap(),
    )
    .unwrap();

    let payload = json!({"text": "sanitized"});
    let observation = durable(material.clone(), payload.clone());

    // The derivation this row was written with: the whole material under the
    // Claude domain, with no separate native-record-id structure.
    let legacy_observation_id = domain_digest_id(b"tracedecay.claude.observation.v1\0", &material);

    assert_ne!(
        legacy_observation_id,
        observation.observation_id().as_str(),
        "fixture must exercise the derivation change, not agree with it"
    );

    // A real pre-change row carries the same digest in both fields, because the
    // writer serialized one value under two names. Rewriting only
    // `observation_id` here is what let the first fix look complete while the
    // daemon still failed on `idempotency_key`.
    let mut wire: Value = serde_json::from_slice(&serde_json::to_vec(&observation).unwrap())
        .expect("durable observation serializes to an object");
    wire["observation_id"] = json!(legacy_observation_id);
    wire["idempotency_key"] = json!(legacy_observation_id);

    let decoded: DurableClaudeObservationV1 =
        serde_json::from_value(wire.clone()).expect("a pre-change row must still decode");
    assert_eq!(decoded.identity(), observation.identity());
    assert_eq!(decoded.payload(), &payload);

    // Accepting the previous derivation must not accept an arbitrary id.
    let arbitrary = json!(format!("sha256:{}", "0".repeat(64)));
    for field in ["observation_id", "idempotency_key"] {
        let mut forged = wire.clone();
        forged[field] = arbitrary.clone();
        assert!(
            serde_json::from_value::<DurableClaudeObservationV1>(forged).is_err(),
            "an id matching no derivation must still be rejected in {field}"
        );
    }
}

/// Rows committed before `idempotency_key` became an alias of `observation_id`
/// carry their own domain-separated digest in that field, and rows committed
/// between that change and the native-identity change carry the whole-material
/// digest. Both predate the current derivation and both must still decode.
#[test]
fn durable_observations_decode_under_every_historical_derivation() {
    let material = native_identity_fixture();
    let observation = durable(material.clone(), json!({"text": "sanitized"}));
    let wire: Value = serde_json::from_slice(&serde_json::to_vec(&observation).unwrap())
        .expect("durable observation serializes to an object");

    let historical = [
        observation.observation_id().as_str().to_string(),
        domain_digest_id(b"tracedecay.claude.observation.v1\0", &material),
        domain_digest_id(b"tracedecay.claude.idempotency.v1\0", &material),
    ];
    assert_eq!(
        historical.iter().collect::<BTreeSet<_>>().len(),
        historical.len(),
        "each historical derivation must produce a distinct digest for this fixture"
    );

    for id in &historical {
        let mut row = wire.clone();
        row["observation_id"] = json!(id);
        row["idempotency_key"] = json!(id);
        let decoded: DurableClaudeObservationV1 = serde_json::from_value(row)
            .unwrap_or_else(|error| panic!("a row derived as {id} must decode: {error}"));
        assert_eq!(decoded.identity(), observation.identity());
    }
}

/// A change to the live derivation must not be able to land quietly.
///
/// Adding a derivation is legitimate; silently dropping the previous one from
/// the accepted set is what took the daemon down twice, once per field. This
/// pins the digest today's derivation produces for a fixed fixture, so any
/// change to it fails here rather than in warm-up against a live profile. The
/// fix when it fails is to add the new derivation to the front of the accepted
/// list, keep this value in that list, and pin the new one here.
#[test]
fn the_live_observation_derivation_is_pinned() {
    let observation = durable(native_identity_fixture(), json!({"text": "sanitized"}));
    assert_eq!(
        observation.observation_id().as_str(),
        "sha256:efd99c7fd87f4ad156b40f16d982d18511ebfb708afc140f9f67e63e0c73f5ba",
        "the live derivation changed; extend the accepted set before repinning"
    );
}

fn native_identity_fixture() -> ClaudeObservationIdentityMaterialV1 {
    ClaudeObservationIdentityMaterialV1::for_native_record(
        ObservationSourceIdentityV1::for_source(
            SessionId::new("session.fixture").unwrap(),
            SessionId::new("source.fixture").unwrap(),
        )
        .unwrap(),
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(3).unwrap(),
        ClaudeByteRangeV1::new(10, 11).unwrap(),
        ObservationOrderingDomainV1::FileBytes,
        ObservationId::new("message.fixture").unwrap(),
    )
    .unwrap()
}

/// A decoded row must report the identity it is stored under.
///
/// The storage audit compares `observations.observation_id` against the id on
/// the decoded observation, and rows that join to an observation carry that
/// same column value. Re-deriving the current form on decode makes a legacy row
/// disagree with its own column, which is the
/// "committed observation authority columns disagree with observation JSON"
/// violation. Accepting a legacy digest is only correct if the object then
/// carries it.
#[test]
fn decoded_observations_report_the_identity_they_are_stored_under() {
    let material = native_identity_fixture();
    let observation = durable(material.clone(), json!({"text": "sanitized"}));
    let wire: Value = serde_json::from_slice(&serde_json::to_vec(&observation).unwrap())
        .expect("durable observation serializes to an object");

    for stored_id in [
        observation.observation_id().as_str().to_string(),
        domain_digest_id(b"tracedecay.claude.observation.v1\0", &material),
        domain_digest_id(b"tracedecay.claude.idempotency.v1\0", &material),
    ] {
        let mut row = wire.clone();
        row["observation_id"] = json!(stored_id);
        row["idempotency_key"] = json!(stored_id);
        let decoded: DurableClaudeObservationV1 =
            serde_json::from_value(row).expect("a row under any accepted derivation must decode");

        assert_eq!(
            decoded.observation_id().as_str(),
            stored_id,
            "the decoded id must equal the stored column, or the audit rejects the row"
        );
        assert_eq!(
            decoded.idempotency_key().as_str(),
            stored_id,
            "the aliased key must follow the stored id"
        );

        // Re-encoding must not restate the row's identity.
        let reencoded: Value = serde_json::from_slice(&serde_json::to_vec(&decoded).unwrap())
            .expect("a decoded observation re-serializes");
        assert_eq!(reencoded["observation_id"], json!(stored_id));
        assert_eq!(reencoded["idempotency_key"], json!(stored_id));
    }
}

fn domain_digest_id(domain: &[u8], material: &ClaudeObservationIdentityMaterialV1) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(tracedecay_domain::canonical_json_bytes(material).unwrap());
    let mut digest = String::with_capacity(64);
    for byte in hasher.finalize() {
        write!(&mut digest, "{byte:02x}").unwrap();
    }
    format!("sha256:{digest}")
}
